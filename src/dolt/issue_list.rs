use mysql::Value as SqlValue;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{IssueListArgs, ProjectId};
use crate::error::{ErrorCode, HiveError};

use super::issue_readiness::Readiness;
use super::records::{IssueStatus, is_canonical_label_name, is_issue_type};
use super::{DoltFailure, DoltSuccess, Snapshot, is_uuid_v7};

const MAX_LIMIT: u64 = 100;
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IssueView {
    List,
    Ready,
    Blocked,
}

const SORT: &str = "i.priority IS NULL ASC, i.priority ASC, i.created_at ASC, i.id ASC";

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
enum Scope<'a> {
    All,
    Area(&'a str),
    Project(&'a str),
}

#[derive(Serialize)]
struct Query<'a> {
    scope: Scope<'a>,
    status: Option<&'a str>,
    kind: Option<&'a str>,
    priority: Option<u8>,
    label: Option<&'a str>,
    limit: u64,
}

pub(super) type SummaryRow = (
    String,
    u64,
    String,
    String,
    String,
    String,
    Option<i8>,
    String,
    String,
);

pub(crate) fn list(
    args: &IssueListArgs,
    project: Option<&ProjectId>,
    view: IssueView,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let query = Query::parse(args, project).map_err(DoltFailure::without_context)?;
    let (mut snapshot, workspace_id) =
        Snapshot::open(&args.target).map_err(DoltFailure::without_context)?;
    let context = json!({
        "workspace_id": workspace_id,
        "db_commit": snapshot.commit,
        "scope": query.scope,
    });
    let binding = if view == IssueView::List {
        json!(["issue-list-v1", workspace_id, snapshot.commit, SORT, query])
    } else {
        json!([
            "issue-readiness-v1",
            view,
            workspace_id,
            snapshot.commit,
            SORT,
            query
        ])
    };
    match list_page(
        &mut snapshot,
        &query,
        view,
        &binding,
        args.cursor.as_deref(),
    ) {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

impl<'a> Query<'a> {
    fn parse(args: &'a IssueListArgs, project: Option<&'a ProjectId>) -> Result<Self, HiveError> {
        let invalid = |message| HiveError::new(ErrorCode::InvalidArgument, message);
        let scope = match (
            args.all,
            args.area.as_deref(),
            project.map(ProjectId::as_str),
        ) {
            (true, None, None) => Scope::All,
            (false, Some(id), None) if is_uuid_v7(id) => Scope::Area(id),
            (false, None, Some(id)) if is_uuid_v7(id) => Scope::Project(id),
            (false, Some(_), None) | (false, None, Some(_)) => {
                return Err(invalid(
                    "Use a canonical lowercase UUIDv7 for the selected scope.",
                ));
            }
            (false, None, None) => {
                return Err(invalid(
                    "Select one scope with --all, --area, or --project.",
                ));
            }
            _ => {
                return Err(HiveError::new(
                    ErrorCode::AmbiguousScope,
                    "Select only one of --all, --area, or --project.",
                ));
            }
        };

        let status = args.status.as_deref();
        if status.is_some_and(|value| IssueStatus::parse(value).is_none()) {
            return Err(invalid("Use a canonical Issue status."));
        }
        let kind = args.kind.as_deref();
        if kind.is_some_and(|value| !is_issue_type(value)) {
            return Err(invalid("Use a canonical Issue type."));
        }
        let priority = match args.priority.as_deref() {
            None => None,
            Some(value @ ("0" | "1" | "2" | "3")) => Some(value.as_bytes()[0] - b'0'),
            Some(_) => return Err(invalid("Use an Issue priority between 0 and 3.")),
        };
        let label = args.label.as_deref();
        if label.is_some_and(|value| !is_canonical_label_name(value)) {
            return Err(invalid("Use an exact canonical Label name."));
        }
        let limit = args
            .limit
            .parse::<u64>()
            .ok()
            .filter(|limit| (1..=MAX_LIMIT).contains(limit))
            .ok_or_else(|| invalid("Use a page size between 1 and 100."))?;
        Ok(Self {
            scope,
            status,
            kind,
            priority,
            label,
            limit,
        })
    }
}

fn list_page(
    snapshot: &mut Snapshot,
    query: &Query<'_>,
    view: IssueView,
    binding: &Value,
    cursor: Option<&str>,
) -> Result<Value, HiveError> {
    let offset = match cursor {
        Some(cursor) => cursor_offset(cursor, binding, view)?,
        None => 0,
    };
    verify_scope(snapshot, query.scope)?;

    let mut items = Vec::new();
    let mut next_offset = None;
    if view == IssueView::List {
        let rows = summary_rows(snapshot, query, view, query.limit + 1, offset)?;
        validate_summaries(&rows)?;
        if rows.len() as u64 > query.limit {
            next_offset = Some(offset + query.limit);
        }
        items.extend(rows.into_iter().take(query.limit as usize).map(summary));
    } else {
        let mut readiness = Readiness::default();
        let mut position = offset;
        let scan_limit = super::MAX_ROWS_PER_TABLE as u64;
        'pages: loop {
            let remaining = scan_limit - (position - offset);
            // Read one extra candidate at the scan boundary to distinguish exhaustion.
            let batch_size = MAX_LIMIT.min(remaining.max(1));
            let rows = summary_rows(snapshot, query, view, batch_size, position)?;
            if rows.len() as u64 > remaining {
                return Err(HiveError::new(
                    ErrorCode::ContextLimit,
                    "The readiness query exceeds the candidate scan limit.",
                ));
            }
            validate_summaries(&rows)?;
            if rows.is_empty() {
                break;
            }
            let mut reasons = readiness.reasons(snapshot, &rows)?;
            let exhausted = (rows.len() as u64) < batch_size;
            for row in rows {
                let blockers = reasons.remove(&row.0).ok_or_else(|| {
                    HiveError::new(
                        ErrorCode::InvalidRecord,
                        "Committed Issue metadata is missing.",
                    )
                })?;
                if blockers.is_empty() == (view == IssueView::Ready) {
                    if items.len() as u64 == query.limit {
                        next_offset = Some(position);
                        break 'pages;
                    }
                    let mut item = summary(row);
                    if view == IssueView::Blocked {
                        item["reasons"] = json!(blockers);
                    }
                    items.push(item);
                }
                position += 1;
            }
            if exhausted {
                break;
            }
        }
    }
    Ok(json!({
        "items": items,
        "next_cursor": next_offset.map(|position| encode_cursor(position, binding)),
        "truncated": next_offset.is_some(),
        "observed_revision": snapshot.commit,
    }))
}

fn summary_rows(
    snapshot: &mut Snapshot,
    query: &Query<'_>,
    view: IssueView,
    limit: u64,
    offset: u64,
) -> Result<Vec<SummaryRow>, HiveError> {
    let (scope_predicate, scope_id) = match query.scope {
        Scope::All => ("", None),
        Scope::Area(id) => ("CAST(i.area_id AS BINARY) = ?", Some(id)),
        Scope::Project(id) => ("CAST(p.project_id AS BINARY) = ?", Some(id)),
    };
    let mut predicates = Vec::new();
    let mut params = Vec::new();
    for (predicate, value) in [
        (scope_predicate, scope_id.map(SqlValue::from)),
        (
            "CAST(l.name AS BINARY) = ?",
            query.label.map(SqlValue::from),
        ),
        (
            "CAST(i.status AS BINARY) = ?",
            query.status.map(SqlValue::from),
        ),
        ("CAST(i.type AS BINARY) = ?", query.kind.map(SqlValue::from)),
        ("i.priority = ?", query.priority.map(SqlValue::from)),
    ] {
        if let Some(value) = value {
            predicates.push(predicate);
            params.push(value);
        }
    }
    if view != IssueView::List {
        predicates.push("CAST(i.status AS BINARY) = 'todo' AND CAST(i.type AS BINARY) IN ('feature', 'task', 'bug')");
    }
    params.extend([SqlValue::from(limit), SqlValue::from(offset)]);
    snapshot.exec_rows(
        |commit| {
            // Only static SQL fragments and the verified commit are interpolated.
            let mut sql = format!(
                "SELECT i.id, i.number, i.area_id, i.title, i.type, i.status, i.priority, i.revision, \
                 DATE_FORMAT(i.created_at, '%Y-%m-%dT%H:%i:%s.%fZ') FROM issues AS OF '{commit}' AS i"
            );
            if matches!(query.scope, Scope::Project(_)) {
                sql.push_str(&format!(
                    " JOIN project_issues AS OF '{commit}' AS p ON \
                      CAST(p.issue_id AS BINARY) = CAST(i.id AS BINARY)"
                ));
            }
            if query.label.is_some() {
                sql.push_str(&format!(
                    " JOIN issue_labels AS OF '{commit}' AS il ON \
                      CAST(il.issue_id AS BINARY) = CAST(i.id AS BINARY) \
                      JOIN labels AS OF '{commit}' AS l ON \
                      CAST(l.id AS BINARY) = CAST(il.label_id AS BINARY)"
                ));
            }
            if !predicates.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&predicates.join(" AND "));
            }
            sql.push_str(&format!(" ORDER BY {SORT} LIMIT ? OFFSET ?"));
            sql
        },
        params,
    )
}

fn validate_summaries(rows: &[SummaryRow]) -> Result<(), HiveError> {
    for (id, number, area_id, _, kind, status, priority, _, _) in rows {
        if !is_uuid_v7(id)
            || *number == 0
            || !is_uuid_v7(area_id)
            || !is_issue_type(kind)
            || IssueStatus::parse(status).is_none()
            || priority.is_some_and(|value| !(0..=3).contains(&value))
        {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "A selected Issue has invalid summary fields at the verified Dolt commit.",
            ));
        }
    }
    Ok(())
}

fn summary(
    (id, number, area_id, title, kind, status, priority, revision, created_at): SummaryRow,
) -> Value {
    json!({
        "id": id, "number": number, "area_id": area_id, "title": title, "type": kind,
        "status": status, "priority": priority, "revision": revision, "created_at": created_at,
    })
}

fn verify_scope(snapshot: &mut Snapshot, scope: Scope<'_>) -> Result<(), HiveError> {
    let (table, id) = match scope {
        Scope::All => return Ok(()),
        Scope::Area(id) => ("areas", id),
        Scope::Project(id) => ("projects", id),
    };
    let rows: Vec<String> = snapshot.exec_rows(
        |commit| format!("SELECT id FROM {table} AS OF '{commit}' WHERE id = ? LIMIT 1"),
        (id,),
    )?;
    if rows.first().is_some_and(|actual| actual == id) {
        Ok(())
    } else {
        Err(HiveError::new(
            ErrorCode::NotFound,
            "The selected scope was not found at the verified Dolt commit.",
        ))
    }
}

// A stable row offset is sufficient because a cursor is usable only at its
// original immutable snapshot and with the identical ordered query.
fn encode_cursor(offset: u64, binding: &Value) -> String {
    let digest = Sha256::digest(json!([binding, offset]).to_string());
    format!("{offset}.{digest:x}")
}

fn cursor_offset(cursor: &str, binding: &Value, view: IssueView) -> Result<u64, HiveError> {
    let max_advance = if view == IssueView::List {
        MAX_LIMIT
    } else {
        super::MAX_ROWS_PER_TABLE as u64
    };
    if cursor.len() <= 85
        && let Some((offset, _)) = cursor.split_once('.')
        && let Ok(offset) = offset.parse::<u64>()
        && offset > 0
        && offset <= u64::MAX - max_advance
        && cursor == encode_cursor(offset, binding)
    {
        return Ok(offset);
    }
    Err(HiveError::new(
        ErrorCode::StaleCursor,
        "The Issue list cursor is invalid for the selected workspace, commit, scope, or query.",
    ))
}

use mysql::Value as SqlValue;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{LabelListArgs, ProjectId};
use crate::error::{ErrorCode, HiveError};

use super::records::is_canonical_label_name;
use super::{DoltFailure, DoltSuccess, Snapshot, is_uuid_v7};

const MAX_LIMIT: u64 = 100;
const UUID_V7_PATTERN: &str =
    "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$";

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
enum Scope<'a> {
    All,
    Issue(&'a str),
    Area(&'a str),
    Project(&'a str),
}

impl<'a> Scope<'a> {
    fn parse(args: &'a LabelListArgs, project: Option<&'a ProjectId>) -> Result<Self, HiveError> {
        let scope = match (
            args.all,
            args.issue.as_deref(),
            args.area.as_deref(),
            project,
        ) {
            (true, None, None, None) => Self::All,
            (false, Some(id), None, None) => Self::Issue(id),
            (false, None, Some(id), None) => Self::Area(id),
            (false, None, None, Some(id)) => Self::Project(id.as_str()),
            (false, None, None, None) => {
                return Err(HiveError::new(
                    ErrorCode::InvalidArgument,
                    "Select one scope with --all, --issue, --area, or --project.",
                ));
            }
            _ => {
                return Err(HiveError::new(
                    ErrorCode::AmbiguousScope,
                    "Select only one of --all, --issue, --area, or --project.",
                ));
            }
        };
        if let Some((_, id)) = scope.endpoint()
            && !is_uuid_v7(id)
        {
            return Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "Use a canonical lowercase UUIDv7 for the selected scope.",
            ));
        }
        Ok(scope)
    }

    fn endpoint(self) -> Option<(&'static str, &'a str)> {
        match self {
            Self::All => None,
            Self::Issue(id) => Some(("issues", id)),
            Self::Area(id) => Some(("areas", id)),
            Self::Project(id) => Some(("projects", id)),
        }
    }
}

pub(crate) fn list(
    args: &LabelListArgs,
    project: Option<&ProjectId>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let scope = Scope::parse(args, project).map_err(DoltFailure::without_context)?;
    let limit = args
        .limit
        .parse::<u64>()
        .ok()
        .filter(|limit| (1..=MAX_LIMIT).contains(limit))
        .ok_or_else(|| {
            DoltFailure::without_context(HiveError::new(
                ErrorCode::InvalidArgument,
                "Use a page size between 1 and 100.",
            ))
        })?;
    let (mut snapshot, workspace_id) =
        Snapshot::open(&args.target).map_err(DoltFailure::without_context)?;
    let context = json!({
        "workspace_id": workspace_id,
        "db_commit": snapshot.commit,
        "scope": scope,
    });
    let binding = json!([
        "label-list-v1",
        workspace_id,
        args.target.database,
        args.target
            .port
            .as_deref()
            .and_then(std::ffi::OsStr::to_str),
        snapshot.commit,
        scope,
        limit,
    ]);
    match list_page(
        &mut snapshot,
        scope,
        limit,
        &binding,
        args.cursor.as_deref(),
    ) {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn verify_scope(snapshot: &mut Snapshot, scope: Scope<'_>) -> Result<(), HiveError> {
    let Some((table, id)) = scope.endpoint() else {
        return Ok(());
    };
    let found: Vec<String> = snapshot.exec_rows(
        |commit| {
            format!("SELECT id FROM {table} AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1")
        },
        (id,),
    )?;
    if !matches!(found.as_slice(), [actual] if actual == id) {
        return Err(HiveError::new(
            ErrorCode::NotFound,
            "The selected scope was not found at the verified Dolt commit.",
        ));
    }
    // Check member Issue endpoints before grouping shared labels. Otherwise an
    // exact join could silently drop a dangling Project membership.
    let invalid: Vec<u8> = match scope {
        Scope::Area(_) => snapshot.exec_rows(
            |commit| format!(
                "SELECT 1 FROM issues AS OF '{commit}' AS i
                 WHERE CAST(i.area_id AS BINARY) = ? AND NOT REGEXP_LIKE(i.id, ?, 'c') LIMIT 1"
            ),
            (id, UUID_V7_PATTERN),
        )?,
        Scope::Project(_) => snapshot.exec_rows(
            |commit| format!(
                "SELECT 1 FROM project_issues AS OF '{commit}' AS p
                 LEFT JOIN issues AS OF '{commit}' AS i ON CAST(i.id AS BINARY) = CAST(p.issue_id AS BINARY)
                 WHERE CAST(p.project_id AS BINARY) = ?
                   AND (i.id IS NULL OR NOT REGEXP_LIKE(i.id, ?, 'c')) LIMIT 1"
            ),
            (id, UUID_V7_PATTERN),
        )?,
        Scope::Issue(_) | Scope::All => return Ok(()),
    };
    if !invalid.is_empty() {
        return Err(HiveError::new(
            ErrorCode::InvalidRecord,
            "A selected scope has an invalid Issue endpoint at the verified Dolt commit.",
        ));
    }
    Ok(())
}

fn list_page(
    snapshot: &mut Snapshot,
    scope: Scope<'_>,
    limit: u64,
    binding: &Value,
    cursor: Option<&str>,
) -> Result<Value, HiveError> {
    let (comparison, after) = match cursor {
        None => (">=", ""),
        Some(cursor) => {
            let id = cursor.get(..36).unwrap_or_default();
            if !is_uuid_v7(id) || cursor != encode_cursor(id, binding) {
                return Err(HiveError::new(
                    ErrorCode::StaleCursor,
                    "The Label list cursor is invalid for the selected target, commit, scope, or page size.",
                ));
            }
            (">", id)
        }
    };
    verify_scope(snapshot, scope)?;
    let mut params = Vec::new();
    if let Some((_, id)) = scope.endpoint() {
        params.push(SqlValue::from(id));
    }
    params.extend([SqlValue::from(after), SqlValue::from(limit + 1)]);
    let rows: Vec<(String, Option<String>, Option<String>)> =
        snapshot.exec_rows(|commit| label_query(commit, scope, comparison), params)?;
    // Validate the lookahead too. A missing exact Label endpoint is observable
    // only as an error for the selected scope, without disclosing the bad link.
    for (id, name, _) in &rows {
        if !is_uuid_v7(id) || !name.as_deref().is_some_and(is_canonical_label_name) {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "A selected Label has an invalid ID, name, or missing endpoint at the verified Dolt commit.",
            ));
        }
    }
    let truncated = rows.len() as u64 > limit;
    let next_cursor = truncated.then(|| encode_cursor(&rows[limit as usize - 1].0, binding));
    let items: Vec<_> = rows
        .into_iter()
        .take(limit as usize)
        .map(|(id, name, revision)| json!({"id": id, "name": name, "revision": revision}))
        .collect();
    Ok(json!({
        "items": items,
        "next_cursor": next_cursor,
        "truncated": truncated,
        "observed_revision": snapshot.commit,
    }))
}

fn label_query(commit: &str, scope: Scope<'_>, comparison: &str) -> String {
    let (joins, predicate) = match scope {
        Scope::All => {
            return format!(
                "SELECT id, name, revision FROM labels AS OF '{commit}'
             WHERE CAST(id AS BINARY) {comparison} ? ORDER BY CAST(id AS BINARY) LIMIT ?"
            );
        }
        Scope::Issue(_) => (String::new(), "CAST(il.issue_id AS BINARY) = ?"),
        Scope::Area(_) => (
            format!(
                "JOIN issues AS OF '{commit}' AS i ON CAST(i.id AS BINARY) = CAST(il.issue_id AS BINARY)"
            ),
            "CAST(i.area_id AS BINARY) = ?",
        ),
        Scope::Project(_) => (
            format!(
                "JOIN project_issues AS OF '{commit}' AS p ON CAST(p.issue_id AS BINARY) = CAST(il.issue_id AS BINARY)"
            ),
            "CAST(p.project_id AS BINARY) = ?",
        ),
    };
    format!(
        "SELECT DISTINCT CAST(il.label_id AS BINARY) AS label_id, l.name, l.revision
         FROM issue_labels AS OF '{commit}' AS il
         {joins}
         LEFT JOIN labels AS OF '{commit}' AS l ON CAST(l.id AS BINARY) = CAST(il.label_id AS BINARY)
         WHERE {predicate} AND CAST(il.label_id AS BINARY) {comparison} ?
         ORDER BY label_id LIMIT ?"
    )
}

// Bytewise keyset pagination is stable within the immutable query snapshot.
fn encode_cursor(id: &str, binding: &Value) -> String {
    let digest = Sha256::digest(json!([binding, id]).to_string());
    format!("{id}.{digest:x}")
}

use mysql::Value as SqlValue;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{ResourceListArgs, ResourceShowArgs};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, Snapshot, is_uuid_v7};

const MAX_LIMIT: u64 = 100;
const SORT: &str = "CAST(r.id AS BINARY) ASC";
const SUMMARY_COLUMNS: &str = "r.id, r.kind, r.name, r.revision";

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
enum Scope<'a> {
    All,
    Area(&'a str),
    Issue(&'a str),
}

type ResourceRow = (String, String, String, String, Option<String>);

pub(crate) fn list(args: &ResourceListArgs) -> Result<DoltSuccess, Box<DoltFailure>> {
    let scope = match (args.all, args.area.as_deref(), args.issue.as_deref()) {
        (true, None, None) => Scope::All,
        (false, Some(id), None) => Scope::Area(id),
        (false, None, Some(id)) => Scope::Issue(id),
        (false, None, None) => {
            return Err(DoltFailure::without_context(HiveError::new(
                ErrorCode::InvalidArgument,
                "Select one scope with --all, --area, or --issue.",
            )));
        }
        _ => {
            return Err(DoltFailure::without_context(HiveError::new(
                ErrorCode::AmbiguousScope,
                "Select only one of --all, --area, or --issue.",
            )));
        }
    };
    if let Scope::Area(id) | Scope::Issue(id) = scope {
        check_id(id).map_err(DoltFailure::without_context)?;
    }
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
        "resource-list-v1",
        workspace_id,
        snapshot.commit,
        scope,
        SORT,
        limit
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

pub(crate) fn show(args: &ResourceShowArgs) -> Result<DoltSuccess, Box<DoltFailure>> {
    check_id(&args.resource).map_err(DoltFailure::without_context)?;
    let (mut snapshot, workspace_id) =
        Snapshot::open(&args.target).map_err(DoltFailure::without_context)?;
    let context = json!({
        "workspace_id": workspace_id,
        "db_commit": snapshot.commit,
        "scope": {"kind": "resource", "id": args.resource},
    });
    match show_resource(&mut snapshot, &args.resource) {
        Ok(resource) => Ok(DoltSuccess {
            context,
            data: json!({"resource": resource}),
        }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn check_id(id: &str) -> Result<(), HiveError> {
    if is_uuid_v7(id) {
        Ok(())
    } else {
        Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply a canonical lowercase UUIDv7.",
        ))
    }
}

fn show_resource(snapshot: &mut Snapshot, id: &str) -> Result<Value, HiveError> {
    let rows: Vec<ResourceRow> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT {SUMMARY_COLUMNS}, r.uri FROM resources AS OF '{commit}' AS r
             WHERE CAST(r.id AS BINARY) = ? LIMIT 1"
            )
        },
        (id,),
    )?;
    let row = rows.into_iter().next().ok_or_else(|| {
        HiveError::new(
            ErrorCode::NotFound,
            "The selected Resource was not found at the verified Dolt commit.",
        )
    })?;
    let uri = row.4.as_deref();
    let mut detail = summary(&row)?;
    detail["uri"] = json!(uri);
    Ok(detail)
}

fn list_page(
    snapshot: &mut Snapshot,
    scope: Scope<'_>,
    limit: u64,
    binding: &Value,
    cursor: Option<&str>,
) -> Result<Value, HiveError> {
    let offset = cursor
        .map(|cursor| cursor_offset(cursor, binding))
        .transpose()?
        .unwrap_or(0);
    let link = match scope {
        Scope::All => None,
        Scope::Area(id) => Some(("areas", "area_resources", "area_id", id)),
        Scope::Issue(id) => Some(("issues", "issue_resources", "issue_id", id)),
    };
    let mut params = Vec::new();
    if let Some((table, _, _, id)) = link {
        let rows: Vec<String> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT id FROM {table} AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
                )
            },
            (id,),
        )?;
        if rows.is_empty() {
            return Err(HiveError::new(
                ErrorCode::NotFound,
                "The selected scope was not found at the verified Dolt commit.",
            ));
        }
        params.push(SqlValue::from(id));
    }
    params.extend([SqlValue::from(limit + 1), SqlValue::from(offset)]);
    let rows: Vec<ResourceRow> = snapshot.exec_rows(
        |commit| {
            let join = link
                .map(|(_, table, column, _)| {
                    format!(
                        " JOIN {table} AS OF '{commit}' AS j
                 ON CAST(j.resource_id AS BINARY) = CAST(r.id AS BINARY)
                 WHERE CAST(j.{column} AS BINARY) = ?"
                    )
                })
                .unwrap_or_default();
            format!(
                "SELECT {SUMMARY_COLUMNS}, NULL FROM resources AS OF '{commit}' AS r{join}
                 ORDER BY {SORT} LIMIT ? OFFSET ?"
            )
        },
        params,
    )?;
    let truncated = rows.len() as u64 > limit;
    let mut items = rows.iter().map(summary).collect::<Result<Vec<_>, _>>()?;
    items.truncate(limit as usize);
    Ok(json!({
        "items": items,
        "next_cursor": truncated.then(|| encode_cursor(offset + limit, binding)),
        "truncated": truncated,
        "observed_revision": snapshot.commit,
    }))
}

fn summary((id, kind, name, revision, _): &ResourceRow) -> Result<Value, HiveError> {
    if !is_uuid_v7(id) {
        return Err(HiveError::new(
            ErrorCode::InvalidRecord,
            "A selected Resource has an invalid ID at the verified Dolt commit.",
        ));
    }
    Ok(json!({"id": id, "kind": kind, "name": name, "revision": revision}))
}

// The immutable snapshot and bytewise ID sort make offsets stable.
fn encode_cursor(offset: u64, binding: &Value) -> String {
    let digest = Sha256::digest(json!([binding, offset]).to_string());
    format!("{offset}.{digest:x}")
}

fn cursor_offset(cursor: &str, binding: &Value) -> Result<u64, HiveError> {
    if cursor.len() <= 85
        && let Some((offset, _)) = cursor.split_once('.')
        && let Ok(offset) = offset.parse::<u64>()
        && offset > 0
        && offset <= u64::MAX - MAX_LIMIT
        && cursor == encode_cursor(offset, binding)
    {
        return Ok(offset);
    }
    Err(HiveError::new(
        ErrorCode::StaleCursor,
        "The Resource list cursor is invalid for the selected workspace, commit, scope, or query.",
    ))
}

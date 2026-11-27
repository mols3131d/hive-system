use mysql::Value as SqlValue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{AreaListArgs, AreaShowArgs, is_slug};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, Snapshot, is_uuid_v7};

const MAX_LIMIT: u64 = 100;
const SORT: &str = "CAST(slug AS BINARY) ASC, CAST(id AS BINARY) ASC";
const SUMMARY_COLUMNS: &str = "id, parent_id, slug, name, \
    DATE_FORMAT(archived_at, '%Y-%m-%dT%H:%i:%s.%fZ'), revision";

type AreaRow = (
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
);

pub(crate) fn list(args: &AreaListArgs) -> Result<DoltSuccess, Box<DoltFailure>> {
    let parent = args.parent.as_deref();
    if let Some(id) = parent {
        check_selector(id).map_err(DoltFailure::without_context)?;
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
    let scope = match parent {
        Some(id) => json!({"kind": "children", "id": id}),
        None => json!({"kind": "roots"}),
    };
    let binding = json!([
        "area-list-v1",
        workspace_id,
        snapshot.commit,
        &scope,
        SORT,
        limit
    ]);
    let context = json!({
        "workspace_id": workspace_id,
        "db_commit": snapshot.commit,
        "scope": scope,
    });
    match list_page(
        &mut snapshot,
        parent,
        limit,
        &binding,
        args.cursor.as_deref(),
    ) {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

pub(crate) fn show(args: &AreaShowArgs) -> Result<DoltSuccess, Box<DoltFailure>> {
    check_selector(&args.area).map_err(DoltFailure::without_context)?;
    let (mut snapshot, workspace_id) =
        Snapshot::open(&args.target).map_err(DoltFailure::without_context)?;
    let context = json!({
        "workspace_id": workspace_id,
        "db_commit": snapshot.commit,
        "scope": {"kind": "area", "id": args.area},
    });
    let result = select_area(
        &mut snapshot,
        &format!("{SUMMARY_COLUMNS}, description"),
        &args.area,
    )
    .and_then(area_value);
    match result {
        Ok(area) => Ok(DoltSuccess {
            context,
            data: json!({"area": area}),
        }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn check_selector(id: &str) -> Result<(), HiveError> {
    if is_uuid_v7(id) {
        Ok(())
    } else {
        Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply a canonical lowercase UUIDv7 Area ID.",
        ))
    }
}

fn select_area<T: mysql::prelude::FromRow>(
    snapshot: &mut Snapshot,
    columns: &str,
    id: &str,
) -> Result<T, HiveError> {
    let mut rows = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT {columns} FROM areas AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
            )
        },
        (id,),
    )?;
    match rows.len() {
        1 => Ok(rows.pop().expect("single row")),
        0 => Err(HiveError::new(
            ErrorCode::NotFound,
            "The selected Area was not found at the verified Dolt commit.",
        )),
        _ => Err(HiveError::new(
            ErrorCode::SchemaIncompatible,
            "Dolt returned multiple rows for a unique Area ID.",
        )),
    }
}

fn list_page(
    snapshot: &mut Snapshot,
    parent: Option<&str>,
    limit: u64,
    binding: &Value,
    cursor: Option<&str>,
) -> Result<Value, HiveError> {
    let offset = match cursor {
        Some(cursor) => cursor_offset(cursor, binding)?,
        None => 0,
    };

    let mut params = Vec::new();
    let predicate = if let Some(id) = parent {
        select_area::<(String,)>(snapshot, "id", id)?;
        params.push(SqlValue::from(id));
        "CAST(parent_id AS BINARY) = ?"
    } else {
        "parent_id IS NULL"
    };
    params.extend([SqlValue::from(limit + 1), SqlValue::from(offset)]);
    let rows: Vec<AreaRow> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT {SUMMARY_COLUMNS}, NULL FROM areas AS OF '{commit}' \
                 WHERE {predicate} ORDER BY {SORT} LIMIT ? OFFSET ?"
            )
        },
        params,
    )?;

    let truncated = rows.len() as u64 > limit;
    let mut items = rows
        .into_iter()
        .map(area_value)
        .collect::<Result<Vec<_>, _>>()?;
    items.truncate(limit as usize);
    let next_cursor = truncated.then(|| encode_cursor(offset + limit, binding));
    Ok(json!({
        "items": items,
        "next_cursor": next_cursor,
        "truncated": truncated,
        "observed_revision": snapshot.commit,
    }))
}

fn area_value(row: AreaRow) -> Result<Value, HiveError> {
    let (id, parent_id, slug, name, archived_at, revision, description) = row;
    if !is_uuid_v7(&id)
        || parent_id
            .as_deref()
            .is_some_and(|parent| !is_uuid_v7(parent))
        || !is_slug(&slug)
    {
        return Err(HiveError::new(
            ErrorCode::InvalidRecord,
            "A selected Area has invalid identity or slug fields at the verified Dolt commit.",
        ));
    }
    let mut area = json!({
        "id": id,
        "parent_id": parent_id,
        "slug": slug,
        "name": name,
        "archived_at": archived_at,
        "revision": revision,
    });
    if let Some(description) = description {
        area["description"] = json!(description);
    }
    Ok(area)
}

// The committed snapshot and binary sort make page offsets stable.
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
        "The Area list cursor is invalid for the selected workspace, commit, scope, or query.",
    ))
}

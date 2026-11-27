use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{DoltTargetArgs, is_slug};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, Snapshot, encode_id_cursor, id_cursor_key, is_uuid_v7};

const COLUMNS: &str =
    "id, slug, display_name, revision, DATE_FORMAT(retired_at, '%Y-%m-%dT%H:%i:%s.%fZ')";
const SORT: &str = "CAST(id AS BINARY) ASC";

type BotRow = (String, String, String, String, Option<String>);

#[derive(Serialize)]
struct BotMetadata {
    id: String,
    slug: String,
    display_name: String,
    revision: String,
    role: &'static str,
    status: &'static str,
    retired_at: Option<String>,
}

impl TryFrom<BotRow> for BotMetadata {
    type Error = HiveError;

    fn try_from((id, slug, display_name, revision, retired_at): BotRow) -> Result<Self, HiveError> {
        if !is_uuid_v7(&id) || !is_slug(&slug) {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "A selected Bot has an invalid UUIDv7 or slug at the verified Dolt commit.",
            ));
        }
        Ok(Self {
            id,
            slug,
            display_name,
            revision,
            role: "queen",
            status: if retired_at.is_some() {
                "retired"
            } else {
                "active"
            },
            retired_at,
        })
    }
}

pub(crate) fn list(
    target: &DoltTargetArgs,
    status: Option<&str>,
    limit: u32,
    cursor: Option<&str>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let status = status.unwrap_or("active");
    let predicate = match status {
        "active" => "retired_at IS NULL AND",
        "retired" => "retired_at IS NOT NULL AND",
        "all" => "",
        _ => {
            return Err(invalid(
                "Use active, retired, or all for the Bot registry status.",
            ));
        }
    };
    if !(1..=100).contains(&limit) {
        return Err(invalid("Use a page size between 1 and 100."));
    }
    let cursor = cursor
        .map(|cursor| {
            id_cursor_key(cursor)
                .map(|id| (id, cursor))
                .ok_or_else(stale_cursor)
        })
        .transpose()
        .map_err(DoltFailure::without_context)?;

    read(target, |snapshot, workspace_id| {
        let binding = json!([
            "bot-list-v1",
            workspace_id,
            snapshot.commit,
            SORT,
            status,
            limit
        ]);
        let (comparison, after) = match cursor {
            Some((id, cursor)) if cursor == encode_id_cursor(id, &binding) => (">", id),
            Some(_) => return Err(stale_cursor()),
            None => (">=", ""),
        };
        let rows: Vec<BotRow> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT {COLUMNS} FROM bots AS OF '{commit}' \
                 WHERE {predicate} CAST(id AS BINARY) {comparison} ? ORDER BY {SORT} LIMIT ?"
                )
            },
            (after, limit + 1),
        )?;
        let mut items: Vec<_> = rows
            .into_iter()
            .map(BotMetadata::try_from)
            .collect::<Result<_, _>>()?;
        let truncated = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = items
            .last()
            .filter(|_| truncated)
            .map(|item| encode_id_cursor(&item.id, &binding));
        Ok(json!({
            "items": items,
            "next_cursor": next_cursor,
            "truncated": truncated,
            "observed_revision": snapshot.commit,
        }))
    })
}

pub(crate) fn show(target: &DoltTargetArgs, id: &str) -> Result<DoltSuccess, Box<DoltFailure>> {
    if !is_uuid_v7(id) {
        return Err(invalid("Use an exact lowercase UUIDv7 for the Bot ID."));
    }
    read(target, |snapshot, _| {
        let rows: Vec<BotRow> = snapshot.exec_rows(
            |commit| format!(
                "SELECT {COLUMNS} FROM bots AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
            ),
            (id,),
        )?;
        let row = rows.into_iter().next().ok_or_else(|| {
            HiveError::new(
                ErrorCode::NotFound,
                "The selected Bot was not found at the verified Dolt commit.",
            )
        })?;
        Ok(json!({"bot": BotMetadata::try_from(row)?, "observed_revision": snapshot.commit}))
    })
}

fn read(
    target: &DoltTargetArgs,
    query: impl FnOnce(&mut Snapshot, &str) -> Result<Value, HiveError>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let (mut snapshot, workspace_id) =
        Snapshot::open(target).map_err(DoltFailure::without_context)?;
    let context = json!({"workspace_id": workspace_id, "db_commit": snapshot.commit});
    match query(&mut snapshot, &workspace_id) {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn invalid(message: &str) -> Box<DoltFailure> {
    DoltFailure::without_context(HiveError::new(ErrorCode::InvalidArgument, message))
}

fn stale_cursor() -> HiveError {
    HiveError::new(
        ErrorCode::StaleCursor,
        "The Bot list cursor is invalid for the selected workspace, commit, or query.",
    )
}

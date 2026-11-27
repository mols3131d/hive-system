use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{CommonOptions, ProjectRead, ProjectWrite};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, Snapshot, is_uuid_v7};

#[cfg(target_os = "linux")]
pub(super) mod planning;

pub(crate) fn write(
    common: &CommonOptions,
    command: &ProjectWrite,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    let result = planning::write(common, command);
    #[cfg(not(target_os = "linux"))]
    let result = {
        let _ = (common, command);
        Err(HiveError::new(
            ErrorCode::NotImplemented,
            "Project mutations require a verified Linux workspace runtime.",
        ))
    };
    result.map_err(DoltFailure::without_context)
}

const MAX_PAGE_SIZE: u64 = 100;
const MEMBER_LIMIT: usize = 25;
const SORT: &str = "CAST(id AS BINARY) ASC";

enum Query<'a> {
    List {
        status: Option<&'a str>,
        limit: u64,
        cursor: Option<&'a str>,
    },
    Show(&'a str),
}

impl<'a> Query<'a> {
    fn parse(command: &'a ProjectRead) -> Result<Self, HiveError> {
        let invalid = |message| HiveError::new(ErrorCode::InvalidArgument, message);
        match command {
            ProjectRead::List(args) => {
                let status = args.status.as_deref();
                if status.is_some_and(|status| !is_project_status(status)) {
                    return Err(invalid(
                        "Use active, completed, or archived as the Project status.",
                    ));
                }
                let limit = args
                    .limit
                    .parse::<u64>()
                    .ok()
                    .filter(|limit| (1..=MAX_PAGE_SIZE).contains(limit))
                    .ok_or_else(|| invalid("Use a page size between 1 and 100."))?;
                Ok(Self::List {
                    status,
                    limit,
                    cursor: args.cursor.as_deref(),
                })
            }
            ProjectRead::Show(args) => {
                let id = args
                    .project_id
                    .as_deref()
                    .filter(|id| is_uuid_v7(id))
                    .ok_or_else(|| {
                        invalid("Supply a canonical lowercase UUIDv7 Project selector.")
                    })?;
                Ok(Self::Show(id))
            }
        }
    }
}

pub(crate) fn read(command: &ProjectRead) -> Result<DoltSuccess, Box<DoltFailure>> {
    let query = Query::parse(command).map_err(DoltFailure::without_context)?;
    let (mut snapshot, workspace_id) =
        Snapshot::open(command.target()).map_err(DoltFailure::without_context)?;
    let scope = match query {
        Query::List { .. } => json!({"kind": "all"}),
        Query::Show(id) => json!({"kind": "project", "id": id}),
    };
    let context = json!({
        "workspace_id": workspace_id,
        "db_commit": snapshot.commit,
        "scope": scope,
    });
    let result = match query {
        Query::List {
            status,
            limit,
            cursor,
        } => {
            let binding = json!([
                "project-list-v1",
                workspace_id,
                snapshot.commit,
                SORT,
                status,
                limit
            ]);
            list(&mut snapshot, status, limit, cursor, &binding)
        }
        Query::Show(id) => show(&mut snapshot, id),
    };
    match result {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn list(
    snapshot: &mut Snapshot,
    status: Option<&str>,
    limit: u64,
    cursor: Option<&str>,
    binding: &Value,
) -> Result<Value, HiveError> {
    let offset = cursor
        .map(|cursor| cursor_offset(cursor, binding))
        .transpose()?
        .unwrap_or(0);
    let rows: Vec<(String, String, String, String)> = snapshot.exec_rows(
        |commit| format!(
            "SELECT id, name, status, revision FROM projects AS OF '{commit}'              WHERE (? IS NULL OR CAST(status AS BINARY) = ?) ORDER BY {SORT} LIMIT ? OFFSET ?"
        ),
        (status, status, limit + 1, offset),
    )?;
    for (id, _, status, _) in &rows {
        validate_summary(id, status)?;
    }
    let truncated = rows.len() as u64 > limit;
    let items: Vec<_> = rows.into_iter().take(limit as usize)
        .map(|(id, name, status, revision)| {
            json!({"id": id, "name": name, "status": status, "revision": revision})
        })
        .collect();
    Ok(json!({
        "items": items,
        "next_cursor": truncated.then(|| encode_cursor(offset + limit, binding)),
        "truncated": truncated,
        "observed_revision": snapshot.commit,
    }))
}

fn show(snapshot: &mut Snapshot, id: &str) -> Result<Value, HiveError> {
    let projects: Vec<(String, String, Option<String>, String, String)> = snapshot.exec_rows(
        |commit| format!(
            "SELECT id, name, description, status, revision FROM projects AS OF '{commit}'              WHERE CAST(id AS BINARY) = ? LIMIT 1"
        ),
        (id,),
    )?;
    let (id, name, description, status, revision) =
        projects.into_iter().next().ok_or_else(|| {
            HiveError::new(
                ErrorCode::NotFound,
                "The requested Project was not found at the selected Dolt commit.",
            )
        })?;
    validate_summary(&id, &status)?;

    // Follow direct membership across Areas. Only select Issue IDs, including
    // the exact joined ID to detect broken relationships in the snapshot.
    let members: Vec<(String, Option<String>)> = snapshot.exec_rows(
        |commit| format!(
            "SELECT p.issue_id, i.id FROM project_issues AS OF '{commit}' AS p              LEFT JOIN issues AS OF '{commit}' AS i ON CAST(i.id AS BINARY) = CAST(p.issue_id AS BINARY)              WHERE CAST(p.project_id AS BINARY) = ?              ORDER BY CAST(p.issue_id AS BINARY) ASC LIMIT {}",
            MEMBER_LIMIT + 1,
        ),
        (&id,),
    )?;
    for (id, issue_id) in &members {
        if !is_uuid_v7(id) || issue_id.as_ref() != Some(id) {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "A selected Project membership has an invalid Issue reference at the verified Dolt commit.",
            ));
        }
    }
    let truncated = members.len() > MEMBER_LIMIT;
    let items: Vec<_> = members
        .into_iter()
        .take(MEMBER_LIMIT)
        .map(|(id, _)| id)
        .collect();
    Ok(json!({
        "project": {"id": id, "name": name, "description": description, "status": status, "revision": revision},
        "issues": {"items": items, "truncated": truncated},
    }))
}

pub(super) fn is_project_status(status: &str) -> bool {
    matches!(status, "active" | "completed" | "archived")
}

fn validate_summary(id: &str, status: &str) -> Result<(), HiveError> {
    if !is_uuid_v7(id) || !is_project_status(status) {
        return Err(HiveError::new(
            ErrorCode::InvalidRecord,
            "A selected Project has invalid summary fields at the verified Dolt commit.",
        ));
    }
    Ok(())
}

fn encode_cursor(offset: u64, binding: &Value) -> String {
    let digest = Sha256::digest(json!([binding, offset]).to_string());
    format!("{offset}.{digest:x}")
}

fn cursor_offset(cursor: &str, binding: &Value) -> Result<u64, HiveError> {
    if cursor.len() <= 85
        && let Some((offset, _)) = cursor.split_once('.')
        && let Ok(offset) = offset.parse::<u64>()
        && offset > 0
        && offset <= u64::MAX - MAX_PAGE_SIZE
        && cursor == encode_cursor(offset, binding)
    {
        return Ok(offset);
    }
    Err(HiveError::new(
        ErrorCode::StaleCursor,
        "The Project list cursor is invalid for the selected workspace, commit, or query.",
    ))
}

use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{DirectiveSelection, DirectiveStatus, ProjectId};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, Snapshot, encode_id_cursor, id_cursor_key, is_uuid_v7};

const METADATA: &str = "id, scope_kind, area_id, project_id, include_descendants, title, status,
    revision, DATE_FORMAT(valid_from, '%Y-%m-%dT%H:%i:%s.%fZ'),
    DATE_FORMAT(expires_at, '%Y-%m-%dT%H:%i:%s.%fZ'), approval_ref IS NOT NULL";
const SORT: &str = "CAST(id AS BINARY) ASC";
const MAX_INLINE_BYTES: u32 = 64 * 1024;

type MetadataRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    i8,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    bool,
);
type DetailRow = (String, String, String, Option<String>);

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
enum Scope<'a> {
    Workspace,
    Area(&'a str),
    Project(&'a str),
}

impl<'a> Scope<'a> {
    fn parse(
        selection: &'a DirectiveSelection,
        project: Option<&'a ProjectId>,
    ) -> Result<Self, HiveError> {
        match (
            selection.workspace_scope,
            selection.area.as_deref(),
            project.map(ProjectId::as_str),
        ) {
            (true, None, None) => Ok(Self::Workspace),
            (false, Some(id), None) if is_uuid_v7(id) => Ok(Self::Area(id)),
            (false, None, Some(id)) if is_uuid_v7(id) => Ok(Self::Project(id)),
            (false, Some(_), None) | (false, None, Some(_)) => Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "Use a canonical lowercase UUIDv7 for the Directive scope ID.",
            )),
            (false, None, None) => Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "Select one Directive scope with --workspace-scope, --area, or --project.",
            )),
            _ => Err(HiveError::new(
                ErrorCode::AmbiguousScope,
                "Select only one of --workspace-scope, --area, or --project.",
            )),
        }
    }

    fn verify(self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let (table, id) = match self {
            Self::Workspace => return Ok(()),
            Self::Area(id) => ("areas", id),
            Self::Project(id) => ("projects", id),
        };
        let rows: Vec<String> = snapshot.exec_rows(
            |commit| format!("SELECT id FROM {table} AS OF '{commit}' WHERE id = ? LIMIT 1"),
            (id,),
        )?;
        match rows.first() {
            Some(actual) if actual == id => Ok(()),
            Some(_) => Err(invalid_record()),
            None => Err(HiveError::new(
                ErrorCode::NotFound,
                "The selected scope was not found at the verified Dolt commit.",
            )),
        }
    }

    fn filter(self) -> (&'static str, &'a str) {
        match self {
            Self::Workspace => ("scope_kind = ?", "workspace"),
            Self::Area(id) => ("scope_kind = 'area' AND area_id = ?", id),
            Self::Project(id) => ("scope_kind = 'project' AND project_id = ?", id),
        }
    }

    fn metadata(self, row: MetadataRow) -> Result<Metadata, HiveError> {
        let (
            id,
            kind,
            area,
            project,
            descendants,
            title,
            status,
            revision,
            valid_from,
            expires_at,
            has_approval,
        ) = row;
        let shape = (
            kind.as_str(),
            area.as_deref(),
            project.as_deref(),
            descendants,
        );
        let exact_scope = match (self, shape) {
            (Self::Workspace, ("workspace", None, None, 0)) => true,
            (Self::Area(expected), ("area", Some(actual), None, 0 | 1)) => expected == actual,
            (Self::Project(expected), ("project", None, Some(actual), 0)) => expected == actual,
            _ => false,
        };
        if !exact_scope
            || !is_uuid_v7(&id)
            || !matches!(
                status.as_str(),
                "proposed" | "active" | "superseded" | "withdrawn"
            )
            || (status == "active" && !has_approval)
            || matches!((&valid_from, &expires_at), (Some(start), Some(end)) if end <= start)
        {
            return Err(invalid_record());
        }
        Ok(Metadata {
            id,
            title,
            status,
            revision,
            valid_from,
            expires_at,
            include_descendants: descendants == 1,
        })
    }
}

#[derive(Serialize)]
struct Metadata {
    id: String,
    title: String,
    status: String,
    revision: String,
    valid_from: Option<String>,
    expires_at: Option<String>,
    include_descendants: bool,
}

pub(crate) fn list(
    selection: &DirectiveSelection,
    project: Option<&ProjectId>,
    status: DirectiveStatus,
    limit: u32,
    cursor: Option<&str>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    if !(1..=100).contains(&limit) {
        return Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::InvalidArgument,
            "Use a page size between 1 and 100.",
        )));
    }
    let status = match status {
        DirectiveStatus::Proposed => "proposed",
        DirectiveStatus::Active => "active",
        DirectiveStatus::Superseded => "superseded",
        DirectiveStatus::Withdrawn => "withdrawn",
        DirectiveStatus::All => "all",
    };
    read(selection, project, |snapshot, workspace_id, scope| {
        let binding = json!([
            "directive-list-v1",
            workspace_id,
            snapshot.commit,
            scope,
            status,
            SORT,
            limit
        ]);
        let (comparison, after) = match cursor {
            Some(cursor) => match id_cursor_key(cursor) {
                Some(id) if cursor == encode_id_cursor(id, &binding) => (">", id),
                _ => {
                    return Err(HiveError::new(
                        ErrorCode::StaleCursor,
                        "The Directive cursor is invalid for the selected workspace, commit, or query.",
                    ));
                }
            },
            None => (">=", ""),
        };
        let (filter, value) = scope.filter();
        let rows: Vec<MetadataRow> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT {METADATA} FROM directives AS OF '{commit}'
                 WHERE {filter} AND (? = 'all' OR status = ?)
                 AND CAST(id AS BINARY) {comparison} ? ORDER BY {SORT} LIMIT ?"
                )
            },
            (value, status, status, after, limit + 1),
        )?;
        let mut items = rows
            .into_iter()
            .map(|row| scope.metadata(row))
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = items
            .last()
            .filter(|_| truncated)
            .map(|item| encode_id_cursor(&item.id, &binding));
        Ok(
            json!({"items": items, "truncated": truncated, "next_cursor": next_cursor, "observed_revision": snapshot.commit}),
        )
    })
}

pub(crate) fn show(
    selection: &DirectiveSelection,
    project: Option<&ProjectId>,
    id: &str,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    if !is_uuid_v7(id) {
        return Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::InvalidArgument,
            "Use an exact lowercase UUIDv7 for the Directive ID.",
        )));
    }
    read(selection, project, |snapshot, _, scope| {
        let (filter, value) = scope.filter();
        let rows: Vec<MetadataRow> = snapshot.exec_rows(
            |commit| format!("SELECT {METADATA} FROM directives AS OF '{commit}' WHERE {filter} AND id = ? LIMIT 1"),
            (value, id),
        )?;
        let row = rows.into_iter().next().ok_or_else(|| HiveError::new(
            ErrorCode::NotFound, "The selected Directive was not found in the exact scope at the verified Dolt commit.",
        ))?;
        let metadata = scope.metadata(row)?;
        if metadata.id != id {
            return Err(invalid_record());
        }

        // The same immutable commit and exact selector gate the body transfer.
        let detail: Vec<DetailRow> = snapshot.exec_rows(
            |commit| {
                format!(
                "SELECT body, issuer_ref, source_ref, approval_ref FROM directives AS OF '{commit}'
                 WHERE {filter} AND id = ?
                   AND OCTET_LENGTH(body) + OCTET_LENGTH(issuer_ref) + OCTET_LENGTH(source_ref)
                       + COALESCE(OCTET_LENGTH(approval_ref), 0) <= ? LIMIT 1"
            )
            },
            (value, id, MAX_INLINE_BYTES),
        )?;
        let (body, issuer_ref, source_ref, approval_ref) =
            detail.into_iter().next().ok_or_else(|| {
                HiveError::new(
                    ErrorCode::ContextLimit,
                    "The selected Directive body and source metadata exceed 64 KiB.",
                )
            })?;
        Ok(json!({
            "directive": Detail { metadata, body, issuer_ref, source_ref, approval_ref },
            "observed_revision": snapshot.commit,
        }))
    })
}

#[derive(Serialize)]
struct Detail {
    #[serde(flatten)]
    metadata: Metadata,
    body: String,
    issuer_ref: String,
    source_ref: String,
    approval_ref: Option<String>,
}

fn read(
    selection: &DirectiveSelection,
    project: Option<&ProjectId>,
    query: impl FnOnce(&mut Snapshot, &str, Scope<'_>) -> Result<Value, HiveError>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let scope = Scope::parse(selection, project).map_err(DoltFailure::without_context)?;
    let (mut snapshot, workspace_id) =
        Snapshot::open(&selection.target).map_err(DoltFailure::without_context)?;
    let context =
        json!({"workspace_id": workspace_id, "db_commit": snapshot.commit, "scope": scope});
    match scope
        .verify(&mut snapshot)
        .and_then(|()| query(&mut snapshot, &workspace_id, scope))
    {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn invalid_record() -> HiveError {
    HiveError::new(
        ErrorCode::InvalidRecord,
        "A selected Directive or scope has invalid committed metadata.",
    )
}

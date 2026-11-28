use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{
    ArtifactCreateArgs, ArtifactListArgs, ArtifactShowArgs, ArtifactUpdateArgs, CommonOptions,
    DoltTargetArgs,
};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, Snapshot, is_uuid_v7};

pub(super) const SUMMARY_COLUMNS: &str = "a.id, a.kind, a.title, a.revision";

const MAX_LIMIT: u32 = 100;
pub(crate) const MAX_SCOPES: usize = 100;
const MAX_BODY_BYTES: u32 = 64 * 1024;

#[cfg(target_os = "linux")]
pub(super) mod create;
#[cfg(target_os = "linux")]
pub(super) mod update;
#[cfg(target_os = "linux")]
mod writes;

pub(crate) fn create(
    common: &CommonOptions,
    args: &ArtifactCreateArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        create::write(common, args).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args);
        Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            "Artifact create requires a verified Linux workspace runtime.",
        )))
    }
}

pub(crate) fn update(
    common: &CommonOptions,
    args: &ArtifactUpdateArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        update::write(common, args).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args);
        Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            "Artifact update requires a verified Linux workspace runtime.",
        )))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopeKind {
    Area,
    Project,
    Issue,
    Resource,
}

impl ScopeKind {
    const ALL: [Self; 4] = [Self::Area, Self::Project, Self::Issue, Self::Resource];

    pub(super) fn tables(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Area => ("areas", "artifact_areas", "area_id"),
            Self::Project => ("projects", "artifact_projects", "project_id"),
            Self::Issue => ("issues", "artifact_issues", "issue_id"),
            Self::Resource => ("resources", "artifact_resources", "resource_id"),
        }
    }
}

struct Scope<'a> {
    kind: ScopeKind,
    id: &'a str,
}

impl<'a> Scope<'a> {
    fn parse(value: &'a str) -> Result<Self, HiveError> {
        let error = || {
            argument(
                "Use area, project, issue, or resource followed by a colon and an exact lowercase UUIDv7.",
            )
        };
        let (kind, id) = value.split_once(':').ok_or_else(error)?;
        let kind = match kind {
            "area" => ScopeKind::Area,
            "project" => ScopeKind::Project,
            "issue" => ScopeKind::Issue,
            "resource" => ScopeKind::Resource,
            _ => return Err(error()),
        };
        if !is_uuid_v7(id) {
            return Err(error());
        }
        Ok(Self { kind, id })
    }

    fn verify(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let (table, _, _) = self.kind.tables();
        let rows: Vec<String> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT id FROM {table} AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
                )
            },
            (self.id,),
        )?;
        if rows.is_empty() {
            return Err(HiveError::new(
                ErrorCode::NotFound,
                "A selected scope was not found at the verified Dolt commit.",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub(super) struct Metadata {
    id: String,
    kind: String,
    title: String,
    revision: String,
}

type MetadataRow = (String, String, String, String);

impl TryFrom<MetadataRow> for Metadata {
    type Error = HiveError;

    fn try_from((id, kind, title, revision): MetadataRow) -> Result<Self, HiveError> {
        if !is_uuid_v7(&id) || !super::records::is_artifact_kind(&kind) {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "A selected Artifact has an invalid ID or kind at the verified Dolt commit.",
            ));
        }
        Ok(Self {
            id,
            kind,
            title,
            revision,
        })
    }
}

pub(crate) fn list(args: &ArtifactListArgs) -> Result<DoltSuccess, Box<DoltFailure>> {
    let scope = args
        .scope
        .as_deref()
        .ok_or_else(|| argument("Select one direct Artifact scope with --scope."))
        .and_then(Scope::parse)
        .map_err(DoltFailure::without_context)?;
    let limit = args
        .limit
        .parse::<u32>()
        .ok()
        .filter(|limit| (1..=MAX_LIMIT).contains(limit))
        .ok_or_else(|| {
            DoltFailure::without_context(argument("Use a page size between 1 and 100."))
        })?;

    read(&args.target, |snapshot, workspace| {
        let binding = json!([
            "artifact-list-v1",
            workspace,
            snapshot.commit,
            args.scope,
            limit
        ]);
        let after = match args.cursor.as_deref() {
            Some(cursor) => cursor_after(cursor, &binding)?,
            None => "",
        };
        scope.verify(snapshot)?;
        let (_, links, column) = scope.kind.tables();
        let rows: Vec<MetadataRow> = snapshot.exec_rows(
            |commit| format!(
                "SELECT {SUMMARY_COLUMNS} FROM artifacts AS OF '{commit}' AS a
                 JOIN {links} AS OF '{commit}' AS link ON CAST(link.artifact_id AS BINARY) = CAST(a.id AS BINARY)
                 WHERE CAST(link.{column} AS BINARY) = ? AND CAST(a.id AS BINARY) > ?
                 ORDER BY CAST(a.id AS BINARY) LIMIT ?"
            ),
            (scope.id, after, limit + 1),
        )?;
        let truncated = rows.len() > limit as usize;
        let items: Vec<_> = rows
            .into_iter()
            .take(limit as usize)
            .map(Metadata::try_from)
            .collect::<Result<_, _>>()?;
        let next_cursor = items
            .last()
            .filter(|_| truncated)
            .map(|item| encode_cursor(&item.id, &binding));
        Ok(json!({
            "items": items,
            "next_cursor": next_cursor,
            "truncated": truncated,
            "observed_revision": snapshot.commit,
        }))
    })
}

pub(crate) fn show(args: &ArtifactShowArgs) -> Result<DoltSuccess, Box<DoltFailure>> {
    if !is_uuid_v7(&args.id) {
        return Err(DoltFailure::without_context(argument(
            "Use an exact lowercase UUIDv7 for the Artifact ID.",
        )));
    }
    if args.scope.len() > MAX_SCOPES {
        return Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::ContextLimit,
            "Supply up to 100 explicit scopes for Artifact show.",
        )));
    }
    let scopes: Vec<_> = args
        .scope
        .iter()
        .map(|scope| Scope::parse(scope))
        .collect::<Result<_, _>>()
        .map_err(DoltFailure::without_context)?;

    read(&args.target, |snapshot, _| {
        let rows: Vec<String> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT id FROM artifacts AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
                )
            },
            (&args.id,),
        )?;
        if rows.is_empty() {
            return Err(HiveError::new(
                ErrorCode::NotFound,
                "The selected Artifact was not found at the verified Dolt commit.",
            ));
        }
        for scope in &scopes {
            scope.verify(snapshot)?;
        }
        verify_links(snapshot, &args.id, &scopes)?;

        // Apply the byte limit inside SQL so an oversized body never reaches the CLI.
        let rows: Vec<(String, String, String, String, Option<String>)> = snapshot.exec_rows(
            |commit| {
                format!(
                "SELECT id, kind, title, revision, CASE WHEN OCTET_LENGTH(body) <= ? THEN body END
                 FROM artifacts AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
            )
            },
            (MAX_BODY_BYTES, &args.id),
        )?;
        let (id, kind, title, revision, body) = rows.into_iter().next().ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidRecord,
                "The verified Artifact is unavailable.",
            )
        })?;
        let mut artifact = json!(Metadata::try_from((id, kind, title, revision))?);
        let body = body.ok_or_else(|| {
            HiveError::new(
                ErrorCode::ContextLimit,
                "The selected Artifact body exceeds 64 KiB.",
            )
        })?;
        artifact["body_bytes"] = json!(body.len());
        artifact["body"] = json!(body);
        Ok(json!({"artifact": artifact, "observed_revision": snapshot.commit}))
    })
}

fn verify_links(snapshot: &mut Snapshot, id: &str, scopes: &[Scope<'_>]) -> Result<(), HiveError> {
    for kind in ScopeKind::ALL {
        let (_, links, column) = kind.tables();
        // Include aliases accepted by the table collation, plus case aliases
        // in binary collations. A malformed link must block body disclosure.
        let rows: Vec<(String, String)> = snapshot.exec_rows(
                |commit| {
                    format!(
                        "SELECT artifact_id, {column} FROM {links} AS OF '{commit}'
                     WHERE artifact_id = CAST(? AS CHAR) OR CAST(LOWER(artifact_id) AS BINARY) = ? LIMIT ?"
                    )
                },
                (id, id, MAX_SCOPES + 1),
            )?;
        verify_coverage(kind, id, &rows, scopes)?;
    }
    Ok(())
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn verify_context_artifact_links(
    snapshot: &mut Snapshot,
    artifact: &str,
    issue: &str,
    area: &str,
    project: Option<&str>,
) -> Result<(), HiveError> {
    let mut scopes = vec![
        Scope {
            kind: ScopeKind::Issue,
            id: issue,
        },
        Scope {
            kind: ScopeKind::Area,
            id: area,
        },
    ];
    if let Some(id) = project {
        scopes.push(Scope {
            kind: ScopeKind::Project,
            id,
        });
    }
    verify_links(snapshot, artifact, &scopes)
}

fn verify_coverage(
    kind: ScopeKind,
    artifact_id: &str,
    links: &[(String, String)],
    scopes: &[Scope<'_>],
) -> Result<(), HiveError> {
    if links.len() > MAX_SCOPES
        || links.iter().any(|(artifact, endpoint)| {
            artifact != artifact_id
                || !scopes
                    .iter()
                    .any(|scope| scope.kind == kind && scope.id == endpoint)
        })
    {
        return Err(HiveError::new(
            ErrorCode::Unauthorized,
            "Artifact body disclosure requires explicit coverage of every linked scope.",
        ));
    }
    Ok(())
}

fn read(
    target: &DoltTargetArgs,
    query: impl FnOnce(&mut Snapshot, &str) -> Result<Value, HiveError>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let (mut snapshot, workspace) = Snapshot::open(target).map_err(DoltFailure::without_context)?;
    let context = json!({"workspace_id": workspace, "db_commit": snapshot.commit});
    match query(&mut snapshot, &workspace) {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn argument(message: &str) -> HiveError {
    HiveError::new(ErrorCode::InvalidArgument, message)
}

fn encode_cursor(id: &str, binding: &Value) -> String {
    let digest = Sha256::digest(json!([binding, id]).to_string());
    format!("{id}.{digest:x}")
}

fn cursor_after<'a>(cursor: &'a str, binding: &Value) -> Result<&'a str, HiveError> {
    if cursor.len() == 101
        && let Some((id, _)) = cursor.split_once('.')
        && is_uuid_v7(id)
        && cursor == encode_cursor(id, binding)
    {
        return Ok(id);
    }
    Err(HiveError::new(
        ErrorCode::StaleCursor,
        "The Artifact list cursor is invalid for the selected workspace, commit, scope, or query.",
    ))
}

#[cfg(test)]
mod tests {
    use super::{Scope, ScopeKind, verify_coverage};

    #[test]
    fn coverage_requires_every_exact_link_and_allows_standalone() {
        let artifact = "01900000-0000-7000-8000-00000000000a";
        let first = "01900000-0000-7000-8000-00000000000b";
        let second = "01900000-0000-7000-8000-00000000000c";
        let links = [
            (artifact.to_owned(), first.to_owned()),
            (artifact.to_owned(), second.to_owned()),
        ];
        let area = |id| Scope {
            kind: ScopeKind::Area,
            id,
        };

        assert!(verify_coverage(ScopeKind::Area, artifact, &[], &[]).is_ok());
        assert!(
            verify_coverage(
                ScopeKind::Area,
                artifact,
                &links,
                &[area(first), area(second)]
            )
            .is_ok()
        );
        assert!(verify_coverage(ScopeKind::Area, artifact, &links, &[area(first)]).is_err());
        assert!(
            verify_coverage(
                ScopeKind::Issue,
                artifact,
                &links,
                &[area(first), area(second)]
            )
            .is_err()
        );

        let aliased = [(artifact.to_ascii_uppercase(), first.to_owned())];
        assert!(verify_coverage(ScopeKind::Area, artifact, &aliased, &[area(first)]).is_err());
    }
}

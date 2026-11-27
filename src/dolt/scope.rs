use crate::cli::is_slug;
use crate::error::{AreaCandidate, ErrorCode, HiveError};

use super::{DoltTarget, Snapshot, is_uuid_v7};

const MAX_CANDIDATES: usize = 16;

pub(crate) struct VerifiedScope {
    pub(crate) commit: String,
    pub(crate) inferred_area: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct ScopeRef<'a> {
    pub(crate) id: &'a str,
    pub(crate) source: &'static str,
}

impl<'a> ScopeRef<'a> {
    pub(crate) fn select(cli: Option<&'a str>, bound: Option<&'a str>) -> Option<Self> {
        cli.or(bound).map(|id| Self {
            id,
            source: if cli.is_some() { "cli" } else { "binding" },
        })
    }
}

pub(crate) fn verify_scope(
    target: DoltTarget<'_>,
    resource: Option<ScopeRef<'_>>,
    area: Option<ScopeRef<'_>>,
    project: Option<ScopeRef<'_>>,
) -> Result<VerifiedScope, HiveError> {
    let (mut snapshot, _) =
        Snapshot::open_target(target).map_err(|error| error.with_input("dolt_target", "cli"))?;
    for (table, reference, kind, label) in [
        ("resources", resource, "resource", "Resource"),
        ("areas", area, "area", "Area"),
        ("projects", project, "project", "Project"),
    ] {
        if let Some(reference) = reference {
            let rows: Vec<String> = snapshot.exec_rows(
                |commit| {
                    format!("SELECT id FROM {table} AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1")
                },
                (reference.id,),
            ).map_err(|error| error.with_input(kind, reference.source))?;
            if rows.is_empty() {
                return Err(HiveError::new(
                    ErrorCode::NotFound,
                    format!("The selected {label} does not exist at the verified Dolt commit."),
                )
                .with_input(kind, reference.source));
            }
        }
    }

    let inferred_area = resource
        .map(|resource| {
            let (kind, source) =
                area.map_or(("resource", resource.source), |area| ("area", area.source));
            linked_area(&mut snapshot, resource.id, area.map(|area| area.id))
                .map_err(|error| error.with_input(kind, source))
        })
        .transpose()?
        .flatten();
    Ok(VerifiedScope {
        commit: snapshot.commit,
        inferred_area,
    })
}

fn linked_area(
    snapshot: &mut Snapshot,
    resource: &str,
    area: Option<&str>,
) -> Result<Option<String>, HiveError> {
    let rows: Vec<(String, Option<String>)> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT link.area_id, a.slug FROM area_resources AS OF '{commit}' AS link
                 LEFT JOIN areas AS OF '{commit}' AS a
                 ON CAST(link.area_id AS BINARY) = CAST(a.id AS BINARY)
                 WHERE CAST(link.resource_id AS BINARY) = ?
                 AND (? IS NULL OR CAST(link.area_id AS BINARY) = ?)
                 ORDER BY CAST(link.area_id AS BINARY) LIMIT ?"
            )
        },
        (
            resource,
            area,
            area,
            if area.is_some() {
                1
            } else {
                MAX_CANDIDATES + 1
            },
        ),
    )?;
    let truncated = rows.len() > MAX_CANDIDATES;
    let candidates = rows
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(id, slug)| {
            let slug = slug.filter(|slug| is_slug(slug));
            match slug {
                Some(slug) if is_uuid_v7(&id) => Ok(AreaCandidate { id, slug }),
                _ => Err(HiveError::new(
                    ErrorCode::InvalidRecord,
                    "The selected Resource has a missing or invalid linked Area at the verified Dolt commit.",
                )),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    match candidates.as_slice() {
        [] if area.is_some() => Err(HiveError::new(
            ErrorCode::NotFound,
            "The selected Area is not directly linked to the selected Resource at the verified Dolt commit.",
        )),
        [] => Ok(None),
        [_] if area.is_some() => Ok(None),
        [only] => Ok(Some(only.id.clone())),
        _ => Err(HiveError::ambiguous_areas(candidates, truncated)),
    }
}

use crate::cli::is_slug;
use crate::error::{AreaCandidate, ErrorCode, HiveError};

use super::{DoltTarget, Snapshot, is_uuid_v7};

const MAX_CANDIDATES: usize = 16;

pub(crate) struct VerifiedScope {
    pub(crate) commit: String,
    pub(crate) inferred_area: Option<String>,
}

pub(crate) fn verify_scope(
    target: DoltTarget<'_>,
    resource: Option<&str>,
    area: Option<&str>,
    project: Option<&str>,
) -> Result<VerifiedScope, HiveError> {
    let (mut snapshot, _) = Snapshot::open_target(target)?;
    for (table, id, kind) in [
        ("resources", resource, "Resource"),
        ("areas", area, "Area"),
        ("projects", project, "Project"),
    ] {
        if let Some(id) = id {
            let rows: Vec<String> = snapshot.exec_rows(
                |commit| {
                    format!("SELECT id FROM {table} AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1")
                },
                (id,),
            )?;
            if rows.is_empty() {
                return Err(HiveError::new(
                    ErrorCode::NotFound,
                    format!("The selected {kind} does not exist at the verified Dolt commit."),
                ));
            }
        }
    }

    let inferred_area = resource
        .map(|resource| linked_area(&mut snapshot, resource, area))
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

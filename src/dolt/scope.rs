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

#[cfg(target_os = "linux")]
pub(super) fn require_active_area(
    db: &mut impl mysql::prelude::Queryable,
    area: &str,
    archived_message: &'static str,
) -> Result<(), HiveError> {
    verify_area_ancestry(area, archived_message, |id| {
        db.exec_first(
            "SELECT parent_id, slug, archived_at IS NOT NULL FROM areas
             WHERE CAST(id AS BINARY) = ?",
            (id,),
        )
        .map_err(super::read_error)
    })
}

#[cfg(target_os = "linux")]
pub(super) fn require_active_area_at(
    db: &mut impl mysql::prelude::Queryable,
    area: &str,
    commit: &str,
    archived_message: &'static str,
) -> Result<(), HiveError> {
    if !super::is_commit_hash(commit) {
        return Err(HiveError::new(
            ErrorCode::InvalidRecord,
            "The Area ancestry commit is malformed.",
        ));
    }
    let sql = format!(
        "SELECT parent_id, slug, archived_at IS NOT NULL FROM areas AS OF '{commit}'
         WHERE CAST(id AS BINARY) = ?"
    );
    verify_area_ancestry(area, archived_message, |id| {
        db.exec_first(&sql, (id,)).map_err(super::read_error)
    })
}

#[cfg(target_os = "linux")]
fn verify_area_ancestry(
    area: &str,
    archived_message: &'static str,
    mut read: impl FnMut(&str) -> Result<Option<(Option<String>, String, bool)>, HiveError>,
) -> Result<(), HiveError> {
    let invalid = || {
        HiveError::new(
            ErrorCode::InvalidRecord,
            "The selected Area ancestry is invalid.",
        )
    };
    let mut visited = std::collections::HashSet::new();
    let mut current = area.to_owned();
    let mut archived = false;
    loop {
        if !is_uuid_v7(&current)
            || visited.len() == super::MAX_ROWS_PER_TABLE
            || !visited.insert(current.clone())
        {
            return Err(invalid());
        }
        let (parent, slug, is_archived) = read(&current)?.ok_or_else(invalid)?;
        if !is_slug(&slug) {
            return Err(invalid());
        }
        archived |= is_archived;
        let Some(parent) = parent else { break };
        current = parent;
    }
    if archived {
        return Err(HiveError::new(
            ErrorCode::InvalidTransition,
            archived_message,
        ));
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn requires_a_complete_active_area_chain() {
        let root = "01900000-0000-7000-8000-000000000001";
        let parent = "01900000-0000-7000-8000-000000000002";
        let leaf = "01900000-0000-7000-8000-000000000003";
        let verify = |root_row: Option<(Option<&str>, bool)>| {
            verify_area_ancestry(leaf, "Archived ancestry.", |id| {
                Ok(match id {
                    id if id == leaf => Some((Some(parent), false)),
                    id if id == parent => Some((Some(root), false)),
                    id if id == root => root_row,
                    _ => None,
                }
                .map(|(parent, archived)| {
                    (parent.map(ToOwned::to_owned), "synthetic".into(), archived)
                }))
            })
        };
        verify(Some((None, false))).unwrap();
        assert!(matches!(
            verify(Some((None, true))).unwrap_err().code,
            ErrorCode::InvalidTransition
        ));
        for root_row in [None, Some((Some(leaf), false))] {
            assert!(matches!(
                verify(root_row).unwrap_err().code,
                ErrorCode::InvalidRecord
            ));
        }
    }
}

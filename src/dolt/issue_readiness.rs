use std::collections::{HashMap, HashSet};

use mysql::Value as SqlValue;
use serde::Serialize;

use crate::cli::is_slug;
use crate::error::{ErrorCode, HiveError};

use super::issue_list::SummaryRow;
use super::records::{IssueStatus, is_issue_type};
use super::{MAX_ROWS_PER_TABLE, Snapshot, is_uuid_v7};

// Match a non-whitespace character using Unicode White_Space, as Rust str::trim does.
const NON_WHITESPACE: &str = r"[^\x{0009}-\x{000D}\x{0020}\x{0085}\x{00A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}]";

#[derive(Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum Blocker {
    Hold,
    CurrentClaim,
    PrerequisiteNotDone,
    InvalidPrerequisite,
    ArchivedArea,
    InvalidAreaAncestry,
    InvalidIssueMetadata,
}

type PrerequisiteRow = (String, String, String, Option<String>, Option<String>, bool);

#[derive(Default)]
pub(super) struct Readiness {
    areas: Option<HashMap<String, Area>>,
    ancestry: HashMap<String, Ancestry>,
    prerequisites_read: usize,
}

struct Area {
    parent: Option<String>,
    slug: String,
    archived: bool,
}

#[derive(Clone, Copy, Default)]
struct Ancestry {
    archived: bool,
    invalid: bool,
}

impl Readiness {
    pub(super) fn reasons(
        &mut self,
        snapshot: &mut Snapshot,
        candidates: &[SummaryRow],
    ) -> Result<HashMap<String, Vec<Blocker>>, HiveError> {
        if self.areas.is_none() {
            let rows = snapshot.records::<(String, Option<String>, String, bool)>(
                "areas",
                "id, parent_id, slug, archived_at IS NOT NULL",
                "id",
            )?;
            self.areas = Some(
                rows.into_iter()
                    .map(|(id, parent, slug, archived)| {
                        (
                            id,
                            Area {
                                parent,
                                slug,
                                archived,
                            },
                        )
                    })
                    .collect(),
            );
        }

        let ids: Vec<_> = candidates
            .iter()
            .map(|row| SqlValue::from(&row.0))
            .collect();
        let placeholders = vec!["?"; ids.len()].join(", ");
        let mut metadata_params = vec![SqlValue::from(NON_WHITESPACE)];
        metadata_params.extend(ids.iter().cloned());
        let metadata: Vec<(String, bool, bool, bool)> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT i.id, i.hold_reason IS NOT NULL, c.issue_id IS NOT NULL,
                    i.closed_at IS NULL AND i.closed_by_ref IS NULL AND i.resolution_note IS NULL
                        AND i.revision REGEXP ?
                 FROM issues AS OF '{commit}' AS i
                 LEFT JOIN issue_claims AS OF '{commit}' AS c ON c.issue_id = i.id
                 WHERE i.id IN ({placeholders})"
                )
            },
            metadata_params,
        )?;
        let mut reasons: HashMap<_, _> = metadata
            .into_iter()
            .map(|(id, hold, claimed, valid_metadata)| {
                let mut reasons = Vec::new();
                if hold {
                    reasons.push(Blocker::Hold);
                }
                if claimed {
                    reasons.push(Blocker::CurrentClaim);
                }
                if !valid_metadata {
                    reasons.push(Blocker::InvalidIssueMetadata);
                }
                (id, reasons)
            })
            .collect();
        // This Readiness instance serves the whole page, including lookahead batches.
        let remaining = MAX_ROWS_PER_TABLE - self.prerequisites_read;
        let mut prerequisite_params = vec![SqlValue::from(NON_WHITESPACE); 3];
        prerequisite_params.extend(ids);
        prerequisite_params.push(SqlValue::from(remaining as u64 + 1));
        let prerequisites: Vec<PrerequisiteRow> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT i.id, d.issue_id, d.prerequisite_id, p.type, p.status,
                         COALESCE(p.closed_at IS NOT NULL AND YEAR(p.closed_at) > 0
                             AND p.closed_by_ref REGEXP ?
                             AND p.resolution_note REGEXP ?
                             AND p.revision REGEXP ?
                             AND c.issue_id IS NULL, FALSE)
                     FROM issue_dependencies AS OF '{commit}' AS d
                     JOIN issues AS OF '{commit}' AS i ON i.id = d.issue_id
                     LEFT JOIN issues AS OF '{commit}' AS p
                        ON p.id = d.prerequisite_id
                        AND CAST(p.id AS BINARY) = CAST(d.prerequisite_id AS BINARY)
                     LEFT JOIN issue_claims AS OF '{commit}' AS c ON c.issue_id = p.id
                     WHERE i.id IN ({placeholders}) LIMIT ?"
                )
            },
            prerequisite_params,
        )?;
        if prerequisites.len() > remaining {
            return Err(HiveError::new(
                ErrorCode::ContextLimit,
                "The readiness query exceeds the prerequisite read limit.",
            ));
        }

        self.prerequisites_read += prerequisites.len();

        let mut unmet = HashSet::new();
        let mut invalid = HashSet::new();
        for (id, owner, prerequisite, kind, status, closure) in prerequisites {
            match status.as_deref().and_then(IssueStatus::parse) {
                Some(status)
                    if id == owner
                        && id != prerequisite
                        && is_uuid_v7(&prerequisite)
                        && kind.as_deref().is_some_and(is_issue_type)
                        && (status != IssueStatus::Done || closure) =>
                {
                    if status != IssueStatus::Done {
                        unmet.insert(id);
                    }
                }
                _ => {
                    invalid.insert(id);
                }
            }
        }

        for (id, _, area, ..) in candidates {
            let blockers = reasons.get_mut(id).ok_or_else(|| {
                HiveError::new(
                    ErrorCode::InvalidRecord,
                    "Committed Issue metadata is missing.",
                )
            })?;
            if unmet.contains(id) {
                blockers.push(Blocker::PrerequisiteNotDone);
            }
            if invalid.contains(id) {
                blockers.push(Blocker::InvalidPrerequisite);
            }
            let ancestry = self.area_ancestry(area);
            if ancestry.archived {
                blockers.push(Blocker::ArchivedArea);
            }
            if ancestry.invalid {
                blockers.push(Blocker::InvalidAreaAncestry);
            }
        }
        Ok(reasons)
    }

    fn area_ancestry(&mut self, id: &str) -> Ancestry {
        let areas = self.areas.as_ref().expect("areas loaded for readiness");
        let mut visited = HashMap::new();
        let mut path = Vec::<(&str, &Area)>::new();
        let mut current = id;
        let mut result = loop {
            if let Some(known) = self.ancestry.get(current) {
                break *known;
            }
            if let Some(&cycle_start) = visited.get(current) {
                let cycle = path.split_off(cycle_start);
                let result = Ancestry {
                    archived: cycle.iter().any(|(_, area)| area.archived),
                    invalid: true,
                };
                for (id, _) in cycle {
                    self.ancestry.insert(id.to_owned(), result);
                }
                break result;
            }
            let Some(area) = areas.get(current).filter(|_| is_uuid_v7(current)) else {
                break Ancestry {
                    archived: false,
                    invalid: true,
                };
            };
            visited.insert(current, path.len());
            path.push((current, area));
            match area.parent.as_deref() {
                Some(parent) => current = parent,
                None => break Ancestry::default(),
            }
        };

        for (id, area) in path.into_iter().rev() {
            result.archived |= area.archived;
            result.invalid |= !is_slug(&area.slug);
            self.ancestry.insert(id.to_owned(), result);
        }
        result
    }
}

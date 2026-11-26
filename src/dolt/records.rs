use std::collections::BTreeSet;

use crate::cli::is_slug;
use crate::error::{HiveError, RecordDiagnostic};

use super::{Snapshot, graph::Graph, is_uuid_v7};

const MAX_DIAGNOSTICS: usize = 100;

#[derive(Default)]
pub(crate) struct Diagnostics {
    pub(crate) items: Vec<RecordDiagnostic>,
    pub(crate) truncated: bool,
}

impl Diagnostics {
    fn add(
        &mut self,
        code: &'static str,
        table: &'static str,
        ids: &[&str],
        message: &'static str,
    ) {
        if self.items.len() == MAX_DIAGNOSTICS {
            self.truncated = true;
            return;
        }
        // Invalid ID values can contain arbitrary text. Report only verified identifiers.
        let record_ids = ids
            .iter()
            .filter(|id| is_uuid_v7(id))
            .map(|id| (*id).to_owned())
            .collect();
        self.items.push(RecordDiagnostic {
            code,
            severity: "error",
            table,
            record_ids,
            message,
        });
    }

    fn check_id(&mut self, table: &'static str, id: &str, owner: &[&str]) {
        if !is_uuid_v7(id) {
            self.add(
                "INVALID_ID",
                table,
                owner,
                "Use a canonical lowercase UUIDv7.",
            );
        }
    }
}

pub(super) fn validate(snapshot: &mut Snapshot) -> Result<Diagnostics, HiveError> {
    let mut diagnostics = Diagnostics::default();
    let areas: Vec<(String, Option<String>, String)> =
        snapshot.records("areas", "id, parent_id, slug", "id")?;
    let area_ids: BTreeSet<_> = areas.iter().map(|(id, ..)| id.as_str()).collect();
    let mut siblings = BTreeSet::new();
    let mut area_graph = Graph::default();
    for (id, parent, slug) in &areas {
        diagnostics.check_id("areas", id, &[]);
        if !is_slug(slug) {
            diagnostics.add(
                "INVALID_AREA_SLUG",
                "areas",
                &[id],
                "Use a lowercase kebab-case Area slug.",
            );
        }
        if !siblings.insert((parent.as_deref(), slug.as_str())) {
            diagnostics.add(
                "DUPLICATE_AREA_SLUG",
                "areas",
                &[id],
                "Sibling Areas must have unique slugs, including at the root.",
            );
        }
        if let Some(parent) = parent {
            if area_ids.contains(parent.as_str()) {
                area_graph.add(id, parent);
            } else {
                diagnostics.add(
                    "MISSING_REFERENCE",
                    "areas",
                    &[id, parent],
                    "The parent Area is missing.",
                );
            }
        }
    }
    if let Some(cycle) = area_graph.cycle() {
        diagnostics.add(
            "AREA_CYCLE",
            "areas",
            &cycle,
            "Area ancestry contains a cycle.",
        );
    }

    let issues: Vec<(String, String, Option<String>, u64, String, String)> = snapshot.records(
        "issues",
        "id, area_id, parent_id, number, type, status",
        "id",
    )?;
    let issue_ids: BTreeSet<_> = issues.iter().map(|(id, ..)| id.as_str()).collect();
    let claims: Vec<(String, String)> =
        snapshot.records("issue_claims", "issue_id, claim_id", "issue_id")?;
    let mut claimed_issues = BTreeSet::new();
    for (issue_id, claim_id) in &claims {
        diagnostics.check_id("issue_claims", claim_id, &[issue_id]);
        if !issue_ids.contains(issue_id.as_str()) {
            diagnostics.add(
                "MISSING_REFERENCE",
                "issue_claims",
                &[issue_id, claim_id],
                "The claimed Issue is missing.",
            );
        }
        if !claimed_issues.insert(issue_id.as_str()) {
            diagnostics.add(
                "INVALID_CLAIM_STATE",
                "issue_claims",
                &[issue_id, claim_id],
                "An Issue may have at most one active Claim.",
            );
        }
    }

    let mut numbers = BTreeSet::new();
    let mut parent_graph = Graph::default();
    for (id, area_id, parent, number, kind, status) in &issues {
        diagnostics.check_id("issues", id, &[]);
        if *number == 0 || !numbers.insert(*number) {
            diagnostics.add(
                "INVALID_ISSUE_NUMBER",
                "issues",
                &[id],
                "Issue numbers must be positive and unique within the workspace.",
            );
        }
        if !area_ids.contains(area_id.as_str()) {
            diagnostics.add(
                "MISSING_REFERENCE",
                "issues",
                &[id, area_id],
                "The owning Area is missing.",
            );
        }
        if let Some(parent) = parent {
            if issue_ids.contains(parent.as_str()) {
                // A parent must wait for each child to finish.
                parent_graph.add(parent, id);
            } else {
                diagnostics.add(
                    "MISSING_REFERENCE",
                    "issues",
                    &[id, parent],
                    "The parent Issue is missing.",
                );
            }
        }
        let claimed = claimed_issues.contains(id.as_str());
        if claimed != (status == "in-progress") || (kind == "epic" && status == "in-progress") {
            diagnostics.add(
                "INVALID_CLAIM_STATE",
                "issues",
                &[id],
                "An active Claim is required exactly when a non-epic Issue is in progress.",
            );
        }
    }

    let dependencies: Vec<(String, String)> = snapshot.records(
        "issue_dependencies",
        "issue_id, prerequisite_id",
        "issue_id, prerequisite_id",
    )?;
    let mut prerequisites = Graph::default();
    for (issue, prerequisite) in &dependencies {
        if issue_ids.contains(issue.as_str()) && issue_ids.contains(prerequisite.as_str()) {
            prerequisites.add(issue, prerequisite);
        } else {
            diagnostics.add(
                "MISSING_REFERENCE",
                "issue_dependencies",
                &[issue, prerequisite],
                "A dependency endpoint is missing.",
            );
        }
    }

    let parent_cycle = parent_graph.cycle();
    let prerequisite_cycle = prerequisites.cycle();
    if let Some(cycle) = &parent_cycle {
        diagnostics.add(
            "ISSUE_PARENT_CYCLE",
            "issues",
            cycle,
            "Issue parentage contains a cycle.",
        );
    }
    if let Some(cycle) = &prerequisite_cycle {
        diagnostics.add(
            "PREREQUISITE_CYCLE",
            "issue_dependencies",
            cycle,
            "Issue prerequisites contain a cycle.",
        );
    }
    if parent_cycle.is_none() && prerequisite_cycle.is_none() {
        parent_graph.extend(prerequisites);
        if let Some(cycle) = parent_graph.cycle() {
            diagnostics.add(
                "COMPLETION_CYCLE",
                "issues",
                &cycle,
                "The combined parent-to-child and prerequisite graph contains a cycle.",
            );
        }
    }
    Ok(diagnostics)
}

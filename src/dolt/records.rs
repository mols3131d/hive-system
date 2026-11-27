mod artifacts;
mod directives;
mod issue_updates;
mod receipts;
mod relations;

use std::collections::{BTreeSet, HashMap, VecDeque, hash_map::Entry};

use crate::cli::is_slug;
use crate::error::{HiveError, RecordDiagnostic};

use super::{Snapshot, graph::Graph, is_uuid, is_uuid_v7};

const MAX_DIAGNOSTICS: usize = 100;

struct AreaRow {
    id: String,
    parent_id: Option<String>,
    slug: String,
    archived: bool,
}

struct IssueRow {
    id: String,
    area_id: String,
    parent_id: Option<String>,
    number: u64,
    kind: String,
    status: Option<IssueStatus>,
    closure_fields_present: [bool; 3],
}

impl IssueRow {
    fn requires_terminal_children(&self) -> bool {
        self.status == Some(IssueStatus::Done)
            || (self.kind == "epic" && self.status == Some(IssueStatus::Review))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IssueStatus {
    Backlog,
    Todo,
    InProgress,
    Review,
    Done,
    Cancelled,
}

impl IssueStatus {
    fn parse(value: &str) -> Option<Self> {
        // SQL CHECK comparisons can accept other spellings under a case-insensitive collation.
        match value {
            "backlog" => Some(Self::Backlog),
            "todo" => Some(Self::Todo),
            "in-progress" => Some(Self::InProgress),
            "review" => Some(Self::Review),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    fn requires_done_prerequisites(self) -> bool {
        matches!(self, Self::InProgress | Self::Review | Self::Done)
    }
}

struct ClaimRow {
    issue_id: String,
    claim_id: String,
}

struct DependencyRow {
    issue_id: String,
    prerequisite_id: String,
}

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
        self.push(code, table, ids.iter().filter(|id| is_uuid_v7(id)), message);
    }

    fn add_uuid(
        &mut self,
        code: &'static str,
        table: &'static str,
        ids: &[&str],
        message: &'static str,
    ) {
        self.push(code, table, ids.iter().filter(|id| is_uuid(id)), message);
    }

    fn push<'a>(
        &mut self,
        code: &'static str,
        table: &'static str,
        ids: impl Iterator<Item = &'a &'a str>,
        message: &'static str,
    ) {
        if self.items.len() == MAX_DIAGNOSTICS {
            self.truncated = true;
            return;
        }
        // Invalid ID values can contain arbitrary text. Report only verified identifiers.
        let record_ids = ids.map(|id| (*id).to_owned()).collect();
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
    let areas: Vec<_> = snapshot
        .records::<(String, Option<String>, String, bool)>(
            "areas",
            "id, parent_id, slug, archived_at IS NOT NULL",
            "id",
        )?
        .into_iter()
        .map(|(id, parent_id, slug, archived)| AreaRow {
            id,
            parent_id,
            slug,
            archived,
        })
        .collect();
    let issues: Vec<_> = snapshot
        .records::<(
            String,
            String,
            Option<String>,
            u64,
            String,
            String,
            bool,
            bool,
            bool,
        )>(
            "issues",
            "id, area_id, parent_id, number, type, status, closed_at IS NOT NULL,
             closed_by_ref IS NOT NULL, resolution_note IS NOT NULL",
            "id",
        )?
        .into_iter()
        .map(
            |(id, area_id, parent_id, number, kind, status, closed_at, closed_by, resolution)| {
                IssueRow {
                    id,
                    area_id,
                    parent_id,
                    number,
                    kind,
                    status: IssueStatus::parse(&status),
                    closure_fields_present: [closed_at, closed_by, resolution],
                }
            },
        )
        .collect();
    let claims: Vec<_> = snapshot
        .records("issue_claims", "issue_id, claim_id", "issue_id, claim_id")?
        .into_iter()
        .map(|(issue_id, claim_id)| ClaimRow { issue_id, claim_id })
        .collect();
    let dependencies: Vec<_> = snapshot
        .records(
            "issue_dependencies",
            "issue_id, prerequisite_id",
            "issue_id, prerequisite_id",
        )?
        .into_iter()
        .map(|(issue_id, prerequisite_id)| DependencyRow {
            issue_id,
            prerequisite_id,
        })
        .collect();
    let counters: Vec<(String, u64)> = snapshot.rows(
        "workspace_counters",
        "name, next_value",
        "WHERE name = 'issue_number' LIMIT 2",
    )?;
    let highest_number = issues.iter().map(|issue| issue.number).max().unwrap_or(0);
    let mut diagnostics = validate_rows(&areas, &issues, &claims, &dependencies);
    // Compare the spelling explicitly because the counter table may use a case-insensitive collation.
    if !matches!(counters.as_slice(), [(name, next_value)] if name == "issue_number" && *next_value > highest_number)
    {
        diagnostics.add(
            "INVALID_ISSUE_COUNTER",
            "workspace_counters",
            &[],
            "The issue_number counter must exist and be greater than zero and every stored Issue number.",
        );
    }
    let area_ids = areas.iter().map(|area| area.id.as_str()).collect();
    let issue_ids = issues.iter().map(|issue| issue.id.as_str()).collect();
    relations::validate(snapshot, &area_ids, &issue_ids, &mut diagnostics)?;
    issue_updates::validate(snapshot, &issue_ids, &mut diagnostics)?;
    receipts::validate(snapshot, &mut diagnostics)?;
    Ok(diagnostics)
}

fn validate_rows(
    areas: &[AreaRow],
    issues: &[IssueRow],
    claims: &[ClaimRow],
    dependencies: &[DependencyRow],
) -> Diagnostics {
    let mut diagnostics = Diagnostics::default();
    let area_ids: BTreeSet<_> = areas.iter().map(|area| area.id.as_str()).collect();
    let mut siblings = BTreeSet::new();
    let mut area_graph = Graph::default();
    for AreaRow {
        id,
        parent_id,
        slug,
        ..
    } in areas
    {
        diagnostics.check_id("areas", id, &[]);
        if !is_slug(slug) {
            diagnostics.add(
                "INVALID_AREA_SLUG",
                "areas",
                &[id],
                "Use a lowercase kebab-case Area slug.",
            );
        }
        if !siblings.insert((parent_id.as_deref(), slug.as_str())) {
            diagnostics.add(
                "DUPLICATE_AREA_SLUG",
                "areas",
                &[id],
                "Sibling Areas must have unique slugs, including at the root.",
            );
        }
        if let Some(parent) = parent_id {
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

    let archived_by = archived_ancestry(areas);
    let issues_by_id: HashMap<_, _> = issues
        .iter()
        .map(|issue| (issue.id.as_str(), issue))
        .collect();
    let mut claimed_issues = BTreeSet::new();
    for ClaimRow { issue_id, claim_id } in claims {
        diagnostics.check_id("issue_claims", claim_id, &[issue_id]);
        if let Some(issue) = issues_by_id.get(issue_id.as_str()) {
            if let Some(archived_area) = archived_by.get(issue.area_id.as_str()) {
                diagnostics.add(
                    "ARCHIVED_AREA_CLAIM",
                    "issue_claims",
                    &[issue_id, claim_id, archived_area],
                    "An active Claim cannot be in an archived Area or its descendants.",
                );
            }
        } else {
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
    for issue in issues {
        let IssueRow {
            id,
            area_id,
            parent_id,
            number,
            kind,
            status,
            closure_fields_present,
        } = issue;
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
        if let Some(parent_id) = parent_id {
            if let Some(parent) = issues_by_id.get(parent_id.as_str()) {
                // Parent completion depends on each child reaching a terminal state.
                parent_graph.add(parent_id, id);
                // Acceptance of a cancelled child's scope change requires transition evidence.
                if parent.requires_terminal_children()
                    && status.is_some_and(|status| !status.is_terminal())
                {
                    diagnostics.add(
                        "UNRESOLVED_CHILD",
                        "issues",
                        &[parent_id, id],
                        "A done Issue or an epic in review requires terminal children.",
                    );
                }
            } else {
                diagnostics.add(
                    "MISSING_REFERENCE",
                    "issues",
                    &[id, parent_id],
                    "The parent Issue is missing.",
                );
            }
        }
        // SQL CHECK comparisons can accept other spellings under a case-insensitive collation.
        if !matches!(kind.as_str(), "epic" | "feature" | "task" | "bug") {
            diagnostics.add(
                "INVALID_ISSUE_TYPE",
                "issues",
                &[id],
                "Use a canonical lowercase Issue type.",
            );
        }
        let Some(status) = status else {
            diagnostics.add(
                "INVALID_ISSUE_STATUS",
                "issues",
                &[id],
                "Use a canonical lowercase Issue status.",
            );
            continue;
        };
        if *closure_fields_present != [status.is_terminal(); 3] {
            diagnostics.add(
                "INVALID_ISSUE_CLOSURE",
                "issues",
                &[id],
                "Done and cancelled Issues require all closure fields. Other statuses require them to be unset.",
            );
        }
        let in_progress = *status == IssueStatus::InProgress;
        let claimed = claimed_issues.contains(id.as_str());
        if claimed != in_progress || (kind == "epic" && in_progress) {
            diagnostics.add(
                "INVALID_CLAIM_STATE",
                "issues",
                &[id],
                "An active Claim is required exactly when a non-epic Issue is in progress.",
            );
        }
    }

    let mut prerequisites = Graph::default();
    for DependencyRow {
        issue_id,
        prerequisite_id,
    } in dependencies
    {
        if let (Some(issue), Some(prerequisite)) = (
            issues_by_id.get(issue_id.as_str()),
            issues_by_id.get(prerequisite_id.as_str()),
        ) {
            prerequisites.add(issue_id, prerequisite_id);
            if issue
                .status
                .is_some_and(IssueStatus::requires_done_prerequisites)
                && prerequisite
                    .status
                    .is_some_and(|status| status != IssueStatus::Done)
            {
                diagnostics.add(
                    "UNMET_PREREQUISITE",
                    "issue_dependencies",
                    &[issue_id, prerequisite_id],
                    "An Issue in progress, in review, or done requires done prerequisites.",
                );
            }
        } else {
            diagnostics.add(
                "MISSING_REFERENCE",
                "issue_dependencies",
                &[issue_id, prerequisite_id],
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
    diagnostics
}

fn archived_ancestry(areas: &[AreaRow]) -> HashMap<&str, &str> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut archived_by = HashMap::new();
    let mut pending = VecDeque::new();
    for area in areas {
        let id = area.id.as_str();
        if let Some(parent) = area.parent_id.as_deref() {
            children.entry(parent).or_default().push(id);
        }
        if area.archived {
            archived_by.insert(id, id);
            pending.push_back((id, id));
        }
    }
    // Start at every archived Area so each descendant is assigned its nearest archived ancestor.
    // Visiting an Area once also bounds traversal when the snapshot contains a cycle.
    while let Some((id, ancestor)) = pending.pop_front() {
        for &child in children.get(id).into_iter().flatten() {
            if let Entry::Vacant(entry) = archived_by.entry(child) {
                entry.insert(ancestor);
                pending.push_back((child, ancestor));
            }
        }
    }
    archived_by
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_issue_closure_fields_to_match_status() {
        let area = AreaRow {
            id: "01900000-0000-7000-8000-000000000001".to_owned(),
            parent_id: None,
            slug: "fixture-area".to_owned(),
            archived: false,
        };
        let mut issue = IssueRow {
            id: "01900000-0000-7000-8000-000000000002".to_owned(),
            area_id: area.id.clone(),
            parent_id: None,
            number: 1,
            kind: "task".to_owned(),
            status: None,
            closure_fields_present: [false; 3],
        };
        for (status, fields, expected) in [
            (IssueStatus::Done, [true; 3], None),
            (
                IssueStatus::Done,
                [true, true, false],
                Some("INVALID_ISSUE_CLOSURE"),
            ),
            (IssueStatus::Todo, [false; 3], None),
            (
                IssueStatus::Todo,
                [true, false, false],
                Some("INVALID_ISSUE_CLOSURE"),
            ),
        ] {
            issue.status = Some(status);
            issue.closure_fields_present = fields;
            let diagnostics = validate_rows(
                std::slice::from_ref(&area),
                std::slice::from_ref(&issue),
                &[],
                &[],
            );
            let codes: Vec<_> = diagnostics.items.iter().map(|item| item.code).collect();
            assert_eq!(codes, expected.as_slice());
        }
    }
}

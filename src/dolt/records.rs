mod artifacts;
pub(super) use artifacts::is_kind as is_artifact_kind;
mod bots;
mod directives;
mod issue_updates;
mod receipts;
mod relations;
pub(crate) use relations::is_canonical_label_name;

use std::collections::{BTreeSet, HashMap, VecDeque, hash_map::Entry};

use mysql::prelude::FromRow;
use serde::de::DeserializeOwned;

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

pub(super) fn is_issue_type(value: &str) -> bool {
    matches!(value, "epic" | "feature" | "task" | "bug")
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum IssueStatus {
    Backlog,
    Todo,
    InProgress,
    Review,
    Done,
    Cancelled,
}

impl IssueStatus {
    pub(super) fn parse(value: &str) -> Option<Self> {
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

    pub(super) fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    pub(super) fn requires_done_prerequisites(self) -> bool {
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
    pub(super) fn add(
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

    pub(super) fn check_id(&mut self, table: &'static str, id: &str, owner: &[&str]) {
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

pub(super) enum RecordReader<'a> {
    Online(&'a mut Snapshot),
    #[cfg(target_os = "linux")]
    Historical {
        connection: &'a mut mysql::Conn,
        commit: &'a str,
    },
    #[cfg(target_os = "linux")]
    Offline {
        installed: &'a crate::workspace_init::Installed<'a>,
        db: &'a std::path::Path,
        commit: &'a str,
    },
}

impl RecordReader<'_> {
    pub(super) fn records<T: FromRow + DeserializeOwned>(
        &mut self,
        table: &'static str,
        columns: &'static str,
        order: &'static str,
    ) -> Result<Vec<T>, HiveError> {
        match self {
            Self::Online(snapshot) => snapshot.records(table, columns, order),
            #[cfg(target_os = "linux")]
            Self::Offline { .. } | Self::Historical { .. } => {
                let limit = super::MAX_ROWS_PER_TABLE;
                let rows = self.rows(
                    table,
                    columns,
                    &format!("ORDER BY {order} LIMIT {}", limit + 1),
                )?;
                if rows.len() > limit {
                    return Err(HiveError::new(
                        crate::error::ErrorCode::ContextLimit,
                        format!("Validation exceeds the limit of {limit} rows in {table}."),
                    ));
                }
                Ok(rows)
            }
        }
    }

    fn rows<T: FromRow + DeserializeOwned>(
        &mut self,
        table: &'static str,
        columns: &'static str,
        suffix: &str,
    ) -> Result<Vec<T>, HiveError> {
        match self {
            Self::Online(snapshot) => snapshot.rows(table, columns, suffix),
            #[cfg(target_os = "linux")]
            Self::Historical { connection, commit } => {
                if !super::is_commit_hash(commit) {
                    return Err(super::context_workflow::unverified());
                }
                super::query_rows(
                    connection,
                    format!("SELECT {columns} FROM {table} AS OF '{commit}' {suffix}"),
                )
            }
            #[cfg(target_os = "linux")]
            Self::Offline { installed, db, commit } => installed
                .query(
                    db,
                    &format!("SELECT JSON_ARRAY({columns}) AS record FROM {table} AS OF '{commit}' {suffix}"),
                )?
                .into_iter()
                .map(|mut row| {
                    serde_json::from_value(row["record"].take()).map_err(|_| {
                        super::incompatible_schema(format!(
                            "The restored {table} table returned unexpected column types."
                        ))
                    })
                })
                .collect(),
        }
    }
}

struct WorkGraph {
    areas: Vec<AreaRow>,
    issues: Vec<IssueRow>,
    claims: Vec<ClaimRow>,
    dependencies: Vec<DependencyRow>,
}

fn read_work_graph(snapshot: &mut RecordReader<'_>) -> Result<WorkGraph, HiveError> {
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
            String,
            String,
            String,
            bool,
            bool,
            bool,
        )>(
            "issues",
            "id, area_id, parent_id, CAST(number AS CHAR), type, status, closed_at IS NOT NULL,
             closed_by_ref IS NOT NULL, resolution_note IS NOT NULL",
            "id",
        )?
        .into_iter()
        .map(
            |(id, area_id, parent_id, number, kind, status, closed_at, closed_by, resolution)| {
                Ok(IssueRow {
                    id,
                    area_id,
                    parent_id,
                    number: parse_unsigned(&number, "issues.number")?,
                    kind,
                    status: IssueStatus::parse(&status),
                    closure_fields_present: [closed_at, closed_by, resolution],
                })
            },
        )
        .collect::<Result<Vec<_>, HiveError>>()?;
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
    Ok(WorkGraph {
        areas,
        issues,
        claims,
        dependencies,
    })
}

#[cfg(target_os = "linux")]
impl WorkGraph {
    fn require_valid(&self) -> Result<(), HiveError> {
        use crate::error::ErrorCode;

        if !validate_rows(&self.areas, &self.issues, &self.claims, &self.dependencies)
            .items
            .is_empty()
        {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "The stored Issue graph or lifecycle state is invalid.",
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub(super) fn check_dependency_edit(
    reader: &mut RecordReader<'_>,
    issue: &str,
    prerequisite: &str,
    add: bool,
) -> Result<(), HiveError> {
    use crate::error::ErrorCode;

    let mut graph = read_work_graph(reader)?;
    graph.require_valid()?;
    let edge = graph
        .dependencies
        .iter()
        .position(|edge| edge.issue_id == issue && edge.prerequisite_id == prerequisite);
    match (add, edge) {
        (true, None) => {
            if graph.dependencies.len() == super::MAX_ROWS_PER_TABLE {
                return Err(HiveError::new(
                    ErrorCode::ContextLimit,
                    "Adding the dependency exceeds the Issue graph limit.",
                ));
            }
            graph.dependencies.push(DependencyRow {
                issue_id: issue.to_owned(),
                prerequisite_id: prerequisite.to_owned(),
            });
            // The current graph is valid. The added edge can introduce an unmet
            // prerequisite or a cycle in the prerequisite or completion graph.
            if graph.require_valid().is_err() {
                return Err(HiveError::new(
                    ErrorCode::InvalidTransition,
                    "The dependency violates the Issue prerequisite or completion graph.",
                ));
            }
        }
        (false, Some(_)) => {
            // Removing an edge preserves acyclicity and every remaining obligation.
        }
        (true, Some(_)) => {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "The dependency already exists.",
            ));
        }
        (false, None) => {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "The dependency does not exist.",
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn check_parent_edit(
    reader: &mut RecordReader<'_>,
    issue_id: &str,
    old_parent: Option<&str>,
    new_parent: Option<&str>,
) -> Result<(), HiveError> {
    use crate::error::ErrorCode;

    let mut graph = read_work_graph(reader)?;
    graph.require_valid()?;
    let index = graph
        .issues
        .iter()
        .position(|row| row.id == issue_id)
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidRecord,
                "The Issue is absent from the parent graph.",
            )
        })?;
    if graph.issues[index].parent_id.as_deref() != old_parent {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "The expected parent relation is stale.",
        ));
    }
    let protects_children = |id: &str| {
        graph.issues.iter().any(|row| {
            row.id == id && matches!(row.status, Some(IssueStatus::Review | IssueStatus::Done))
        })
    };
    let invalid_transition = |message| HiveError::new(ErrorCode::InvalidTransition, message);
    if old_parent.is_some_and(protects_children) {
        return Err(invalid_transition(
            "Changing the child scope of an existing review or accepted parent requires reconciliation.",
        ));
    }
    let protected_new_parent = new_parent.is_some_and(protects_children);
    let issue = &mut graph.issues[index];
    if protected_new_parent && issue.status != Some(IssueStatus::Done) {
        // A planning decision cannot accept a cancelled child's changed scope.
        return Err(invalid_transition(
            "A parent in review or done requires an accepted child.",
        ));
    }
    issue.parent_id = new_parent.map(str::to_owned);
    if graph.require_valid().is_err() {
        return Err(invalid_transition(
            "The parent edit violates the Issue hierarchy or completion graph.",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn check_issue_reconciliation(
    reader: &mut RecordReader<'_>,
    issue_id: &str,
    reopening: bool,
) -> Result<(), HiveError> {
    use crate::error::ErrorCode;

    let graph = read_work_graph(reader)?;
    graph.require_valid()?;
    let issue = graph
        .issues
        .iter()
        .find(|row| row.id == issue_id)
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidRecord,
                "The Issue is absent from the work graph.",
            )
        })?;
    let dependents: BTreeSet<_> = graph
        .dependencies
        .iter()
        .filter(|edge| reopening && edge.prerequisite_id == issue_id)
        .map(|edge| edge.issue_id.as_str())
        .collect();
    for row in &graph.issues {
        if issue.parent_id.as_deref() == Some(row.id.as_str())
            && matches!(row.status, Some(IssueStatus::Review | IssueStatus::Done))
        {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "The parent Issue in review or done requires prior reconciliation.",
            ));
        }
        if dependents.contains(row.id.as_str())
            && row
                .status
                .is_some_and(IssueStatus::requires_done_prerequisites)
        {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "A dependent Issue in progress, review or done requires prior reconciliation.",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate(snapshot: &mut RecordReader<'_>) -> Result<Diagnostics, HiveError> {
    let WorkGraph {
        areas,
        issues,
        claims,
        dependencies,
    } = read_work_graph(snapshot)?;
    let counters = snapshot
        .rows::<(String, String)>(
            "workspace_counters",
            "name, CAST(next_value AS CHAR)",
            "WHERE name = 'issue_number' LIMIT 2",
        )?
        .into_iter()
        .map(|(name, value)| {
            Ok((
                name,
                parse_unsigned(&value, "workspace_counters.next_value")?,
            ))
        })
        .collect::<Result<Vec<_>, HiveError>>()?;
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
    bots::validate(snapshot, &mut diagnostics)?;
    receipts::validate(snapshot, &mut diagnostics)?;
    Ok(diagnostics)
}

fn parse_unsigned(value: &str, column: &str) -> Result<u64, HiveError> {
    // Decimal text avoids the floating-point conversion in Dolt's JSON_ARRAY.
    value.parse().map_err(|_| {
        super::incompatible_schema(format!(
            "The {column} column returned an invalid unsigned integer."
        ))
    })
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
        if !is_issue_type(kind) {
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

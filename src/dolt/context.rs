//! Draft context inputs. Workflow scope and role, Soul and Directive provenance
//! must be verified before disclosing these inputs. Snapshot authentication
//! establishes the database selection, not the actor's workflow authority.

use std::collections::HashSet;

use super::{
    IssueDetail, IssueMetaRow, MAX_INLINE_BYTES, load_issue_detail, load_issue_meta,
    parse_selector, validate_issue,
};
use crate::dolt::{
    Snapshot,
    area::select_area,
    is_uuid_v7,
    issue_readiness::NON_WHITESPACE,
    project::is_project_status,
    records::{Diagnostics, IssueStatus, is_issue_type},
};
use crate::error::{ErrorCode, HiveError};

const DEFAULT_INLINE_BYTES: usize = 32 * 1024;

const ANCESTRY_LIMIT: usize = 32;
const REFERENCE_LIMIT: usize = 25;
const PREREQUISITE_LIMIT: usize = 1_000;
const UPDATE_ORDER: &str = "created_at DESC, CAST(id AS BINARY) ASC";

#[derive(Debug, PartialEq)]
struct AreaRef {
    id: String,
    revision: String,
    archived: bool,
}

#[derive(Debug, PartialEq)]
struct ProjectSummary {
    id: String,
    name: String,
    status: String,
    revision: String,
}

#[derive(Debug)]
struct Scope {
    db_commit: String,
    // Exact Area first. Ancestors support Directive applicability and archive
    // checks and confer no additional disclosure scope.
    ancestry: Vec<AreaRef>,
    project: Option<ProjectSummary>,
    issue: Option<IssueMetaRow>,
}

fn resolve_scope(
    snapshot: &mut Snapshot,
    area: Option<&str>,
    project: Option<&str>,
    issue: Option<&str>,
) -> Result<Scope, HiveError> {
    if [area, project]
        .into_iter()
        .flatten()
        .any(|id| !is_uuid_v7(id))
    {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply canonical Area and Project IDs.",
        ));
    }
    if area.is_none() && project.is_none() && issue.is_none() {
        return Err(HiveError::new(
            ErrorCode::AmbiguousScope,
            "Select an Area, Project, or Issue explicitly.",
        ));
    }

    let issue = issue
        .map(|selector| load_issue_meta(snapshot, &parse_selector(selector)?))
        .transpose()?;
    if let Some((id, _, owner, ..)) = &issue {
        if !is_uuid_v7(id) || !is_uuid_v7(owner) {
            return Err(invalid_record(
                "The selected Issue has an invalid ID or owning Area.",
            ));
        }
        if area.is_some_and(|area| area != owner) {
            return Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "The selected Area must match the Issue's owning Area.",
            ));
        }
    }
    let ancestry = load_ancestry(
        snapshot,
        issue.as_ref().map(|row| row.2.as_str()).or(area),
        issue.is_some(),
    )?;
    let project = project
        .map(|id| load_project(snapshot, id, issue.as_ref().map(|row| row.0.as_str())))
        .transpose()?;
    Ok(Scope {
        db_commit: snapshot.commit.clone(),
        ancestry,
        project,
        issue,
    })
}

fn load_ancestry(
    snapshot: &mut Snapshot,
    area: Option<&str>,
    mut required_endpoint: bool,
) -> Result<Vec<AreaRef>, HiveError> {
    let mut ancestry = Vec::new();
    let mut seen = HashSet::new();
    let mut current = area.map(str::to_owned);
    while let Some(id) = current {
        if !is_uuid_v7(&id) || !seen.insert(id.clone()) {
            return Err(invalid_record(
                "The selected Area ancestry is invalid or cyclic.",
            ));
        }
        if ancestry.len() == ANCESTRY_LIMIT {
            return Err(limit("The selected Area ancestry exceeds 32 levels."));
        }
        let (parent, revision, archived, slug): (Option<String>, String, bool, String) =
            select_area(
                snapshot,
                "parent_id, revision, archived_at IS NOT NULL, slug",
                &id,
            )
            .map_err(|error| {
                if required_endpoint && matches!(error.code, ErrorCode::NotFound) {
                    invalid_record("A required Area in the selected ancestry is missing.")
                } else {
                    error
                }
            })?;
        if revision.trim().is_empty() || !crate::cli::is_slug(&slug) {
            return Err(invalid_record(
                "The selected Area ancestry has invalid metadata.",
            ));
        }
        ancestry.push(AreaRef {
            id,
            revision,
            archived,
        });
        required_endpoint = true;
        current = parent;
    }
    Ok(ancestry)
}

fn load_project(
    snapshot: &mut Snapshot,
    id: &str,
    issue: Option<&str>,
) -> Result<ProjectSummary, HiveError> {
    let mut rows: Vec<(String, String, String, bool)> = snapshot.exec_rows(
        |commit| format!(
            "SELECT name, status, revision, EXISTS(SELECT 1 FROM project_issues AS OF '{commit}' AS m
             WHERE m.project_id = ? AND CAST(m.project_id AS BINARY) = ?
               AND m.issue_id = ? AND CAST(m.issue_id AS BINARY) = ?)
             FROM projects AS OF '{commit}' WHERE id = ? AND CAST(id AS BINARY) = ? LIMIT 2"
        ),
        (id, id, issue, issue, id, id),
    )?;
    if rows.len() > 1 {
        return Err(invalid_record("The selected Project ID is ambiguous."));
    }
    let (name, status, revision, member) = rows
        .pop()
        .ok_or_else(|| HiveError::new(ErrorCode::NotFound, "The selected Project is missing."))?;
    if !is_project_status(&status) || revision.trim().is_empty() {
        return Err(invalid_record("The selected Project has invalid metadata."));
    }
    if issue.is_some() && !member {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "The selected Issue must be an exact member of the selected Project.",
        ));
    }
    Ok(ProjectSummary {
        id: id.to_owned(),
        name,
        status,
        revision,
    })
}

#[derive(Debug, PartialEq)]
struct UpdateRef {
    id: String,
    kind: String,
    created_at: String,
}

struct Work {
    issue: IssueDetail,
    inline_bytes: usize,
    prerequisites: Vec<String>,
    omitted_prerequisites: usize,
    prerequisite_blocked: bool,
    claimed: bool,
    archived_ancestry: bool,
    handoff: Option<UpdateRef>,
    updates: Vec<UpdateRef>,
    omitted_updates: u64,
}

impl Work {
    fn require_handoff_provenance(&self) -> Result<(), HiveError> {
        if self.handoff.is_some() {
            return Err(HiveError::new(
                ErrorCode::AuthorityUnverified,
                "Current handoff body provenance is unavailable. Use an authorized exact handoff retrieval path.",
            ));
        }
        Ok(())
    }
}

// Budget is the remaining allowance after role, Soul and Directives.
fn collect_work(
    snapshot: &mut Snapshot,
    scope: &Scope,
    budget: Option<usize>,
) -> Result<Option<Work>, HiveError> {
    if snapshot.commit != scope.db_commit {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "Resolve scope and work records from the same committed snapshot.",
        ));
    }
    let budget = budget.unwrap_or(DEFAULT_INLINE_BYTES);
    if budget > MAX_INLINE_BYTES {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "The context inline budget cannot exceed 64 KiB.",
        ));
    }
    let Some(meta) = &scope.issue else {
        return Ok(None);
    };
    let mut issue = load_issue_detail(snapshot, meta.clone(), budget)?;
    let mut diagnostics = Diagnostics::default();
    validate_issue(&issue, &mut diagnostics);
    let claimed = super::load_claim_id(snapshot, &issue, &mut diagnostics)?.is_some();
    if !diagnostics.items.is_empty() || issue.revision.trim().is_empty() {
        return Err(invalid_record(
            "The selected Issue or its current Claim is invalid.",
        ));
    }
    let inline_bytes = issue.inline_bytes();
    let (prerequisites, omitted_prerequisites, prerequisite_blocked) =
        load_prerequisites(snapshot, &issue)?;
    if let Some(id) = &issue.parent_id {
        let rows: Vec<(String,)> = snapshot.exec_rows(
            |commit| format!("SELECT area_id FROM issues AS OF '{commit}' WHERE id = ? AND CAST(id AS BINARY) = ? LIMIT 2"),
            (id, id),
        )?;
        let [(area,)] = rows.as_slice() else {
            return Err(invalid_record(
                "The selected Issue has an invalid parent reference.",
            ));
        };
        if !is_uuid_v7(area) {
            return Err(invalid_record(
                "The selected Issue has an invalid parent Area.",
            ));
        }
        if area != &issue.area_id {
            issue.parent_id = None;
        }
    }
    let (handoff, _) = load_updates(snapshot, &issue.id, true)?;
    let (updates, omitted_updates) = load_updates(snapshot, &issue.id, false)?;
    // Closure author is unnecessary for resume.
    issue.closed_by_ref = None;
    Ok(Some(Work {
        issue,
        inline_bytes,
        prerequisites,
        omitted_prerequisites,
        prerequisite_blocked,
        claimed,
        archived_ancestry: scope.ancestry.iter().any(|area| area.archived),
        handoff: handoff.into_iter().next(),
        updates,
        omitted_updates,
    }))
}

type PrerequisiteRow = (String, Option<String>, Option<String>, Option<String>, bool);

fn load_prerequisites(
    snapshot: &mut Snapshot,
    issue: &IssueDetail,
) -> Result<(Vec<String>, usize, bool), HiveError> {
    let rows: Vec<PrerequisiteRow> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT d.prerequisite_id, p.area_id, p.type, p.status,
               COALESCE(p.closed_at IS NOT NULL AND YEAR(p.closed_at) > 0
                 AND p.closed_by_ref REGEXP ? AND p.resolution_note REGEXP ?
                 AND p.revision REGEXP ? AND c.issue_id IS NULL, FALSE)
             FROM issue_dependencies AS OF '{commit}' AS d
             LEFT JOIN issues AS OF '{commit}' AS p ON p.id = d.prerequisite_id
               AND CAST(p.id AS BINARY) = CAST(d.prerequisite_id AS BINARY)
             LEFT JOIN issue_claims AS OF '{commit}' AS c ON c.issue_id = p.id
             WHERE d.issue_id = ? AND CAST(d.issue_id AS BINARY) = ?
             ORDER BY CAST(d.prerequisite_id AS BINARY) LIMIT {}",
                PREREQUISITE_LIMIT + 1
            )
        },
        (
            NON_WHITESPACE,
            NON_WHITESPACE,
            NON_WHITESPACE,
            &issue.id,
            &issue.id,
        ),
    )?;
    summarize_prerequisites(rows, &issue.id, &issue.area_id)
}

fn summarize_prerequisites(
    rows: Vec<PrerequisiteRow>,
    issue_id: &str,
    area_id: &str,
) -> Result<(Vec<String>, usize, bool), HiveError> {
    if rows.len() > PREREQUISITE_LIMIT {
        return Err(limit(
            "The selected Issue exceeds the prerequisite evaluation limit.",
        ));
    }
    let mut refs = Vec::new();
    let mut omitted = 0;
    let mut blocked = false;
    for (id, area, kind, status, valid_closure) in rows {
        if id == issue_id
            || !is_uuid_v7(&id)
            || !area.as_deref().is_some_and(is_uuid_v7)
            || !kind.as_deref().is_some_and(is_issue_type)
        {
            return Err(invalid_record(
                "The selected Issue has an invalid prerequisite.",
            ));
        }
        match status.as_deref().and_then(IssueStatus::parse) {
            Some(IssueStatus::Done) if valid_closure => {}
            Some(IssueStatus::Done) | None => {
                return Err(invalid_record(
                    "A prerequisite has invalid committed state.",
                ));
            }
            _ => blocked = true,
        }
        if area.as_deref() == Some(area_id) {
            if refs.len() == REFERENCE_LIMIT {
                omitted += 1;
            } else {
                refs.push(id);
            }
        }
    }
    Ok((refs, omitted, blocked))
}

fn load_updates(
    snapshot: &mut Snapshot,
    issue: &str,
    handoff: bool,
) -> Result<(Vec<UpdateRef>, u64), HiveError> {
    // Select exact Issue-owned references. Case variants are included by the
    // predicate and rejected below so an invalid newer handoff cannot be missed.
    let kinds = if handoff {
        "'handoff'"
    } else {
        "'progress', 'evidence'"
    };
    let rows: Vec<(String, String, String, bool, u64)> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT id, kind, DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'),
               transition_json IS NULL, COUNT(*) OVER ()
             FROM issue_updates AS OF '{commit}'
             WHERE issue_id = ? AND CAST(issue_id AS BINARY) = ? AND kind IN ({kinds})
             ORDER BY {UPDATE_ORDER} LIMIT ?"
            )
        },
        (issue, issue, if handoff { 1 } else { REFERENCE_LIMIT }),
    )?;
    let count = rows.first().map_or(0, |row| row.4);
    let omitted = count.saturating_sub(rows.len() as u64);
    let refs = rows
        .into_iter()
        .map(|(id, kind, created_at, no_transition, _)| {
            if !is_uuid_v7(&id)
                || !no_transition
                || !matches!(kind.as_str(), "handoff" | "progress" | "evidence")
            {
                return Err(invalid_record(
                    "A selected Issue update has invalid committed metadata.",
                ));
            }
            Ok(UpdateRef {
                id,
                kind,
                created_at,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok((refs, omitted))
}

fn invalid_record(message: &str) -> HiveError {
    HiveError::new(ErrorCode::InvalidRecord, message)
}

fn limit(message: &str) -> HiveError {
    HiveError::new(ErrorCode::ContextLimit, message)
}

#[path = "context/tests.rs"]
mod tests;

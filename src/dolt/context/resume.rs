//! Required handoff selection for a verified Issue.
//!
//! The latest Issue note uses committed creation time and an exact ID tie-break.
//! An active exact Issue-linked handoff Artifact is an alternative source. A
//! selection spanning both representations, or multiple Artifacts, is ambiguous
//! until the resolver has a reviewed explicit selection rule.

use super::workflow::{self, Expected, Operation, RecordKind, Request};
use super::{Scope, Work, invalid_record, limit};
use crate::dolt::{Snapshot, artifact::verify_context_handoff_links, is_uuid_v7};
use crate::error::{ErrorCode, HiveError};

pub(super) struct HandoffBody {
    pub(super) id: String,
    pub(super) revision: String,
    pub(super) body: String,
    pub(super) inline_bytes: usize,
}

pub(super) fn read_required(
    snapshot: &mut Snapshot,
    scope: &Scope,
    work: &Work,
    work_budget: usize,
    request: &Request<'_>,
) -> Result<Option<HandoffBody>, HiveError> {
    if snapshot.commit != scope.db_commit
        || snapshot.commit != work.db_commit
        || !scope
            .issue
            .as_ref()
            .is_some_and(|row| row.0 == work.issue.id)
    {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "Resolve the handoff from the same committed Issue context.",
        ));
    }
    let remaining = work_budget
        .checked_sub(work.inline_bytes)
        .filter(|_| work_budget <= super::MAX_INLINE_BYTES)
        .ok_or_else(|| limit("Required Issue text exceeds the remaining context budget."))?;

    let artifacts = artifact_candidates(snapshot, &work.issue.id)?;
    let candidate = match (work.handoff.as_ref(), artifacts.as_slice()) {
        (None, []) => return Ok(None),
        (Some(note), []) => (
            RecordKind::IssueUpdate,
            Operation::ReadHandoffBody,
            note.id.as_str(),
            snapshot.commit.as_str(),
        ),
        (None, [(id, revision)]) => (
            RecordKind::Artifact,
            Operation::DiscloseArtifactBody,
            id.as_str(),
            revision.as_str(),
        ),
        _ => {
            return Err(HiveError::new(
                ErrorCode::AmbiguousScope,
                "Multiple handoff sources are current candidates. An exact handoff selection is required.",
            ));
        }
    };

    let (kind, operation, id, revision) = candidate;
    workflow::verify(
        request,
        &Expected {
            workspace_id: &snapshot.workspace_id,
            db_commit: &snapshot.commit,
            area_id: &work.issue.area_id,
            issue_id: &work.issue.id,
            operation,
            record_kind: kind,
            record_id: id,
            record_revision: revision,
        },
    )?;
    let revision = revision.to_owned();

    let body = match kind {
        RecordKind::IssueUpdate => {
            let rows = snapshot.exec_rows(
                |commit| {
                    format!(
                        "SELECT author_ref, CASE WHEN OCTET_LENGTH(body) <= ? THEN body END
                     FROM issue_updates AS OF '{commit}'
                     WHERE id = ? AND CAST(id AS BINARY) = ?
                     AND CAST(issue_id AS BINARY) = ? AND CAST(kind AS BINARY) = 'handoff'
                     AND transition_json IS NULL LIMIT 2"
                    )
                },
                (remaining, id, id, &work.issue.id),
            )?;
            checked_body(rows, remaining)?
        }
        RecordKind::Artifact => {
            verify_context_handoff_links(
                snapshot,
                id,
                &work.issue.id,
                &work.issue.area_id,
                scope.project.as_ref().map(|project| project.id.as_str()),
            )?;
            let rows = snapshot.exec_rows(
                |commit| {
                    format!(
                        "SELECT author_ref, CASE WHEN OCTET_LENGTH(body) <= ? THEN body END
                     FROM artifacts AS OF '{commit}' WHERE id = ? AND CAST(id AS BINARY) = ?
                     AND CAST(revision AS BINARY) = ? AND CAST(kind AS BINARY) = 'handoff'
                     AND archived_at IS NULL AND archive_reason IS NULL LIMIT 2"
                    )
                },
                (remaining, id, id, &revision),
            )?;
            checked_body(rows, remaining)?
        }
    };
    Ok(Some(HandoffBody {
        id: id.to_owned(),
        revision,
        inline_bytes: body.len(),
        body,
    }))
}

type ArtifactCandidateRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    bool,
);

fn artifact_candidates(
    snapshot: &mut Snapshot,
    issue: &str,
) -> Result<Vec<(String, String)>, HiveError> {
    let rows: Vec<ArtifactCandidateRow> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT l.artifact_id, l.issue_id, a.id, a.kind, a.revision,
                    a.archived_at IS NOT NULL, a.archive_reason IS NOT NULL
                 FROM artifact_issues AS OF '{commit}' AS l
                 LEFT JOIN artifacts AS OF '{commit}' AS a ON a.id = l.artifact_id
                 WHERE (l.issue_id = ? OR CAST(LOWER(l.issue_id) AS BINARY) = ?)
                 AND (a.id IS NULL OR LOWER(a.kind) = 'handoff')
                 AND (a.archived_at IS NULL OR a.archive_reason IS NULL)
                 ORDER BY CAST(l.artifact_id AS BINARY) LIMIT 2"
            )
        },
        (issue, issue),
    )?;
    rows.into_iter()
        .map(|(id, owner, actual, kind, revision, archived, reason)| {
            let Some(revision) = revision.filter(|value| workflow::valid_ref(value)) else {
                return Err(invalid_record(
                    "An applicable handoff Artifact has invalid revision provenance.",
                ));
            };
            if owner != issue
                || !is_uuid_v7(&id)
                || actual.as_ref() != Some(&id)
                || kind.as_deref() != Some("handoff")
                || archived
                || reason
            {
                return Err(invalid_record(
                    "An applicable handoff Artifact has invalid committed scope or state.",
                ));
            }
            Ok((id, revision))
        })
        .collect()
}

fn checked_body(
    rows: Vec<(String, Option<String>)>,
    remaining: usize,
) -> Result<String, HiveError> {
    let Ok([(author, body)]) = <[_; 1]>::try_from(rows) else {
        return Err(invalid_record("The exact handoff record is unavailable."));
    };
    if !workflow::valid_ref(&author) {
        return Err(invalid_record("The handoff has invalid author provenance."));
    }
    let body = body
        .filter(|body| body.len() <= remaining)
        .ok_or_else(|| limit("The required handoff body exceeds the remaining context budget."))?;
    if body.trim().is_empty() {
        return Err(invalid_record("The current handoff body is empty."));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exact_utf8_boundary_and_rejects_missing_provenance_or_body() {
        let row = || vec![("fixture-author".to_owned(), Some("é".to_owned()))];
        assert_eq!(checked_body(row(), 2).unwrap(), "é");
        assert!(matches!(
            checked_body(row(), 1).unwrap_err().code,
            ErrorCode::ContextLimit
        ));
        assert!(matches!(
            checked_body(vec![(" ".to_owned(), None)], 2)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRecord
        ));
        assert!(matches!(
            checked_body(vec![("fixture-author".to_owned(), None)], 2)
                .unwrap_err()
                .code,
            ErrorCode::ContextLimit
        ));
    }
}

//! Required handoff selection for a verified Issue.
//!
//! The latest Issue note uses committed creation time and an exact ID tie-break.
//! An active exact Issue-linked handoff Artifact is an alternative source. A
//! supplied exact declaration selects among eligible candidates. Without one,
//! competing sources are ambiguous.

use super::workflow::{self, CurrentInput, Declaration, Expected, Operation, RecordKind, Request};
use super::{Scope, Work, invalid_record, limit};
use crate::dolt::{Snapshot, artifact::verify_context_artifact_links, is_uuid_v7};
use crate::error::{ErrorCode, HiveError};

pub(super) struct HandoffBody {
    pub(super) kind: RecordKind,
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

    let Some((kind, id, revision)) = select_handoff(snapshot, work, request.input)? else {
        return Ok(None);
    };
    let operation = match kind {
        RecordKind::IssueUpdate => Operation::ReadHandoffBody,
        RecordKind::Artifact => Operation::DiscloseArtifactBody,
    };
    workflow::verify(
        request,
        &Expected {
            workspace_id: &snapshot.workspace_id,
            db_commit: &snapshot.commit,
            area_id: &work.issue.area_id,
            issue_id: &work.issue.id,
            operation,
            record_kind: kind,
            record_id: &id,
            record_revision: &revision,
        },
    )?;
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
                (remaining, &id, &id, &work.issue.id),
            )?;
            checked_body(rows, remaining)
                .map_err(|error| error.context_limit_ref("issue_update", &id))?
        }
        RecordKind::Artifact => {
            read_artifact_body(snapshot, scope, work, &id, &revision, remaining)
                .map_err(|error| error.context_limit_ref("artifact", &id))?
        }
    };
    Ok(Some(HandoffBody {
        kind,
        id,
        revision,
        inline_bytes: body.len(),
        body,
    }))
}

fn select_handoff(
    snapshot: &mut Snapshot,
    work: &Work,
    input: Option<CurrentInput<'_>>,
) -> Result<Option<(RecordKind, String, String)>, HiveError> {
    let note = work.handoff.as_ref().map(|note| {
        (
            RecordKind::IssueUpdate,
            note.id.clone(),
            snapshot.commit.clone(),
        )
    });
    if let Some(input) = input {
        #[cfg(not(test))]
        let CurrentInput::OperatorRequest(bytes) = input;
        #[cfg(test)]
        let bytes = match input {
            CurrentInput::OperatorRequest(bytes) => bytes,
            CurrentInput::ResolvedSource { bytes, .. } => bytes,
        };
        // This only selects a candidate. Current authority is verified against
        // the chosen committed ID, scope and revision before reading any body.
        let declaration = Declaration::parse(bytes)?;
        let id = declaration.record.id.as_str();
        let selected = match declaration.record.kind {
            RecordKind::IssueUpdate => note.filter(|(_, latest, _)| latest == id),
            RecordKind::Artifact => artifact_candidates(snapshot, &work.issue.id, Some(id))?
                .into_iter()
                .next()
                .map(|(id, revision)| (RecordKind::Artifact, id, revision)),
        };
        return selected.map(Some).ok_or_else(|| {
            HiveError::new(
                ErrorCode::Unauthorized,
                "No eligible current handoff matches the supplied declaration.",
            )
        });
    }

    let artifacts = artifact_candidates(snapshot, &work.issue.id, None)?;
    match (note, artifacts.as_slice()) {
        (note, []) => Ok(note),
        (None, [(id, revision)]) => Ok(Some((RecordKind::Artifact, id.clone(), revision.clone()))),
        _ => Err(HiveError::new(
            ErrorCode::AmbiguousScope,
            "Multiple handoff sources are current candidates. An exact handoff selection is required.",
        )),
    }
}

pub(super) fn selected_artifact_revision(
    snapshot: &mut Snapshot,
    work: &Work,
    id: &str,
    request: &Request<'_>,
) -> Result<String, HiveError> {
    if snapshot.commit != work.db_commit {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "Select Artifacts from the same committed context.",
        ));
    }
    let rows: Vec<(String, String)> = snapshot.exec_rows(
        |commit| format!(
            "SELECT kind, revision FROM artifacts AS OF '{commit}'
             WHERE CAST(id AS BINARY) = ? AND archived_at IS NULL AND archive_reason IS NULL LIMIT 2"
        ),
        (id,),
    )?;
    let Ok([(kind, revision)]) = <[_; 1]>::try_from(rows) else {
        return Err(HiveError::new(
            ErrorCode::NotFound,
            "The selected active Artifact is unavailable.",
        ));
    };
    if !crate::dolt::records::is_artifact_kind(&kind) || !workflow::valid_ref(&revision) {
        return Err(invalid_record(
            "A selected Artifact has invalid committed metadata.",
        ));
    }
    workflow::verify(
        request,
        &Expected {
            workspace_id: &snapshot.workspace_id,
            db_commit: &snapshot.commit,
            area_id: &work.issue.area_id,
            issue_id: &work.issue.id,
            operation: Operation::DiscloseArtifactBody,
            record_kind: RecordKind::Artifact,
            record_id: id,
            record_revision: &revision,
        },
    )?;
    Ok(revision)
}

pub(super) fn read_artifact_body(
    snapshot: &mut Snapshot,
    scope: &Scope,
    work: &Work,
    id: &str,
    revision: &str,
    remaining: usize,
) -> Result<String, HiveError> {
    verify_context_artifact_links(
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
             FROM artifacts AS OF '{commit}'
             WHERE CAST(id AS BINARY) = ? AND CAST(revision AS BINARY) = ?
               AND archived_at IS NULL AND archive_reason IS NULL LIMIT 2"
            )
        },
        (remaining, id, revision),
    )?;
    checked_body(rows, remaining)
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
    selected: Option<&str>,
) -> Result<Vec<(String, String)>, HiveError> {
    let rows: Vec<ArtifactCandidateRow> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT l.artifact_id, l.issue_id, a.id, a.kind, a.revision,
                    a.archived_at IS NOT NULL, a.archive_reason IS NOT NULL
                 FROM artifact_issues AS OF '{commit}' AS l
                 LEFT JOIN artifacts AS OF '{commit}' AS a ON a.id = l.artifact_id
                 WHERE (l.issue_id = ? OR CAST(LOWER(l.issue_id) AS BINARY) = ?)
                 AND (? IS NULL OR CAST(l.artifact_id AS BINARY) = ?)
                 AND (a.id IS NULL OR LOWER(a.kind) = 'handoff')
                 AND (a.archived_at IS NULL OR a.archive_reason IS NULL)
                 ORDER BY CAST(l.artifact_id AS BINARY) LIMIT 2"
            )
        },
        (issue, issue, selected, selected),
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
        return Err(invalid_record("The exact work record is unavailable."));
    };
    if !workflow::valid_ref(&author) {
        return Err(invalid_record(
            "The work record has invalid author provenance.",
        ));
    }
    let body = body.filter(|body| body.len() <= remaining).ok_or_else(|| {
        limit("The required work record body exceeds the remaining context budget.")
    })?;
    if body.trim().is_empty() || body.contains('\0') {
        return Err(invalid_record("The required work record body is invalid."));
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

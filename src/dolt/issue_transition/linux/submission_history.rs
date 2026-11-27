use std::collections::HashSet;

use super::*;
use crate::dolt::receipt_history::{receipt_at, receipt_introduction};

const MAX_HISTORY_EDGES: usize = 256;

pub(super) fn verify(
    snapshot: &mut Snapshot,
    base: &str,
    request: &Request<'_>,
    decision: &ReviewDecision,
) -> Result<(), HiveError> {
    let at = as_of(Some(base))?;
    let submission_id = &decision.binding.submission_id;
    let typed: Option<String> = snapshot
        .connection
        .exec_first(
            format!(
                "SELECT CASE WHEN OCTET_LENGTH(transition_json) <= ? THEN transition_json END
                 FROM issue_updates{at} WHERE CAST(id AS BINARY) = ?
                 AND CAST(issue_id AS BINARY) = ? AND CAST(kind AS BINARY) = 'transition'"
            ),
            (
                workflow::MAX_DECLARATION_BYTES,
                submission_id,
                request.issue,
            ),
        )
        .map_err(read_error)?
        .flatten();
    let typed = typed.ok_or_else(workflow::unverified)?;
    let submission: Record = serde_json::from_str(&typed).map_err(|_| workflow::unverified())?;
    let operation = submission.effect.operation();
    if !matches!(operation, Operation::Submit | Operation::SubmitEpic)
        || submission.transition_id != *submission_id
    {
        return Err(workflow::unverified());
    }
    let receipt = receipt_at(snapshot, &submission.request_id, base, None)?
        .filter(|receipt| receipt.result == typed)
        .ok_or_else(workflow::unverified)?;
    let submission = check_scope(
        snapshot,
        operation,
        &submission.request_id,
        request.area,
        request.issue,
        &submission.actor_ref,
        &receipt,
    )?;
    let introduced = receipt_introduction(snapshot, &submission.request_id, &receipt)?;
    if !ancestor(snapshot, &introduced, base)? {
        return Err(workflow::unverified());
    }
    verify_rows(snapshot, &introduced, &receipt, &submission)?;

    if request.actor == submission.actor_ref
        || submission
            .effect
            .stopped()
            .is_some_and(|stopped| request.actor == stopped.stop.holder_ref)
        || !matches!((workflow::parse_time(&decision.reviewed_at), workflow::parse_time(&receipt.created_at)),
            (Some(reviewed), Some(submitted)) if reviewed >= submitted)
    {
        return Err(workflow::unverified());
    }

    // Look at retained edges, including removed or modified rows. A current
    // row by itself cannot establish that a later cycle did not replace it.
    type UpdateEdge = (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let transitions: Vec<UpdateEdge> = snapshot.exec_rows(
        |_| format!(
            "SELECT from_commit, to_commit, diff_type, to_id, to_issue_id, to_kind
             FROM dolt_diff_issue_updates{at}
             WHERE from_id = ? OR to_id = ?
                OR (from_issue_id = ? AND (from_kind = 'transition' OR from_transition_json IS NOT NULL))
                OR (to_issue_id = ? AND (to_kind = 'transition' OR to_transition_json IS NOT NULL))
             LIMIT ?"
        ),
        (submission_id, submission_id, request.issue, request.issue, MAX_HISTORY_EDGES + 1),
    )?;
    complete(snapshot, transitions.len())?;
    let mut found = false;
    for (parent, commit, kind, id, issue, update_kind) in transitions {
        if kind != "added"
            || issue.as_deref() != Some(request.issue)
            || update_kind.as_deref() != Some("transition")
            || !id.as_deref().is_some_and(is_uuid_v7)
        {
            return Err(workflow::unverified());
        }
        if id.as_ref() == Some(submission_id) {
            if found || commit != introduced || parent != receipt.base_commit {
                return Err(workflow::unverified());
            }
            found = true;
        } else if !ancestor(snapshot, &commit, &receipt.base_commit)? {
            return Err(workflow::unverified());
        }
    }
    if !found {
        return Err(workflow::unverified());
    }
    verify_revisions(
        snapshot,
        base,
        request,
        &introduced,
        &submission.issue_revision,
    )
}

fn verify_revisions(
    snapshot: &mut Snapshot,
    base: &str,
    request: &Request<'_>,
    introduced: &str,
    submitted_revision: &str,
) -> Result<(), HiveError> {
    let at = as_of(Some(base))?;
    type RevisionEdge = (String, String, String, Option<String>, Option<String>, bool);
    let edges: Vec<RevisionEdge> = snapshot.exec_rows(
        |_| format!(
            "SELECT from_commit, to_commit, diff_type, from_revision, to_revision,
               COALESCE(CAST(from_status AS BINARY) = 'review' AND CAST(to_status AS BINARY) = 'review'
                 AND CAST(from_id AS BINARY) = CAST(to_id AS BINARY)
                 AND CAST(from_area_id AS BINARY) = CAST(to_area_id AS BINARY)
                 AND CAST(from_type AS BINARY) = CAST(to_type AS BINARY)
                 AND from_number = to_number AND from_created_at = to_created_at
                 AND (CAST(from_title AS BINARY) <=> CAST(to_title AS BINARY))
                 AND (CAST(from_body AS BINARY) <=> CAST(to_body AS BINARY))
                 AND (CAST(from_acceptance_criteria AS BINARY) <=> CAST(to_acceptance_criteria AS BINARY))
                 AND (CAST(from_hold_reason AS BINARY) <=> CAST(to_hold_reason AS BINARY))
                 AND from_closed_at IS NULL AND to_closed_at IS NULL
                 AND from_closed_by_ref IS NULL AND to_closed_by_ref IS NULL
                 AND from_resolution_note IS NULL AND to_resolution_note IS NULL, FALSE)
             FROM dolt_diff_issues{at} WHERE from_id = ? OR to_id = ? LIMIT ?"
        ), (request.issue, request.issue, MAX_HISTORY_EDGES + 1),
    )?;
    complete(snapshot, edges.len())?;
    let mut later = Vec::new();
    let mut found = false;
    for (parent, commit, kind, before, after, preserves_submission) in edges {
        if commit == introduced {
            if found || kind != "modified" || after.as_deref() != Some(submitted_revision) {
                return Err(workflow::unverified());
            }
            found = true;
        } else if !ancestor(snapshot, &commit, introduced)? {
            if kind != "modified"
                || !preserves_submission
                || !ancestor(snapshot, introduced, &parent)?
            {
                return Err(workflow::unverified());
            }
            let before = before
                .filter(|revision| valid_revision(revision))
                .ok_or_else(workflow::unverified)?;
            let after = after
                .filter(|revision| valid_revision(revision))
                .ok_or_else(workflow::unverified)?;
            later.push((parent, commit, before, after));
        }
    }

    // Walk the exact revision edges backwards. Include every post-submission
    // edit and require its commit to precede the next edit in the chain.
    let mut revision = request.expected.to_owned();
    let mut head = base.to_owned();
    let mut seen = HashSet::from([revision.clone()]);
    while !later.is_empty() {
        let index = later
            .iter()
            .position(|(_, _, _, after)| *after == revision)
            .ok_or_else(workflow::unverified)?;
        let (parent, commit, before, _) = later.swap_remove(index);
        if !seen.insert(before.clone()) || !ancestor(snapshot, &commit, &head)? {
            return Err(workflow::unverified());
        }
        head = parent;
        revision = before;
    }
    if !found || revision != submitted_revision {
        return Err(workflow::unverified());
    }
    Ok(())
}

fn ancestor(snapshot: &mut Snapshot, ancestor: &str, head: &str) -> Result<bool, HiveError> {
    if !is_commit_hash(ancestor) || !is_commit_hash(head) {
        return Err(workflow::unverified());
    }
    if ancestor == head {
        return Ok(true);
    }
    let rows: Vec<Option<String>> = snapshot.exec_rows(
        |_| "SELECT DOLT_MERGE_BASE(?, ?)".to_owned(),
        (ancestor, head),
    )?;
    match rows.as_slice() {
        [Some(commit)] if is_commit_hash(commit) => Ok(commit == ancestor),
        _ => Err(workflow::unverified()),
    }
}

fn complete(snapshot: &mut Snapshot, len: usize) -> Result<(), HiveError> {
    if len > MAX_HISTORY_EDGES
        || snapshot
            .connection
            .query_first::<mysql::Row, _>("SHOW WARNINGS LIMIT 1")
            .map_err(read_error)?
            .is_some()
    {
        return Err(workflow::unverified());
    }
    Ok(())
}

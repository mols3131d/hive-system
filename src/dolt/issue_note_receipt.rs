//! Version 1 payload for the Issue-note receipt. The writer must bind the
//! normalized request (including the note bytes) to the fingerprint and insert
//! the note, advance the Issue revision and publish this receipt atomically.
//! Historical workflow evidence records the declaration at that boundary.

use serde::{Deserialize, Serialize};

use super::{
    Snapshot, context_workflow as workflow, is_uuid_v7, receipt_history::HistoricalReceipt,
};
use crate::error::HiveError;

use super::issue_note_fingerprint::KIND;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NoteResult {
    pub(super) version: u8,
    pub(super) area_id: String,
    pub(super) issue_id: String,
    pub(super) note_id: String,
    pub(super) issue_revision: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NoteEvidence {
    pub(super) version: u8,
    pub(super) operation: String,
    pub(super) workspace_id: String,
    pub(super) request_id: String,
    pub(super) fingerprint: String,
    pub(super) actor_ref: String,
    pub(super) bot_id: Option<String>,
    pub(super) area_id: String,
    pub(super) issue_id: String,
    pub(super) expected_issue_revision: String,
    pub(super) claim_id: Option<String>,
    pub(super) authorization_ref: String,
    pub(super) evidence_id: String,
    pub(super) revision: String,
    pub(super) source_ref: String,
    pub(super) issuer_ref: String,
    pub(super) valid_from: String,
    pub(super) expires_at: String,
    pub(super) withdrawn: bool,
}

pub(super) struct ScopedNote {
    pub(super) result: NoteResult,
    pub(super) evidence: NoteEvidence,
}

pub(super) fn check_scope(
    snapshot: &Snapshot,
    request: &str,
    area: &str,
    issue: &str,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<ScopedNote, HiveError> {
    if receipt.kind != KIND || receipt.actor_ref != actor {
        return Err(workflow::unverified());
    }
    let result: NoteResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: NoteEvidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    if result.version != 1
        || result.area_id != area
        || result.issue_id != issue
        || !is_uuid_v7(&result.note_id)
        || !workflow::valid_ref(&result.issue_revision)
        || evidence.version != 1
        || evidence.operation != KIND
        || evidence.workspace_id != snapshot.workspace_id
        || evidence.request_id != request
        || evidence.fingerprint != receipt.fingerprint
        || evidence.actor_ref != actor
        || evidence.area_id != area
        || evidence.issue_id != issue
        || evidence.authorization_ref != receipt.authorization_ref
        || evidence.bot_id.as_deref().is_some_and(|id| !is_uuid_v7(id))
        || evidence
            .claim_id
            .as_deref()
            .is_some_and(|id| !is_uuid_v7(id))
        || ![
            &evidence.evidence_id,
            &evidence.revision,
            &evidence.source_ref,
            &evidence.issuer_ref,
            &evidence.authorization_ref,
            &evidence.expected_issue_revision,
        ]
        .into_iter()
        .all(|value| workflow::valid_ref(value))
        || evidence.expected_issue_revision == result.issue_revision
        || evidence.withdrawn
        || !matches!(
            (workflow::parse_time(&evidence.valid_from),
             workflow::parse_time(&receipt.created_at),
             workflow::parse_time(&evidence.expires_at)),
            (Some(start), Some(created), Some(end)) if start <= created && created < end
        )
    {
        return Err(workflow::unverified());
    }
    Ok(ScopedNote { result, evidence })
}

pub(super) fn verify_effect(
    snapshot: &mut Snapshot,
    introduced: &str,
    receipt: &HistoricalReceipt,
    note: &ScopedNote,
) -> Result<(), HiveError> {
    let result = &note.result;
    // Both literals have been validated by receipt_introduction.
    let read_issue = |snapshot: &mut Snapshot, commit: &str| {
        snapshot.exec_rows::<(String, String), _>(
            |_| format!("SELECT area_id, revision FROM issues AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"),
            (&result.issue_id,),
        )
    };
    let before = read_issue(snapshot, &receipt.base_commit)?;
    let after = read_issue(snapshot, introduced)?;
    let base_notes: Vec<String> = snapshot.exec_rows(
        |_| {
            format!(
                "SELECT id FROM issue_updates AS OF '{}' WHERE CAST(id AS BINARY) = ? LIMIT 2",
                receipt.base_commit
            )
        },
        (&result.note_id,),
    )?;
    let notes: Vec<(String, String, String, bool)> = snapshot.exec_rows(
        |_| {
            format!(
                "SELECT issue_id, kind, author_ref, transition_json IS NULL
                     FROM issue_updates AS OF '{introduced}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
            )
        },
        (&result.note_id,),
    )?;
    if !matches!(before.as_slice(), [(area, revision)]
        if area == &result.area_id && revision == &note.evidence.expected_issue_revision)
        || !matches!(after.as_slice(), [(area, revision)]
            if area == &result.area_id && revision == &result.issue_revision)
        || !base_notes.is_empty()
        || !matches!(notes.as_slice(), [(issue, kind, author, true)]
            if issue == &result.issue_id && author == &receipt.actor_ref
                && matches!(kind.as_str(), "progress" | "review" | "handoff" | "evidence" | "comment"))
    {
        return Err(workflow::unverified());
    }
    if let Some(claim) = &note.evidence.claim_id {
        let holders: Vec<String> = snapshot.exec_rows(
            |_| format!("SELECT holder_ref FROM issue_claims AS OF '{}'
                         WHERE CAST(issue_id AS BINARY) = ? AND CAST(claim_id AS BINARY) = ? LIMIT 2", receipt.base_commit),
            (&result.issue_id, claim),
        )?;
        if !matches!(holders.as_slice(), [holder] if holder == &receipt.actor_ref) {
            return Err(workflow::unverified());
        }
    }
    Ok(())
}

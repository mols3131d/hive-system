//! Proposed v1 read declaration for the draft resolver. The entry point must
//! identify a fresh operator request or re-resolve the selected host source on
//! each invocation. Receipt JSON and cached sources are historical inputs.

use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::dolt::{is_commit_hash, is_uuid, is_uuid_v7};
use crate::error::{ErrorCode, HiveError};

const MAX_DECLARATION_BYTES: usize = 16 * 1024;
const MAX_REF_BYTES: usize = 255;
const MAX_OPERATIONS: usize = 2;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Operation {
    ReadHandoffBody,
    DiscloseArtifactBody,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum RecordKind {
    IssueUpdate,
    Artifact,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactRecord {
    kind: RecordKind,
    id: String,
    revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    version: u8,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    workspace_id: String,
    actor_ref: String,
    bot_id: Option<String>,
    area_id: String,
    issue_id: String,
    operations: Vec<Operation>,
    record: ExactRecord,
    invocation_id: String,
    db_commit: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Locator<'a> {
    pub(super) evidence_id: &'a str,
    pub(super) revision: &'a str,
    pub(super) source_ref: &'a str,
    pub(super) issuer_ref: &'a str,
}

#[derive(Clone, Copy)]
pub(super) enum CurrentInput<'a> {
    /// A one-invocation declaration supplied through the explicit operator path.
    OperatorRequest(&'a [u8]),
    /// Bytes returned by the current state of the explicitly selected source.
    ResolvedSource {
        selected: Locator<'a>,
        bytes: &'a [u8],
    },
}

#[derive(Clone, Copy)]
pub(super) struct Request<'a> {
    pub(super) actor_ref: &'a str,
    pub(super) bot_id: Option<&'a str>,
    pub(super) invocation_id: &'a str,
    pub(super) observed_at: OffsetDateTime,
    pub(super) input: Option<CurrentInput<'a>>,
}

pub(super) struct Expected<'a> {
    pub(super) workspace_id: &'a str,
    pub(super) db_commit: &'a str,
    pub(super) area_id: &'a str,
    pub(super) issue_id: &'a str,
    pub(super) operation: Operation,
    pub(super) record_kind: RecordKind,
    pub(super) record_id: &'a str,
    pub(super) record_revision: &'a str,
}

pub(super) fn verify(request: &Request<'_>, expected: &Expected<'_>) -> Result<(), HiveError> {
    let (selected, bytes) = match request.input.as_ref() {
        Some(CurrentInput::OperatorRequest(bytes)) => (None, *bytes),
        Some(CurrentInput::ResolvedSource { selected, bytes }) => (Some(selected), *bytes),
        None => return Err(unverified()),
    };
    if bytes.len() > MAX_DECLARATION_BYTES {
        return Err(HiveError::new(
            ErrorCode::ContextLimit,
            "The workflow read declaration exceeds 16 KiB.",
        ));
    }
    let evidence: Declaration = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let starts = parse_time(&evidence.valid_from).ok_or_else(malformed)?;
    let expires = parse_time(&evidence.expires_at).ok_or_else(malformed)?;
    if evidence.version != 1
        || !is_uuid(&evidence.workspace_id)
        || !is_uuid(&evidence.invocation_id)
        || !is_commit_hash(&evidence.db_commit)
        || !is_uuid_v7(&evidence.area_id)
        || !is_uuid_v7(&evidence.issue_id)
        || !is_uuid_v7(&evidence.record.id)
        || evidence.bot_id.as_deref().is_some_and(|id| !is_uuid_v7(id))
        || ![
            &evidence.evidence_id,
            &evidence.revision,
            &evidence.source_ref,
            &evidence.issuer_ref,
            &evidence.actor_ref,
            &evidence.record.revision,
        ]
        .into_iter()
        .all(|value| valid_ref(value))
        || evidence.operations.is_empty()
        || evidence.operations.len() > MAX_OPERATIONS
        || (evidence.operations.len() == 2 && evidence.operations[0] == evidence.operations[1])
        || starts >= expires
    {
        return Err(malformed());
    }

    if selected.is_some_and(|locator| {
        evidence.evidence_id != locator.evidence_id
            || evidence.revision != locator.revision
            || evidence.source_ref != locator.source_ref
            || evidence.issuer_ref != locator.issuer_ref
    }) {
        return Err(unverified());
    }
    if evidence.withdrawn || request.observed_at < starts || request.observed_at >= expires {
        return Err(unverified());
    }
    if evidence.workspace_id != expected.workspace_id
        || evidence.actor_ref != request.actor_ref
        || evidence.bot_id.as_deref() != request.bot_id
        || evidence.area_id != expected.area_id
        || evidence.issue_id != expected.issue_id
        || evidence.invocation_id != request.invocation_id
        || evidence.db_commit != expected.db_commit
        || !evidence.operations.contains(&expected.operation)
        || evidence.record.kind != expected.record_kind
        || evidence.record.id != expected.record_id
        || evidence.record.revision != expected.record_revision
    {
        return Err(HiveError::new(
            ErrorCode::Unauthorized,
            "The current workflow declaration does not cover this exact body read.",
        ));
    }
    Ok(())
}

pub(super) fn parse_time(value: &str) -> Option<OffsetDateTime> {
    if value.len() > 35 {
        return None;
    }
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

pub(super) fn valid_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REF_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn malformed() -> HiveError {
    HiveError::new(
        ErrorCode::InvalidArgument,
        "The workflow read declaration is malformed.",
    )
}

pub(super) fn unverified() -> HiveError {
    HiveError::new(
        ErrorCode::AuthorityUnverified,
        "The current workflow evidence is unavailable, stale, or withdrawn.",
    )
}

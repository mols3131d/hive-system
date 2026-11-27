//! Receipt rows and their immutable introduction edge in retained Dolt history.

use super::{Snapshot, context_workflow as workflow, is_commit_hash, is_uuid};
use crate::error::{ErrorCode, HiveError};

const MAX_RECEIPT_BYTES: usize = 256 * 1024;

type ReceiptRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
);

#[derive(PartialEq, Eq)]
pub(super) struct HistoricalReceipt {
    pub(super) fingerprint: String,
    pub(super) base_commit: String,
    pub(super) created_at: String,
    pub(super) kind: String,
    pub(super) authorization_ref: String,
    pub(super) actor_ref: String,
    pub(super) workflow_evidence: String,
    pub(super) result: String,
}

pub(super) fn receipt_at(
    snapshot: &mut Snapshot,
    request_id: &str,
    commit: &str,
    expected: Option<(&str, &str)>,
) -> Result<Option<HistoricalReceipt>, HiveError> {
    // Dolt requires a literal AS OF revision. Never interpolate unvalidated history.
    if !is_commit_hash(commit) || !is_uuid(request_id) {
        return Err(invalid_record("A receipt history commit is malformed."));
    }
    let rows: Vec<ReceiptRow> = snapshot.exec_rows(
        |_| {
            format!(
                "SELECT kind, actor_ref, fingerprint, CASE WHEN OCTET_LENGTH(authorization_ref) <= ? THEN authorization_ref END,
                base_commit, DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'),
                CASE WHEN OCTET_LENGTH(workflow_evidence_json) + OCTET_LENGTH(result_json) <= ?
                     THEN workflow_evidence_json END,
                CASE WHEN OCTET_LENGTH(workflow_evidence_json) + OCTET_LENGTH(result_json) <= ?
                     THEN result_json END
             FROM operation_receipts AS OF '{commit}' WHERE CAST(request_id AS BINARY) = ? LIMIT 2"
            )
        },
        (
            MAX_RECEIPT_BYTES,
            MAX_RECEIPT_BYTES,
            MAX_RECEIPT_BYTES,
            request_id,
        ),
    )?;
    if rows.is_empty() {
        return Ok(None);
    }
    let [(kind, actor, fingerprint, authorization_ref, base_commit, created_at, evidence, result)] =
        rows.as_slice()
    else {
        return Err(workflow::unverified());
    };
    // Refuse a wrong Directive receipt header before decoding its payload.
    if expected.is_some_and(|(expected_kind, approval)| {
        kind != expected_kind || authorization_ref.as_deref() != Some(approval)
    }) || !workflow::valid_ref(actor)
        || !valid_sha256(fingerprint)
    {
        return Err(workflow::unverified());
    }
    Ok(Some(HistoricalReceipt {
        kind: kind.clone(),
        authorization_ref: authorization_ref.clone().ok_or_else(workflow::unverified)?,
        fingerprint: fingerprint.clone(),
        base_commit: base_commit.clone(),
        created_at: created_at.clone(),
        actor_ref: actor.clone(),
        workflow_evidence: receipt_object(evidence.as_deref())?.to_owned(),
        result: receipt_object(result.as_deref())?.to_owned(),
    }))
}

/// Inspect changes to the exact receipt in the retained, committed view.
pub(super) fn receipt_introduction(
    snapshot: &mut Snapshot,
    request_id: &str,
    receipt: &HistoricalReceipt,
) -> Result<String, HiveError> {
    if !is_commit_hash(&receipt.base_commit) {
        return Err(invalid_record("The receipt has a malformed base commit."));
    }
    let observed = snapshot.commit.clone();
    let changes = receipt_changes(snapshot, request_id)?;
    let [(base, introduced, kind, None, Some(added_request))] = changes.as_slice() else {
        return Err(workflow::unverified());
    };
    if kind != "added"
        || added_request != request_id
        || base != &receipt.base_commit
        || !is_commit_hash(introduced)
        || introduced == base
    {
        return Err(workflow::unverified());
    }

    // Anchor the edge to the paired Snapshot and require the receipt's base
    // to be the introducing commit's sole parent.
    let merger: Vec<Option<String>> = snapshot.exec_rows(
        |_| "SELECT DOLT_MERGE_BASE(?, ?)".to_owned(),
        (introduced, &observed),
    )?;
    let parents: Vec<String> = snapshot.exec_rows(
        |_| {
            "SELECT parent_hash FROM dolt_commit_ancestors WHERE commit_hash = ? LIMIT 2".to_owned()
        },
        (introduced,),
    )?;
    let expected = Some((receipt.kind.as_str(), receipt.authorization_ref.as_str()));
    if !matches!(merger.as_slice(), [Some(ancestor)] if ancestor == introduced)
        || !matches!(parents.as_slice(), [parent] if parent == base)
        || receipt_at(snapshot, request_id, base, expected)?.is_some()
        || receipt_at(snapshot, request_id, introduced, expected)?.as_ref() != Some(receipt)
    {
        return Err(workflow::unverified());
    }
    Ok(introduced.clone())
}

/// A request ID remains reserved even when its receipt has been deleted.
#[cfg(target_os = "linux")]
pub(super) fn has_receipt_history(
    snapshot: &mut Snapshot,
    request_id: &str,
) -> Result<bool, HiveError> {
    receipt_changes(snapshot, request_id).map(|changes| !changes.is_empty())
}

type Delta = (String, String, String, Option<String>, Option<String>);

fn receipt_changes(snapshot: &mut Snapshot, request_id: &str) -> Result<Vec<Delta>, HiveError> {
    if !is_uuid(request_id) {
        return Err(invalid_record("The receipt request ID is malformed."));
    }
    snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT from_commit, to_commit, diff_type, from_request_id, to_request_id
                 FROM dolt_diff_operation_receipts AS OF '{commit}'
                 WHERE from_request_id = ? OR to_request_id = ? LIMIT 2"
            )
        },
        (request_id, request_id),
    )
}

pub(super) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn receipt_object(value: Option<&str>) -> Result<&str, HiveError> {
    let value =
        value.ok_or_else(|| limit("The historical receipt exceeds the provenance read limit."))?;
    match serde_json::from_str(value) {
        Ok(serde_json::Value::Object(_)) => Ok(value),
        _ => Err(invalid_record(
            "The historical receipt has invalid provenance JSON.",
        )),
    }
}

fn invalid_record(message: &str) -> HiveError {
    HiveError::new(ErrorCode::InvalidRecord, message)
}

fn limit(message: &str) -> HiveError {
    HiveError::new(ErrorCode::ContextLimit, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_historical_provenance_objects_within_the_read_limit() {
        assert_eq!(receipt_object(Some("{}")).unwrap(), "{}");
        for malformed in ["[]", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
            assert!(matches!(
                receipt_object(Some(malformed)).unwrap_err().code,
                ErrorCode::InvalidRecord
            ));
        }
        assert!(matches!(
            receipt_object(None).unwrap_err().code,
            ErrorCode::ContextLimit
        ));
    }
}

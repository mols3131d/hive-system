//! Structural applicability and historical provenance candidates.
//!
//! Active status and a matching approval reference do not prove an approved
//! direction. The activation receipt result and reviewed decision must bind
//! the exact Directive revision and its scope before any body can be included.

use time::OffsetDateTime;

use super::{Scope, invalid_record, limit, workflow};
use crate::dolt::{
    Snapshot,
    directive::{METADATA, MetadataRow, Scope as DirectiveScope},
    is_uuid,
};
use crate::error::{ErrorCode, HiveError};

const MAX_DIRECTIVES: usize = 100;
const MAX_RECEIPT_BYTES: usize = 256 * 1024;

pub(super) struct Candidate {
    db_commit: String,
    pub(super) id: String,
    pub(super) revision: String,
    pub(super) issuer_ref: String,
    pub(super) source_ref: String,
    pub(super) approval_ref: String,
}

pub(super) fn collect(
    snapshot: &mut Snapshot,
    scope: &Scope,
    observed_at: OffsetDateTime,
) -> Result<Vec<Candidate>, HiveError> {
    if snapshot.commit != scope.db_commit {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "Resolve Directives from the same committed context.",
        ));
    }
    let mut selections = vec![DirectiveScope::Workspace];
    selections.extend(
        scope
            .ancestry
            .iter()
            .rev()
            .map(|area| DirectiveScope::Area(&area.id)),
    );
    if let Some(project) = &scope.project {
        selections.push(DirectiveScope::Project(&project.id));
    }
    let mut candidates = Vec::new();
    let mut scanned = 0;
    for selection in selections {
        let (filter, value, descendants_only) = match selection {
            DirectiveScope::Workspace => (
                "(scope_kind = ? OR LOWER(scope_kind) = ?)",
                "workspace",
                false,
            ),
            DirectiveScope::Area(id) => (
                "(area_id = ? OR CAST(LOWER(area_id) AS BINARY) = CAST(? AS BINARY))",
                id,
                scope.ancestry.first().is_some_and(|area| area.id != id),
            ),
            DirectiveScope::Project(id) => (
                "(project_id = ? OR CAST(LOWER(project_id) AS BINARY) = CAST(? AS BINARY))",
                id,
                false,
            ),
        };
        let rows: Vec<MetadataRow> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT {METADATA} FROM directives AS OF '{commit}'
                 WHERE {filter} AND LOWER(status) = 'active'
                   AND (NOT ? OR include_descendants <> 0)
                 ORDER BY CAST(id AS BINARY) LIMIT ?"
                )
            },
            (value, value, descendants_only, MAX_DIRECTIVES - scanned + 1),
        )?;
        scanned += rows.len();
        if scanned > MAX_DIRECTIVES {
            return Err(limit(
                "The context exceeds the applicable Directive scan limit.",
            ));
        }
        for row in rows {
            if row.6 != "active" {
                return Err(invalid_record(
                    "An applicable Directive has noncanonical active status.",
                ));
            }
            let metadata = selection.metadata(row)?;
            if !currently_applicable(
                metadata.valid_from.as_deref(),
                metadata.expires_at.as_deref(),
                observed_at,
            )? {
                continue;
            }
            if !workflow::valid_ref(&metadata.revision) {
                return Err(invalid_record(
                    "An applicable Directive has invalid revision provenance.",
                ));
            }
            let rows: Vec<(String, Option<String>, Option<String>)> = snapshot.exec_rows(
                |commit| {
                    format!(
                        "SELECT issuer_ref,
                       CASE WHEN OCTET_LENGTH(source_ref) <= 255 THEN source_ref END,
                       CASE WHEN OCTET_LENGTH(approval_ref) <= 255 THEN approval_ref END
                     FROM directives AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
                    )
                },
                (&metadata.id,),
            )?;
            let [(issuer, Some(source), Some(approval))] = rows.as_slice() else {
                return Err(invalid_record(
                    "An applicable Directive has missing or oversized provenance.",
                ));
            };
            if ![issuer, source, approval]
                .into_iter()
                .all(|value| workflow::valid_ref(value))
            {
                return Err(invalid_record(
                    "An applicable Directive has invalid provenance references.",
                ));
            }
            candidates.push(Candidate {
                db_commit: snapshot.commit.clone(),
                id: metadata.id,
                revision: metadata.revision,
                issuer_ref: issuer.clone(),
                source_ref: source.clone(),
                approval_ref: approval.clone(),
            });
        }
    }
    Ok(candidates)
}

fn currently_applicable(
    start: Option<&str>,
    end: Option<&str>,
    observed_at: OffsetDateTime,
) -> Result<bool, HiveError> {
    let parse = |value| {
        workflow::parse_time(value).ok_or_else(|| {
            invalid_record("An applicable Directive has an invalid validity timestamp.")
        })
    };
    let start = start.map(parse).transpose()?;
    let end = end.map(parse).transpose()?;
    if matches!((start, end), (Some(start), Some(end)) if end <= start) {
        return Err(invalid_record(
            "An applicable Directive has an invalid validity interval.",
        ));
    }
    Ok(start.is_none_or(|value| observed_at >= value)
        && end.is_none_or(|value| observed_at < value))
}

type ReceiptRow = (String, String, bool, Option<String>, Option<String>);

pub(super) struct HistoricalReceipt {
    pub(super) actor_ref: String,
    pub(super) workflow_evidence: serde_json::Value,
    pub(super) result: serde_json::Value,
}

/// This is a historical lookup only. The caller supplies an exact request ID
/// from reviewed provenance; approval_ref is an opaque locator, not that ID.
pub(super) fn lookup_activation_receipt(
    snapshot: &mut Snapshot,
    candidate: &Candidate,
    request_id: &str,
) -> Result<HistoricalReceipt, HiveError> {
    if snapshot.commit != candidate.db_commit {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "Resolve historical Directive provenance from the same committed context.",
        ));
    }
    if !is_uuid(request_id) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Select an exact activation receipt request ID.",
        ));
    }
    let rows: Vec<ReceiptRow> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT kind, actor_ref, CAST(authorization_ref AS BINARY) = ?,
                CASE WHEN OCTET_LENGTH(workflow_evidence_json) + OCTET_LENGTH(result_json) <= ?
                     THEN workflow_evidence_json END,
                CASE WHEN OCTET_LENGTH(workflow_evidence_json) + OCTET_LENGTH(result_json) <= ?
                     THEN result_json END
             FROM operation_receipts AS OF '{commit}' WHERE CAST(request_id AS BINARY) = ? LIMIT 2"
            )
        },
        (
            &candidate.approval_ref,
            MAX_RECEIPT_BYTES,
            MAX_RECEIPT_BYTES,
            request_id,
        ),
    )?;
    let [(kind, actor, matches_approval, evidence, result)] = rows.as_slice() else {
        return Err(workflow::unverified());
    };
    // Candidate operation spelling until the guarded writer defines its codec.
    if kind != "directive.activate" || !*matches_approval || !workflow::valid_ref(actor) {
        return Err(workflow::unverified());
    }
    Ok(HistoricalReceipt {
        actor_ref: actor.clone(),
        workflow_evidence: receipt_object(evidence.as_deref())?,
        result: receipt_object(result.as_deref())?,
    })
}

fn receipt_object(value: Option<&str>) -> Result<serde_json::Value, HiveError> {
    let value =
        value.ok_or_else(|| limit("The historical receipt exceeds the provenance read limit."))?;
    match serde_json::from_str(value) {
        Ok(value @ serde_json::Value::Object(_)) => Ok(value),
        _ => Err(invalid_record(
            "The historical receipt has invalid provenance JSON.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_historical_provenance_objects_within_the_read_limit() {
        assert_eq!(receipt_object(Some("{}")).unwrap(), serde_json::json!({}));
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

    #[test]
    fn checks_directive_validity_at_one_observed_time() {
        let start = "2026-01-01T00:00:00.000000Z";
        let end = "2026-01-02T00:00:00.000000Z";
        assert!(
            currently_applicable(Some(start), Some(end), workflow::parse_time(start).unwrap())
                .unwrap()
        );
        assert!(
            !currently_applicable(Some(start), Some(end), workflow::parse_time(end).unwrap())
                .unwrap()
        );
        assert!(
            currently_applicable(
                Some("fixture-invalid"),
                None,
                workflow::parse_time(start).unwrap()
            )
            .is_err()
        );
    }
}

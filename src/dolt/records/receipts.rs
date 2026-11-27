use super::{Diagnostics, HiveError, Snapshot, is_uuid};

const TABLE: &str = "operation_receipts";
const COLUMNS: &str = "request_id, fingerprint, \
    CASE WHEN JSON_VALID(workflow_evidence_json) THEN JSON_TYPE(workflow_evidence_json) = 'OBJECT' ELSE FALSE END, \
    CASE WHEN JSON_VALID(result_json) THEN JSON_TYPE(result_json) = 'OBJECT' ELSE FALSE END";

pub(super) fn validate(
    snapshot: &mut Snapshot,
    diagnostics: &mut Diagnostics,
) -> Result<(), HiveError> {
    let rows = snapshot.records::<(String, String, bool, bool)>(TABLE, COLUMNS, "request_id")?;
    for (request_id, fingerprint, workflow_evidence_is_object, result_is_object) in rows {
        validate_row(
            &request_id,
            &fingerprint,
            workflow_evidence_is_object,
            result_is_object,
            diagnostics,
        );
    }
    Ok(())
}

fn validate_row(
    request_id: &str,
    fingerprint: &str,
    workflow_evidence_is_object: bool,
    result_is_object: bool,
    diagnostics: &mut Diagnostics,
) {
    if !is_uuid(request_id) {
        diagnostics.add_uuid("INVALID_ID", TABLE, &[], "Use a canonical lowercase UUID.");
    }
    if !is_sha256_hex(fingerprint) {
        diagnostics.add_uuid(
            "INVALID_RECEIPT_FINGERPRINT",
            TABLE,
            &[request_id],
            "Use a 64-character lowercase hexadecimal SHA-256 fingerprint.",
        );
    }
    if !workflow_evidence_is_object {
        diagnostics.add_uuid(
            "INVALID_RECEIPT_WORKFLOW_EVIDENCE",
            TABLE,
            &[request_id],
            "Receipt workflow_evidence_json must be a JSON object.",
        );
    }
    if !result_is_object {
        diagnostics.add_uuid(
            "INVALID_RECEIPT_RESULT",
            TABLE,
            &[request_id],
            "Receipt result_json must be a JSON object.",
        );
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_uuid_versions_and_rejects_malformed_receipt_fields() {
        let valid_fp = "a".repeat(64);
        for request_id in [
            "01900000-0000-4000-8000-000000000001",
            "01900000-0000-7000-8000-000000000002",
        ] {
            let mut diagnostics = Diagnostics::default();
            validate_row(request_id, &valid_fp, true, true, &mut diagnostics);
            assert!(diagnostics.items.is_empty(), "{request_id}");
        }

        let mut diagnostics = Diagnostics::default();
        let bad_fp = "A".repeat(64);
        let valid_v4 = "01900000-0000-4000-8000-000000000003";
        validate_row(valid_v4, &bad_fp, false, false, &mut diagnostics);
        let codes: Vec<_> = diagnostics.items.iter().map(|item| item.code).collect();
        assert_eq!(
            codes,
            [
                "INVALID_RECEIPT_FINGERPRINT",
                "INVALID_RECEIPT_WORKFLOW_EVIDENCE",
                "INVALID_RECEIPT_RESULT",
            ]
        );
        for item in &diagnostics.items {
            assert_eq!(item.record_ids, [valid_v4]);
        }
    }
}

use mysql::Conn;
use mysql::prelude::Queryable;

use super::{ADMIN, Fixture, assert_diagnostics, commit, id, storage_state};

fn insert_receipt(
    admin: &mut Conn,
    request_id: &str,
    kind: &str,
    fingerprint: &str,
    workflow_evidence_json: &str,
    base_commit: &str,
    result_json: &str,
) {
    admin
        .exec_drop(
            "INSERT INTO operation_receipts
             (request_id, kind, fingerprint, actor_ref, authorization_ref, workflow_evidence_json,
              base_commit, result_json, created_at)
             VALUES (?, ?, ?, 'fixture-actor', 'fixture-authorization', ?, ?, ?, UTC_TIMESTAMP(6))",
            (
                request_id,
                kind,
                fingerprint,
                workflow_evidence_json,
                base_commit,
                result_json,
            ),
        )
        .unwrap();
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn accepts_canonical_uuid_and_sha256_receipts_with_json_objects() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let head = storage_state(&mut admin).0;
    let uuidv7 = id(0x80);
    let uuidv4 = id(0x81).replace("-7000-", "-4000-");
    let fp1 = "a".repeat(64);
    let fp2 = "0123456789abcdef".repeat(4);
    let result1 = format!(r#"{{"issue_id":"{}","revision":"{}"}}"#, id(11), id(4));

    insert_receipt(&mut admin, &uuidv7, "claim", &fp1, "{}", &head, &result1);
    insert_receipt(
        &mut admin,
        &uuidv4,
        "issue create",
        &fp2,
        r#"{"mode":"reviewed"}"#,
        "fixture-base-commit",
        "{}",
    );
    commit(&mut admin);
    fixture.check(0, &mut admin);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rejects_malformed_receipt_fields_only_at_the_committed_snapshot_with_redaction() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let baseline = fixture.check(0, &mut admin);
    let before = storage_state(&mut admin);

    let uuidv4 = id(0x80).replace("-7000-", "-4000-");
    let uuidv7 = id(0x81);
    let uppercase_id = id(0x8a).to_ascii_uppercase();
    let invalid_id = "fixture-invalid-receipt-id";
    let upper_fp = "A".repeat(64);
    let short_fp = "0".repeat(63);
    let non_hex_fp = "g".repeat(64);
    let valid_fp = "f".repeat(64);

    insert_receipt(
        &mut admin,
        &uuidv4,
        "claim",
        &upper_fp,
        "[]",
        "fixture-base-commit",
        r#""scalar""#,
    );
    insert_receipt(
        &mut admin,
        &uuidv7,
        "rename",
        &short_fp,
        "{bad",
        "fixture-base-commit",
        "null",
    );
    insert_receipt(
        &mut admin,
        &uppercase_id,
        "claim",
        &valid_fp,
        "{}",
        "fixture-base-commit",
        "{}",
    );
    insert_receipt(
        &mut admin,
        invalid_id,
        "claim",
        &non_hex_fp,
        "{}",
        "fixture-base-commit",
        "{}",
    );

    let dirty = storage_state(&mut admin);
    assert_ne!(dirty.1, before.1);
    assert_eq!(dirty.2, before.2);
    assert_eq!(fixture.check(0, &mut admin), baseline);
    admin.query_drop("CALL DOLT_ADD('.')").unwrap();
    assert_eq!(storage_state(&mut admin).2, dirty.1);
    assert_eq!(fixture.check(0, &mut admin), baseline);
    commit(&mut admin);

    let report = fixture.check(4, &mut admin);
    assert_diagnostics(
        &report,
        &[
            (
                "INVALID_RECEIPT_FINGERPRINT",
                "operation_receipts",
                &[&uuidv4],
            ),
            (
                "INVALID_RECEIPT_WORKFLOW_EVIDENCE",
                "operation_receipts",
                &[&uuidv4],
            ),
            ("INVALID_RECEIPT_RESULT", "operation_receipts", &[&uuidv4]),
            (
                "INVALID_RECEIPT_FINGERPRINT",
                "operation_receipts",
                &[&uuidv7],
            ),
            (
                "INVALID_RECEIPT_WORKFLOW_EVIDENCE",
                "operation_receipts",
                &[&uuidv7],
            ),
            ("INVALID_RECEIPT_RESULT", "operation_receipts", &[&uuidv7]),
            ("INVALID_ID", "operation_receipts", &[]),
            ("INVALID_ID", "operation_receipts", &[]),
            ("INVALID_RECEIPT_FINGERPRINT", "operation_receipts", &[]),
        ],
    );
    assert_eq!(fixture.check(4, &mut admin), report);

    let report_text = report.to_string();
    for secret in [
        invalid_id,
        &uppercase_id,
        &upper_fp,
        &short_fp,
        &non_hex_fp,
        "{bad",
        "fixture-actor",
        "fixture-authorization",
        "fixture-base-commit",
    ] {
        assert!(!report_text.contains(secret), "{secret}");
    }
}

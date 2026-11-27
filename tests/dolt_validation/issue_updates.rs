use mysql::Conn;
use mysql::prelude::Queryable;

use super::{ADMIN, Fixture, assert_diagnostics, commit, id, storage_state};

fn insert_update(
    admin: &mut Conn,
    update_id: &str,
    issue_id: &str,
    kind: &str,
    transition_json: Option<&str>,
) {
    admin
        .exec_drop(
            "INSERT INTO issue_updates (id, issue_id, kind, body, author_ref, created_at, transition_json)
             VALUES (?, ?, ?, 'Synthetic update body', 'fixture-author', UTC_TIMESTAMP(6), ?)",
            (update_id, issue_id, kind, transition_json),
        )
        .unwrap();
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn accepts_canonical_issue_updates_and_transition_objects() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let issue = id(11);
    for (number, kind) in (0x70..).zip(["progress", "review", "handoff", "evidence", "comment"]) {
        insert_update(&mut admin, &id(number), &issue, kind, None);
    }
    insert_update(&mut admin, &id(0x75), &issue, "transition", Some("{}"));
    let payload = format!(
        r#"{{"request_id":"{}","from_status":"todo","to_status":"in-progress"}}"#,
        id(0x77)
    );
    insert_update(&mut admin, &id(0x76), &id(12), "transition", Some(&payload));
    commit(&mut admin);
    fixture.check(0, &mut admin);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rejects_noncanonical_issue_update_ids_kinds_and_transition_json() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let issue = id(11);
    let issue_alias = issue.to_ascii_uppercase();
    let (bad_owner_ref, bad_transition_kind, bad_progress_kind, bad_json, array_json, null_json) =
        (id(0x70), id(0x71), id(0x72), id(0x73), id(0x74), id(0x75));
    let invalid_id = "fixture-invalid-update-id";
    let uuidv4_id = id(0x78).replace("-7000-", "-4000-");

    // Case-insensitive FK and CHECK collations allow noncanonical aliases and spellings.
    insert_update(&mut admin, &bad_owner_ref, &issue_alias, "progress", None);
    insert_update(
        &mut admin,
        &bad_transition_kind,
        &issue,
        "TRANSITION",
        Some("{}"),
    );
    insert_update(&mut admin, &bad_progress_kind, &issue, "Progress", None);
    insert_update(&mut admin, &bad_json, &issue, "transition", Some("{bad"));
    insert_update(&mut admin, &array_json, &issue, "transition", Some("[]"));
    insert_update(&mut admin, &null_json, &issue, "transition", Some("null"));
    insert_update(&mut admin, invalid_id, &issue, "comment", None);
    insert_update(&mut admin, &uuidv4_id, &issue, "comment", None);
    commit(&mut admin);

    let report = fixture.check(4, &mut admin);
    assert_diagnostics(
        &report,
        &[
            ("INVALID_ID", "issue_updates", &[]),
            ("INVALID_ID", "issue_updates", &[&bad_owner_ref]),
            (
                "INVALID_ISSUE_UPDATE_KIND",
                "issue_updates",
                &[&bad_transition_kind],
            ),
            (
                "INVALID_ISSUE_UPDATE_KIND",
                "issue_updates",
                &[&bad_progress_kind],
            ),
            (
                "INVALID_ISSUE_UPDATE_TRANSITION",
                "issue_updates",
                &[&bad_json],
            ),
            (
                "INVALID_ISSUE_UPDATE_TRANSITION",
                "issue_updates",
                &[&array_json],
            ),
            (
                "INVALID_ISSUE_UPDATE_TRANSITION",
                "issue_updates",
                &[&null_json],
            ),
            ("INVALID_ID", "issue_updates", &[]),
        ],
    );
    let report_text = report.to_string();
    for value in [
        invalid_id,
        &uuidv4_id,
        &issue_alias,
        "{bad",
        "Synthetic update body",
        "fixture-author",
    ] {
        assert!(!report_text.contains(value), "{value}");
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn checks_missing_issue_update_owner_only_in_the_committed_snapshot() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let baseline = fixture.check(0, &mut admin);
    let before = storage_state(&mut admin);

    let update = id(0x70);
    let missing_issue = id(0x90);
    let insert = format!(
        "INSERT INTO issue_updates (id, issue_id, kind, body, author_ref, created_at)
         VALUES ('{update}', '{missing_issue}', 'progress', 'Synthetic update body', 'fixture-author', UTC_TIMESTAMP(6))"
    );
    let Err(mysql::Error::MySqlError(error)) = admin.query_drop(&insert) else {
        panic!("Expected SQL FK error 1452");
    };
    assert_eq!(error.code, 1452);
    assert_eq!(storage_state(&mut admin), before);

    admin
        .query_drop("SET SESSION foreign_key_checks=0")
        .unwrap();
    admin.query_drop(&insert).unwrap();
    let dirty = storage_state(&mut admin);
    assert_ne!(dirty.1, before.1);
    assert_eq!(dirty.2, before.2);
    assert_eq!(fixture.check(0, &mut admin), baseline);
    admin.query_drop("CALL DOLT_ADD('.')").unwrap();
    assert_eq!(storage_state(&mut admin).2, dirty.1);
    assert_eq!(fixture.check(0, &mut admin), baseline);
    admin
        .query_drop("SET SESSION dolt_force_transaction_commit=1")
        .unwrap();
    commit(&mut admin);

    let report = fixture.check(4, &mut admin);
    assert_diagnostics(
        &report,
        &[(
            "MISSING_REFERENCE",
            "issue_updates",
            &[&update, &missing_issue],
        )],
    );
    assert_eq!(fixture.check(4, &mut admin), report);
}

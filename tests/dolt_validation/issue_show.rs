use std::net::TcpListener;
use std::process::Output;

use mysql::Conn;
use mysql::prelude::Queryable;
use serde_json::Value;

use super::{ADMIN, CASE_INSENSITIVE, DATABASE, Fixture, HOST, commit, id, storage_state};

fn show_issue(fixture: &Fixture, selector: &str, json: bool) -> Output {
    show_issue_with(
        fixture,
        selector,
        &id(1),
        &fixture.password,
        fixture.port,
        json,
    )
}

fn show_issue_with(
    fixture: &Fixture,
    selector: &str,
    workspace_id: &str,
    password: &str,
    port: u16,
    json: bool,
) -> Output {
    let port_str = port.to_string();
    let mut args = vec!["--contract", "2"];
    if json {
        args.push("--json");
    }
    args.extend([
        "issue",
        "show",
        selector,
        "--workspace-id",
        workspace_id,
        "--database",
        DATABASE,
        "--port",
        &port_str,
    ]);
    fixture.run_hive(&args, password)
}

fn check_issue_show(fixture: &Fixture, selector: &str, exit: i32, writer: &mut Conn) -> Value {
    let before = storage_state(writer);
    let output = show_issue(fixture, selector, true);
    assert_eq!(
        storage_state(writer),
        before,
        "issue show altered the Dolt state"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(exit), "{report}");
    assert!(output.stderr.is_empty(), "{report}");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], "issue show");
    assert_eq!(report["ok"], exit == 0);
    report
}

fn seed_relations(admin: &mut Conn) {
    let issue = id(11);
    let revision = id(4);
    admin
        .exec_drop(
            "UPDATE issues SET body=?, acceptance_criteria=?, priority=?, hold_reason=? WHERE id=?",
            (
                "Target issue body",
                "Target acceptance criteria",
                1i8,
                "Target hold reason",
                &issue,
            ),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issues (id, number, area_id, parent_id, title, body, type, status, revision, created_at, updated_at)
             VALUES (?, 5, ?, ?, 'Child secret title', 'Child secret body', 'task', 'todo', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (id(0x20), id(3), &issue, &revision),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issues (id, number, area_id, title, body, type, status, revision, created_at, updated_at)
             VALUES (?, 6, ?, 'Dependent secret title', 'Dependent secret body', 'task', 'todo', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (id(0x21), id(3), &revision),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issue_dependencies VALUES (?, ?)",
            (id(0x21), &issue),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO projects (id, name, description, status, revision)
             VALUES (?, 'Secret project name', 'Secret project description', 'active', ?)",
            (id(0x30), &revision),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO project_issues VALUES (?, ?)",
            (id(0x30), &issue),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO labels (id, name, description, revision)
             VALUES (?, 'secret-label-name', 'Secret label description', ?)",
            (id(0x40), &revision),
        )
        .unwrap();
    admin
        .exec_drop("INSERT INTO issue_labels VALUES (?, ?)", (&issue, id(0x40)))
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO resources (id, kind, name, uri, revision)
             VALUES (?, 'repo', 'Secret resource name', 'file:///secret/resource/path', ?)",
            (id(0x50), &revision),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issue_resources VALUES (?, ?)",
            (&issue, id(0x50)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO artifacts (id, kind, title, body, author_ref, source_ref, revision, created_at, substantive_updated_at)
             VALUES (?, 'handoff', 'Secret artifact title', 'Secret artifact body', 'secret-artifact-author', 'secret-source-ref', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (id(0x60), &revision),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO artifact_issues VALUES (?, ?)",
            (id(0x60), &issue),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issue_updates (id, issue_id, kind, body, author_ref, created_at, transition_json) VALUES
             (?, ?, 'progress', 'Secret update body one', 'secret-update-author', '2026-01-01 00:00:01.000000', NULL),
             (?, ?, 'transition', 'Secret update body two', 'secret-update-author', '2026-01-01 00:00:02.000000', '{\"secret\":\"transition\"}')",
            (id(0x70), &issue, id(0x71), &issue),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO directives (id, scope_kind, area_id, title, body, status, issuer_ref, source_ref, approval_ref, revision, created_at, updated_at)
             VALUES (?, 'area', ?, 'Secret directive title', 'Secret directive body', 'active', 'secret-issuer', 'secret-source', 'secret-approval', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (id(0x80), id(3), &revision),
        )
        .unwrap();
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn reads_issue_detail_and_bounded_relation_ids_across_collations_without_leaking_adjacent_content()
{
    for collation in ["utf8mb4_0900_bin", CASE_INSENSITIVE] {
        let fixture = Fixture::start_with_collation(collation);
        let mut admin = fixture.connect(ADMIN);
        seed_relations(&mut admin);
        commit(&mut admin);

        let before = storage_state(&mut admin);
        let by_uuid = check_issue_show(&fixture, &id(11), 0, &mut admin);
        let by_number = check_issue_show(&fixture, "#2", 0, &mut admin);
        assert_eq!(by_uuid, by_number);
        assert_eq!(by_uuid["context"]["workspace_id"], id(1));
        assert_eq!(by_uuid["context"]["db_commit"], before.0);
        assert_eq!(by_uuid["context"]["scope"], "issue");

        let data = &by_uuid["data"];
        assert_eq!(data["issue"]["id"], id(11));
        assert_eq!(data["issue"]["number"], 2);
        assert_eq!(data["issue"]["area_id"], id(3));
        assert_eq!(data["issue"]["parent_id"], id(10));
        assert_eq!(data["issue"]["title"], "Child");
        assert_eq!(data["issue"]["body"], "Target issue body");
        assert_eq!(
            data["issue"]["acceptance_criteria"],
            "Target acceptance criteria"
        );
        assert_eq!(data["issue"]["type"], "task");
        assert_eq!(data["issue"]["status"], "in-progress");
        assert_eq!(data["issue"]["priority"], 1);
        assert_eq!(data["issue"]["hold_reason"], "Target hold reason");
        assert_eq!(data["claim_id"], id(14));
        assert_eq!(data["children"]["items"], serde_json::json!([id(0x20)]));
        assert_eq!(data["children"]["truncated"], false);
        assert_eq!(data["prerequisites"]["items"], serde_json::json!([id(12)]));
        assert_eq!(data["dependents"]["items"], serde_json::json!([id(0x21)]));
        assert_eq!(data["projects"]["items"], serde_json::json!([id(0x30)]));
        assert_eq!(data["labels"]["items"], serde_json::json!([id(0x40)]));
        assert_eq!(data["resources"]["items"], serde_json::json!([id(0x50)]));
        assert_eq!(data["artifacts"]["items"], serde_json::json!([id(0x60)]));
        // Updates are deterministically ordered newest-first.
        assert_eq!(
            data["updates"]["items"],
            serde_json::json!([id(0x71), id(0x70)])
        );

        let text_out = show_issue(&fixture, "#2", false);
        assert_eq!(text_out.status.code(), Some(0));
        assert!(text_out.stderr.is_empty());
        let text_stdout = String::from_utf8(text_out.stdout).unwrap();
        assert!(text_stdout.contains("issue show"));

        let wire = by_uuid.to_string();
        for forbidden in [
            "fixture-holder",
            "Child secret title",
            "Child secret body",
            "Dependent secret title",
            "Dependent secret body",
            "Secret project name",
            "Secret project description",
            "secret-label-name",
            "Secret label description",
            "Secret resource name",
            "file:///secret/resource/path",
            "Secret artifact title",
            "Secret artifact body",
            "secret-artifact-author",
            "secret-source-ref",
            "Secret update body one",
            "Secret update body two",
            "secret-update-author",
            "transition_json",
            "Secret directive title",
            "Secret directive body",
            "secret-approval",
            "Prerequisite",
            "Parent",
        ] {
            assert!(!wire.contains(forbidden), "{collation}: leaked {forbidden}");
            assert!(
                !text_stdout.contains(forbidden),
                "{collation}: text leaked {forbidden}"
            );
        }

        let closed = check_issue_show(&fixture, "#3", 0, &mut admin);
        assert_eq!(closed["data"]["issue"]["status"], "done");
        assert_eq!(closed["data"]["issue"]["closed_by_ref"], "fixture-reviewer");
        assert_eq!(closed["data"]["issue"]["resolution_note"], "Accepted");
        assert!(closed["data"]["issue"].get("closed_at").is_some());
        assert!(closed["data"].get("claim_id").is_none());
        assert_eq!(storage_state(&mut admin), before);
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn reads_only_committed_snapshot_and_isolates_working_and_staged_changes() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let baseline = check_issue_show(&fixture, &id(10), 0, &mut admin);
    let uncommitted_id = id(0x2a);
    let revision = id(4);

    admin
        .exec_drop(
            "UPDATE issues SET title='Dirty title' WHERE id=?",
            (id(10),),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
             VALUES (?, 99, ?, 'Uncommitted issue', 'task', 'todo', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (&uncommitted_id, id(2), &revision),
        )
        .unwrap();

    // Dirty working tree changes must not appear.
    assert_eq!(check_issue_show(&fixture, &id(10), 0, &mut admin), baseline);
    let missing_dirty = check_issue_show(&fixture, &uncommitted_id, 3, &mut admin);
    assert_eq!(missing_dirty["error"]["code"], "NOT_FOUND");
    assert_eq!(
        check_issue_show(&fixture, "#99", 3, &mut admin)["error"]["code"],
        "NOT_FOUND"
    );

    // Staged uncommitted changes must not appear either.
    admin.query_drop("CALL DOLT_ADD('issues')").unwrap();
    assert_eq!(check_issue_show(&fixture, &id(10), 0, &mut admin), baseline);
    assert_eq!(
        check_issue_show(&fixture, &uncommitted_id, 3, &mut admin)["error"]["code"],
        "NOT_FOUND"
    );

    commit(&mut admin);
    let committed = check_issue_show(&fixture, &id(10), 0, &mut admin);
    assert_eq!(committed["data"]["issue"]["title"], "Dirty title");
    assert_ne!(
        committed["context"]["db_commit"],
        baseline["context"]["db_commit"]
    );
    assert_eq!(
        check_issue_show(&fixture, "#99", 0, &mut admin)["data"]["issue"]["id"],
        uncommitted_id
    );
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn bounds_relation_ids_and_reports_typed_limit_missing_and_invalid_diagnostics() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let issue = id(10);
    let revision = id(4);

    let mut label_ids = Vec::new();
    for offset in 0..27u32 {
        let label_id = id(0x100 + offset);
        let label_name = format!("fixture-label-{offset:02}");
        admin
            .exec_drop(
                "INSERT INTO labels (id, name, revision) VALUES (?, ?, ?)",
                (&label_id, &label_name, &revision),
            )
            .unwrap();
        admin
            .exec_drop(
                "INSERT INTO issue_labels VALUES (?, ?)",
                (&issue, &label_id),
            )
            .unwrap();
        label_ids.push(label_id);
    }
    commit(&mut admin);

    let bounded = check_issue_show(&fixture, "#1", 0, &mut admin);
    let returned = bounded["data"]["labels"]["items"].as_array().unwrap();
    assert_eq!(returned.len(), 25);
    assert_eq!(bounded["data"]["labels"]["truncated"], true);
    let expected_first_25: Vec<_> = label_ids[..25]
        .iter()
        .map(|id| Value::String(id.clone()))
        .collect();
    assert_eq!(*returned, expected_first_25);

    // Oversized Issue body triggers CONTEXT_LIMIT without echoing the body.
    let huge_body = "x".repeat(65_537);
    admin
        .exec_drop("UPDATE issues SET body=? WHERE id=?", (&huge_body, &issue))
        .unwrap();
    commit(&mut admin);
    let limit_err = check_issue_show(&fixture, "#1", 2, &mut admin);
    assert_eq!(limit_err["error"]["code"], "CONTEXT_LIMIT");
    assert_eq!(limit_err["context"]["workspace_id"], id(1));
    assert!(!limit_err.to_string().contains(&huge_body));

    // Missing selector -> NOT_FOUND.
    let not_found = check_issue_show(&fixture, "#999999", 3, &mut admin);
    assert_eq!(not_found["error"]["code"], "NOT_FOUND");
    assert_eq!(not_found["context"]["workspace_id"], id(1));

    // Wrong credentials, wrong workspace ID, unreachable port.
    let wrong_pw = show_issue_with(
        &fixture,
        "#2",
        &id(1),
        "secret-bad-password",
        fixture.port,
        true,
    );
    let wrong_pw_json: Value = serde_json::from_slice(&wrong_pw.stdout).unwrap();
    assert_eq!(wrong_pw.status.code(), Some(6));
    assert_eq!(wrong_pw_json["error"]["code"], "UNAUTHORIZED");
    assert!(!wrong_pw_json.to_string().contains("secret-bad-password"));

    let wrong_ws = show_issue_with(
        &fixture,
        "#2",
        &id(99),
        &fixture.password,
        fixture.port,
        true,
    );
    let wrong_ws_json: Value = serde_json::from_slice(&wrong_ws.stdout).unwrap();
    assert_eq!(wrong_ws.status.code(), Some(7));
    assert_eq!(wrong_ws_json["error"]["code"], "AUTHORITY_UNVERIFIED");

    let unused = TcpListener::bind((HOST, 0)).unwrap();
    let offline = show_issue_with(
        &fixture,
        "#2",
        &id(1),
        &fixture.password,
        unused.local_addr().unwrap().port(),
        true,
    );
    let offline_json: Value = serde_json::from_slice(&offline.stdout).unwrap();
    assert_eq!(offline.status.code(), Some(7));
    assert_eq!(offline_json["error"]["code"], "RUNTIME_UNAVAILABLE");

    // Noncanonical committed Issue record -> CONSTRAINT_VIOLATION.
    admin
        .exec_drop("UPDATE issues SET type='FEATURE' WHERE id=?", (id(13),))
        .unwrap();
    commit(&mut admin);
    let invalid_rec = check_issue_show(&fixture, "#4", 4, &mut admin);
    assert_eq!(invalid_rec["error"]["code"], "CONSTRAINT_VIOLATION");
    assert_eq!(
        invalid_rec["error"]["details"]["diagnostics"][0]["code"],
        "INVALID_ISSUE_TYPE"
    );
    assert!(!invalid_rec.to_string().contains("FEATURE"));
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn preserves_exact_uuid_identity_under_case_insensitive_collation() {
    let fixture = Fixture::start_with_collation(CASE_INSENSITIVE);
    let mut admin = fixture.connect(ADMIN);
    let upper_issue_id = id(0x6a).to_ascii_uppercase();
    let lower_issue_id = id(0x6a);
    admin
        .exec_drop(
            "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
             VALUES (?, 77, ?, 'Upper ID', 'task', 'todo', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (&upper_issue_id, id(2), id(4)),
        )
        .unwrap();

    // Aliasing a lowercase issue's FK with an uppercase ID under ai_ci.
    let project_id = id(0x61);
    admin
        .exec_drop(
            "INSERT INTO projects (id, name, status, revision) VALUES (?, 'P', 'active', ?)",
            (&project_id, id(4)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO project_issues VALUES (?, ?)",
            (&project_id, id(12).to_ascii_uppercase()),
        )
        .unwrap();
    commit(&mut admin);

    // Lowercase UUID lookup must NOT match the uppercase stored ID.
    let alias_lookup = check_issue_show(&fixture, &lower_issue_id, 3, &mut admin);
    assert_eq!(alias_lookup["error"]["code"], "NOT_FOUND");

    // #77 lookup matches the row and rejects the noncanonical uppercase ID.
    let by_number = check_issue_show(&fixture, "#77", 4, &mut admin);
    assert_eq!(by_number["error"]["code"], "CONSTRAINT_VIOLATION");
    assert_eq!(
        by_number["error"]["details"]["diagnostics"][0]["code"],
        "INVALID_ID"
    );
    assert!(!by_number.to_string().contains(&upper_issue_id));

    // Uppercase FK aliasing on id(12) is rejected rather than silently accepted or dropped.
    let fk_alias = check_issue_show(&fixture, &id(12), 4, &mut admin);
    assert_eq!(fk_alias["error"]["code"], "CONSTRAINT_VIOLATION");
    assert_eq!(
        fk_alias["error"]["details"]["diagnostics"][0]["code"],
        "INVALID_ID"
    );
    assert!(!fk_alias.to_string().contains(&id(12).to_ascii_uppercase()));
}

use mysql::prelude::Queryable;
use serde_json::{Value, json};

use super::{ADMIN, DATABASE, Fixture, commit, id, storage_state};

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn lists_exact_committed_labels_and_rejects_broken_scope_paths() {
    let fixture = Fixture::start();
    let mut writer = fixture.connect(ADMIN);
    let (area, issue, child, sibling, other) = (id(2), id(10), id(11), id(12), id(13));
    let (project, shared, local, cross_area, unrelated, unlinked, revision) =
        (id(30), id(40), id(41), id(42), id(43), id(44), id(4));
    let empty_project = id(31);
    let name = "Équipe  Alpha";
    writer.query_drop(format!(
        "INSERT INTO projects (id, name, status, revision) VALUES ('{project}', 'Fixture', 'active', '{revision}'),
              ('{empty_project}', 'Empty fixture', 'active', '{revision}')"
    )).unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO project_issues VALUES ('{project}', '{issue}'), ('{project}', '{child}')"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO labels VALUES
         ('{shared}', '{name}', 'Fixture description', '{revision}'),
         ('{local}', 'Local', NULL, '{revision}'),
         ('{cross_area}', 'Cross Area', NULL, '{revision}'),
         ('{unrelated}', 'Unrelated', NULL, '{revision}'),
         ('{unlinked}', 'Unlinked', NULL, '{revision}')"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO issue_labels VALUES
         ('{issue}', '{shared}'), ('{issue}', '{local}'),
         ('{sibling}', '{shared}'), ('{child}', '{shared}'),
         ('{child}', '{cross_area}'), ('{other}', '{unrelated}')"
        ))
        .unwrap();
    commit(&mut writer);
    writer
        .exec_drop("UPDATE labels SET name = 'Staged' WHERE id = ?", (&shared,))
        .unwrap();
    writer.query_drop("CALL DOLT_ADD('labels')").unwrap();
    writer
        .exec_drop(
            "DELETE FROM issue_labels WHERE issue_id = ? AND label_id = ?",
            (&issue, &local),
        )
        .unwrap();

    let workspace = id(1);
    let port = fixture.port.to_string();
    let invoke = |options: &[&str], workspace: &str, password: &str| {
        fixture.run_hive(
            &[
                &["label", "list"],
                options,
                &[
                    "--database",
                    DATABASE,
                    "--port",
                    &port,
                    "--workspace-id",
                    workspace,
                    "--contract",
                    "2",
                    "--json",
                ],
            ]
            .concat(),
            password,
        )
    };
    let run = |writer: &mut _, options: &[&str], exit| {
        let before = storage_state(writer);
        let output = invoke(options, &workspace, &fixture.password);
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {output:?}"));
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert!(output.stderr.is_empty());
        assert_eq!(report["command"], "label list");
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["ok"], exit == 0);
        assert_eq!(report["context"]["workspace_id"], workspace);
        assert_eq!(report["context"]["db_commit"], before.0);
        assert_eq!(storage_state(writer), before);
        report
    };
    let ids = |report: &Value| {
        assert_eq!(
            report["data"]["observed_revision"],
            report["context"]["db_commit"]
        );
        report["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                assert_eq!(item.as_object().unwrap().len(), 3);
                assert_eq!(item["revision"], revision);
                item["id"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>()
    };

    let first = run(&mut writer, &["--area", &area, "--limit", "1"], 0);
    assert_eq!(first["context"]["scope"], json!({"kind":"area","id":area}));
    assert_eq!(
        first["data"]["items"],
        json!([{"id":shared,"name":name,"revision":revision}])
    );
    assert_eq!(first["data"]["truncated"], true);
    assert_eq!(
        run(&mut writer, &["--area", &area, "--limit", "1"], 0),
        first
    );
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    let second = run(
        &mut writer,
        &["--area", &area, "--limit", "1", "--cursor", cursor],
        0,
    );
    assert_eq!(ids(&second), [local.as_str()]);
    assert_eq!(second["data"]["truncated"], false);
    assert!(second["data"]["next_cursor"].is_null());
    assert_eq!(
        ids(&run(&mut writer, &["--issue", &issue], 0)),
        [&shared, &local].map(String::as_str)
    );
    assert_eq!(
        ids(&run(&mut writer, &["--project", &project], 0)),
        [&shared, &local, &cross_area].map(String::as_str)
    );
    assert_eq!(
        ids(&run(&mut writer, &["--all"], 0)),
        [&shared, &local, &cross_area, &unrelated, &unlinked].map(String::as_str)
    );

    let empty = run(&mut writer, &["--project", &empty_project], 0);
    assert!(ids(&empty).is_empty());
    assert!(empty["data"]["next_cursor"].is_null());

    let before = storage_state(&mut writer);
    for (workspace, password, exit, code) in [
        (id(99), fixture.password.as_str(), 7, "AUTHORITY_UNVERIFIED"),
        (id(1), "wrong-fixture-password", 6, "UNAUTHORIZED"),
    ] {
        let output = invoke(&["--all"], &workspace, password);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert_eq!(report["error"]["code"], code, "{report}");
        assert!(report["data"].is_null());
        assert_eq!(storage_state(&mut writer), before);
    }

    let missing = id(99);
    for flag in ["--area", "--issue", "--project"] {
        assert_eq!(
            run(&mut writer, &[flag, &missing], 3)["error"]["code"],
            "NOT_FOUND"
        );
    }
    for options in [
        ["--issue", &issue, "--limit", "1", "--cursor", cursor],
        ["--area", &area, "--limit", "2", "--cursor", cursor],
        ["--area", &area, "--limit", "1", "--cursor", "tampered"],
    ] {
        assert_eq!(
            run(&mut writer, &options, 5)["error"]["code"],
            "STALE_CURSOR"
        );
    }

    commit(&mut writer);
    assert_eq!(
        run(
            &mut writer,
            &["--area", &area, "--limit", "1", "--cursor", cursor],
            5
        )["error"]["code"],
        "STALE_CURSOR"
    );
    writer
        .query_drop("SET SESSION foreign_key_checks=0")
        .unwrap();
    writer
        .query_drop("SET SESSION dolt_force_transaction_commit=1")
        .unwrap();
    // Leave a dangling Label link and a deleted Issue with surviving membership
    // and Label links. The deleted Issue cannot confer Label visibility.
    writer
        .exec_drop("DELETE FROM labels WHERE id = ?", (&cross_area,))
        .unwrap();
    writer
        .exec_drop("DELETE FROM issues WHERE id = ?", (&issue,))
        .unwrap();
    writer
        .exec_drop(
            "UPDATE labels SET name = ' malformed ' WHERE id = ?",
            (&unrelated,),
        )
        .unwrap();
    commit(&mut writer);

    assert_eq!(
        run(&mut writer, &["--issue", &issue], 3)["error"]["code"],
        "NOT_FOUND"
    );
    assert_eq!(
        run(&mut writer, &["--project", &project], 4)["error"]["code"],
        "INVALID_RECORD"
    );
    assert_eq!(
        run(&mut writer, &["--issue", &child], 4)["error"]["code"],
        "INVALID_RECORD"
    );
    assert_eq!(
        run(&mut writer, &["--issue", &other], 4)["error"]["code"],
        "INVALID_RECORD"
    );
    assert_eq!(
        ids(&run(&mut writer, &["--area", &area], 0)),
        [shared.as_str()]
    );

    // A case-insensitive Label lookup could make this malformed link appear
    // valid. Only the byte-identical endpoint is eligible.
    let alias = unlinked.to_uppercase();
    writer
        .exec_drop("INSERT INTO issue_labels VALUES (?, ?)", (&sibling, &alias))
        .unwrap();
    commit(&mut writer);
    let report = run(&mut writer, &["--issue", &sibling], 4);
    assert_eq!(report["error"]["code"], "INVALID_RECORD");
    assert!(!report.to_string().contains(&alias));
    // An existing Issue with malformed spelling fails the Area membership
    // check even when no exact Issue UUID could select it directly.
    let malformed_issue = id(66).replace("-7000-", "-4000-");
    writer.exec_drop(
        "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
         VALUES (?, 5, ?, 'Malformed fixture', 'task', 'todo', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
        (&malformed_issue, &area, &revision),
    ).unwrap();
    commit(&mut writer);
    let report = run(&mut writer, &["--area", &area], 4);
    assert_eq!(report["error"]["code"], "INVALID_RECORD");
    assert!(!report.to_string().contains(&malformed_issue));
    writer
        .exec_drop(
            "INSERT INTO labels VALUES ('', 'Malformed ID fixture', NULL, ?)",
            (&revision,),
        )
        .unwrap();
    commit(&mut writer);
    assert_eq!(
        run(&mut writer, &["--all", "--limit", "1"], 4)["error"]["code"],
        "INVALID_RECORD"
    );
}

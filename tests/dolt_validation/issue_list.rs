use mysql::prelude::Queryable;
use serde_json::Value;

use super::{ADMIN, DATABASE, Fixture, commit, id, storage_state};

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn lists_exact_committed_issue_scopes_with_stable_pages() {
    let fixture = Fixture::start();
    let mut writer = fixture.connect(ADMIN);
    let run = |options: &[&str], exit| {
        let before = storage_state(&mut fixture.connect(ADMIN));
        let mut args = vec!["--contract", "2", "--json", "issue", "list"];
        let port = fixture.port.to_string();
        let workspace = id(1);
        args.extend([
            "--workspace-id",
            &workspace,
            "--database",
            DATABASE,
            "--port",
            &port,
        ]);
        args.extend(options);
        let output = fixture.run_hive(&args, &fixture.password);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert!(output.stderr.is_empty(), "{report}");
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], "issue list");
        assert_eq!(report["ok"], exit == 0);
        if exit == 0 {
            assert_eq!(report["context"]["workspace_id"], workspace);
            assert_eq!(report["context"]["db_commit"], before.0);
            assert_eq!(report["data"]["observed_revision"], before.0);
        }
        assert_eq!(storage_state(&mut fixture.connect(ADMIN)), before);
        report
    };

    let area = id(2);
    let project = id(30);
    let other_project = id(31);
    let case_mismatched_project = id(42);
    let label = id(32);
    let revision = id(4);
    let (parent, child, prerequisite, dependent) = (id(10), id(11), id(12), id(13));
    for sql in [
        format!(
            "INSERT INTO projects (id, name, status, revision) VALUES
             ('{project}', 'Selected', 'active', '{revision}'),
             ('{other_project}', 'Adjacent', 'active', '{revision}'),
             ('{}', 'Malformed ID', 'active', '{revision}')",
            case_mismatched_project.to_ascii_uppercase()
        ),
        format!(
            "INSERT INTO project_issues VALUES
             ('{project}', '{child}'), ('{project}', '{prerequisite}'),
             ('{other_project}', '{child}'), ('{other_project}', '{dependent}')"
        ),
        format!("INSERT INTO labels (id, name, revision) VALUES ('{label}', 'Résumé', '{revision}')"),
        format!("INSERT INTO issue_labels VALUES ('{child}', '{label}'), ('{dependent}', '{label}')"),
        "UPDATE issues SET created_at = '2025-01-01 00:00:00.000002', body = 'private body', acceptance_criteria = 'private criteria'".to_owned(),
        format!("UPDATE issues SET priority = 0 WHERE id IN ('{parent}', '{child}', '{dependent}')"),
        format!("UPDATE issues SET created_at = '2025-01-01 00:00:00.000001' WHERE id = '{dependent}'"),
    ] {
        writer.query_drop(sql).unwrap();
    }
    commit(&mut writer);

    let numbers = |report: &Value| {
        report["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                assert!(item.get("body").is_none() && item.get("acceptance_criteria").is_none());
                assert!(item.get("projects").is_none() && item.get("labels").is_none());
                item["number"].as_u64().unwrap()
            })
            .collect::<Vec<_>>()
    };

    let first = run(&["--all", "--limit", "2"], 0);
    assert_eq!(numbers(&first), [4, 1]);
    assert_eq!(first["data"]["truncated"], true);
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    assert_eq!(run(&["--all", "--limit", "2"], 0), first);

    // Changes in the staged and working roots must neither leak nor stale the cursor.
    writer
        .query_drop(format!(
            "UPDATE issues SET title = 'staged title' WHERE id = '{child}'"
        ))
        .unwrap();
    writer.query_drop("CALL DOLT_ADD('issues')").unwrap();
    writer
        .query_drop(format!(
            "UPDATE issues SET title = 'working title' WHERE id = '{child}'"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO project_issues VALUES ('{project}', '{dependent}')"
        ))
        .unwrap();
    let second = run(&["--all", "--limit", "2", "--cursor", cursor], 0);
    assert_eq!(numbers(&second), [2, 3]);
    assert_eq!(second["data"]["items"][0]["title"], "Child");
    assert_eq!(second["data"]["items"][1]["priority"], Value::Null);
    assert_eq!(second["data"]["truncated"], false);
    assert_eq!(second["data"]["next_cursor"], Value::Null);

    assert_eq!(numbers(&run(&["--area", &area], 0)), [1, 3]);
    assert_eq!(numbers(&run(&["--project", &project], 0)), [2, 3]);
    let unknown = run(&["--project", &case_mismatched_project], 3);
    assert_eq!(unknown["error"]["code"], "NOT_FOUND");

    let filters = [
        "--project",
        &project,
        "--status",
        "in-progress",
        "--type",
        "task",
        "--priority",
        "0",
        "--label",
    ];
    let mut options = filters.to_vec();
    options.push("Résumé");
    assert_eq!(numbers(&run(&options, 0)), [2]);
    options.pop();
    options.push("résumé");
    assert!(numbers(&run(&options, 0)).is_empty());

    for options in [
        [
            "--all", "--limit", "2", "--status", "todo", "--cursor", cursor,
        ]
        .as_slice(),
        &["--all", "--limit", "1", "--cursor", cursor],
        &["--area", &area, "--limit", "2", "--cursor", cursor],
        &["--all", "--limit", "2", "--cursor", "malformed"],
    ] {
        assert_eq!(run(options, 5)["error"]["code"], "STALE_CURSOR");
    }

    commit(&mut writer);
    let stale = run(&["--all", "--limit", "2", "--cursor", cursor], 5);
    assert_eq!(stale["error"]["code"], "STALE_CURSOR");
    assert_ne!(stale["context"]["db_commit"], first["context"]["db_commit"]);

    // The second sorted row is the lookahead on a one-item page.
    let malformed = id(14);
    writer
        .exec_drop(
            "INSERT INTO issues
             (id, number, area_id, title, type, status, priority, revision, created_at, updated_at)
             VALUES (?, 5, ?, 'Malformed', 'EPIC', 'todo', 0, ?,
                     '2025-01-01 00:00:00.000001', '2025-01-01 00:00:00.000001')",
            (&malformed, &area, &revision),
        )
        .unwrap();
    commit(&mut writer);
    let stored_type: String = writer
        .exec_first("SELECT type FROM issues WHERE id = ?", (&malformed,))
        .unwrap()
        .unwrap();
    assert_eq!(stored_type, "EPIC");
    let invalid = run(&["--all", "--limit", "1"], 4);
    assert_eq!(invalid["error"]["code"], "INVALID_RECORD");
    assert_eq!(
        invalid["context"]["db_commit"],
        storage_state(&mut writer).0
    );
    assert!(invalid.get("data").is_none());
    assert!(!invalid.to_string().contains("EPIC"));
    assert!(!invalid.to_string().contains(&malformed));
}

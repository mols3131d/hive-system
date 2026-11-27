use mysql::prelude::Queryable;
use serde_json::{Value, json};

use super::{ADMIN, DATABASE, Fixture, commit, id, storage_state};

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn reads_committed_resources_in_exact_scope() {
    let fixture = Fixture::start();
    let mut writer = fixture.connect(ADMIN);
    let (area, child_area, issue, child_issue) = (id(2), id(3), id(10), id(11));
    let (shared, area_only, issue_only, adjacent, standalone, revision) =
        (id(20), id(21), id(22), id(23), id(24), id(25));
    let uri = "file:///synthetic-resource-directory/selected-record.txt";
    writer.query_drop(format!(
        "INSERT INTO resources VALUES
         ('{shared}', 'document', 'Shared', '{uri}', '{revision}'),
         ('{area_only}', 'directory', 'Area fixture', NULL, '{revision}'),
         ('{issue_only}', 'repository', 'Issue fixture', NULL, '{revision}'),
         ('{adjacent}', 'document', 'Adjacent fixture', 'https://example.invalid/adjacent', '{revision}'),
         ('{standalone}', 'document', 'Standalone fixture', NULL, '{revision}')"
    )).unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO area_resources VALUES
         ('{area}', '{shared}'), ('{area}', '{area_only}'),
         ('{child_area}', '{shared}'), ('{child_area}', '{adjacent}')"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO issue_resources VALUES
         ('{issue}', '{shared}'), ('{issue}', '{issue_only}'),
         ('{child_issue}', '{shared}'), ('{child_issue}', '{adjacent}')"
        ))
        .unwrap();
    commit(&mut writer);
    // Exercise staged link changes and unstaged metadata in the writer's workspace.
    writer
        .exec_drop(
            "DELETE FROM area_resources WHERE area_id = ? AND resource_id = ?",
            (&area, &area_only),
        )
        .unwrap();
    writer
        .query_drop("CALL DOLT_ADD('area_resources')")
        .unwrap();
    writer
        .exec_drop(
            "UPDATE resources SET name = 'Uncommitted' WHERE id = ?",
            (&shared,),
        )
        .unwrap();
    let run = |writer: &mut _, command: &str, options: &[&str], exit| {
        let before = storage_state(writer);
        let workspace = id(1);
        let port = fixture.port.to_string();
        let mut args = vec![
            "--contract",
            "2",
            "--json",
            "resource",
            command,
            "--workspace-id",
            &workspace,
            "--database",
            DATABASE,
            "--port",
            &port,
        ];
        args.extend(options);
        let output = fixture.run_hive(&args, &fixture.password);
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {output:?}"));
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert!(output.stderr.is_empty(), "{report}");
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], format!("resource {command}"));
        assert_eq!(report["ok"], exit == 0);
        assert_eq!(report["context"]["workspace_id"], workspace);
        assert_eq!(report["context"]["db_commit"], before.0);
        if command == "list" && exit == 0 {
            assert_eq!(report["data"]["observed_revision"], before.0);
        }
        assert_eq!(storage_state(writer), before);
        report
    };
    let items = |report: Value| {
        report["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                assert_eq!(item.as_object().unwrap().len(), 4, "{item}");
                assert!(item["kind"].is_string() && item["name"].is_string());
                assert_eq!(item["revision"], revision);
                item["id"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>()
    };

    let first = run(&mut writer, "list", &["--area", &area, "--limit", "1"], 0);
    assert_eq!(first["context"]["scope"], json!({"kind":"area", "id":area}));
    assert_eq!(first["data"]["truncated"], true);
    assert_eq!(first["data"]["items"][0]["name"], "Shared");
    assert_eq!(items(first.clone()), [shared.as_str()]);
    assert_eq!(
        run(&mut writer, "list", &["--area", &area, "--limit", "1"], 0),
        first
    );
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    let second = run(
        &mut writer,
        "list",
        &["--area", &area, "--limit", "1", "--cursor", cursor],
        0,
    );
    assert_eq!(second["data"]["truncated"], false);
    assert!(second["data"]["next_cursor"].is_null());
    assert_eq!(items(second), [area_only.as_str()]);

    let scoped = run(&mut writer, "list", &["--issue", &issue], 0);
    assert_eq!(
        scoped["context"]["scope"],
        json!({"kind":"issue", "id":issue})
    );
    assert_eq!(items(scoped), [shared.as_str(), issue_only.as_str()]);
    assert_eq!(
        items(run(&mut writer, "list", &["--all"], 0)),
        [&shared, &area_only, &issue_only, &adjacent, &standalone].map(String::as_str)
    );
    assert!(items(run(&mut writer, "list", &["--issue", &id(12)], 0)).is_empty());
    for option in ["--area", "--issue"] {
        assert_eq!(
            run(&mut writer, "list", &[option, &id(99)], 3)["error"]["code"],
            "NOT_FOUND"
        );
    }
    assert_eq!(
        run(&mut writer, "show", &[&id(99)], 3)["error"]["code"],
        "NOT_FOUND"
    );
    assert_eq!(
        run(&mut writer, "show", &[shared.as_str()], 0)["data"],
        json!({
            "resource": {"id":shared, "kind":"document", "name":"Shared", "revision":revision, "uri":uri}
        })
    );
    assert!(run(&mut writer, "show", &[&standalone], 0)["data"]["resource"]["uri"].is_null());

    for options in [
        ["--issue", &issue, "--limit", "1", "--cursor", cursor],
        ["--area", &area, "--limit", "2", "--cursor", cursor],
    ] {
        assert_eq!(
            run(&mut writer, "list", &options, 5)["error"]["code"],
            "STALE_CURSOR"
        );
    }
    commit(&mut writer);
    assert_eq!(
        run(
            &mut writer,
            "list",
            &["--area", &area, "--limit", "1", "--cursor", cursor],
            5
        )["error"]["code"],
        "STALE_CURSOR"
    );
}

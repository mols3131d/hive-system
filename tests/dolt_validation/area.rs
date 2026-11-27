use mysql::prelude::Queryable;
use serde_json::Value;

use super::{
    ADMIN, CASE_INSENSITIVE, DATABASE, Fixture, commit, empty_seed_sql, id, storage_state,
};

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn lists_and_shows_committed_areas_without_writes() {
    for collation in ["utf8mb4_0900_bin", CASE_INSENSITIVE] {
        check_area_reads(collation);
    }
}

fn check_area_reads(collation: &str) {
    let fixture = Fixture::start_with_seed(&empty_seed_sql(collation));
    let mut writer = fixture.connect(ADMIN);
    let run = |command: &str, options: &[&str], exit| {
        let before = storage_state(&mut fixture.connect(ADMIN));
        let workspace = id(1);
        let port = fixture.port.to_string();
        let mut args = vec![
            "--contract",
            "2",
            "--json",
            "area",
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
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{collation}: {report}");
        assert!(output.stderr.is_empty(), "{report}");
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], format!("area {command}"));
        assert_eq!(report["ok"], exit == 0);
        if exit == 0 {
            assert_eq!(report["context"]["workspace_id"], workspace);
            assert_eq!(report["context"]["db_commit"], before.0);
            if command == "list" {
                assert_eq!(report["data"]["observed_revision"], before.0);
            }
        }
        assert_eq!(storage_state(&mut fixture.connect(ADMIN)), before);
        report
    };
    let items = |report: Value| {
        report["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                assert!(item.get("description").is_none());
                item["id"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>()
    };

    assert!(items(run("list", &[], 0)).is_empty());
    let (root, leaf, child, grandchild, removed, revision) =
        (id(10), id(11), id(12), id(13), id(14), id(15));
    writer
        .query_drop(format!(
            "INSERT INTO areas (id, slug, name, description, revision) VALUES
             ('{root}', 'z-root', 'Root', 'Fixture detail', '{revision}'),
             ('{leaf}', 'a-leaf', 'Leaf', NULL, '{revision}'),
             ('{removed}', 'removed', 'Removed', NULL, '{revision}')"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO areas (id, parent_id, slug, name, archived_at, revision) VALUES
             ('{child}', '{root}', 'child', 'Child', '2025-01-01 00:00:00.000001', '{revision}'),
             ('{grandchild}', '{child}', 'grandchild', 'Grandchild', NULL, '{revision}')"
        ))
        .unwrap();
    if collation == CASE_INSENSITIVE {
        writer
            .exec_drop(
                "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES
                 (?, ?, 'alias-child', 'Case-mismatched parent', ?)",
                (id(16), root.to_ascii_uppercase(), &revision),
            )
            .unwrap();
    }
    commit(&mut writer);

    let first = run("list", &["--limit", "1"], 0);
    assert_eq!(first["context"]["scope"]["kind"], "roots");
    assert_eq!(items(first.clone()), [leaf.as_str()]);
    assert_eq!(first["data"]["truncated"], true);
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    assert_eq!(run("list", &["--limit", "1"], 0), first);

    writer
        .exec_drop("DELETE FROM areas WHERE id = ?", (&removed,))
        .unwrap();
    writer.query_drop("CALL DOLT_ADD('areas')").unwrap();
    writer
        .exec_drop(
            "UPDATE areas SET name = 'Uncommitted' WHERE id = ?",
            (&root,),
        )
        .unwrap();

    let second = run("list", &["--limit", "1", "--cursor", cursor], 0);
    assert_eq!(items(second.clone()), [removed.as_str()]);
    let next = second["data"]["next_cursor"].as_str().unwrap();
    let third = run("list", &["--limit", "1", "--cursor", next], 0);
    assert_eq!(third["data"]["next_cursor"], Value::Null);
    assert_eq!(items(third), [root.as_str()]);
    assert_eq!(
        run("show", &[&removed], 0)["data"]["area"]["name"],
        "Removed"
    );
    let detail = run("show", &[&root], 0);
    assert_eq!(detail["data"]["area"]["name"], "Root");
    assert_eq!(detail["data"]["area"]["description"], "Fixture detail");
    assert_eq!(detail["data"]["area"]["revision"], revision);
    assert_eq!(detail["data"]["area"]["parent_id"], Value::Null);

    let children = run("list", &["--parent", &root], 0);
    assert_eq!(children["context"]["scope"]["id"], root);
    assert_eq!(
        children["data"]["items"][0]["archived_at"],
        "2025-01-01T00:00:00.000001Z"
    );
    assert_eq!(items(children), [child.as_str()]);
    assert_eq!(items(run("list", &["--parent", &child], 0)), [grandchild]);
    let empty = run("list", &["--parent", &leaf], 0);
    assert_eq!(empty["data"]["truncated"], false);
    assert_eq!(empty["data"]["next_cursor"], Value::Null);
    assert!(items(empty).is_empty());

    for options in [
        &["--limit", "2", "--cursor", cursor][..],
        &["--limit", "1", "--parent", &root, "--cursor", cursor][..],
        &["--limit", "1", "--cursor", "broken-cursor"][..],
    ] {
        assert_eq!(run("list", options, 5)["error"]["code"], "STALE_CURSOR");
    }
    commit(&mut writer);
    assert_eq!(
        run("list", &["--limit", "1", "--cursor", cursor], 5)["error"]["code"],
        "STALE_CURSOR"
    );
    assert_eq!(run("show", &[&removed], 3)["error"]["code"], "NOT_FOUND");
    assert_eq!(
        run("list", &["--parent", &removed], 3)["error"]["code"],
        "NOT_FOUND"
    );
    let last = run("list", &["--limit", "2"], 0);
    assert_eq!(last["data"]["truncated"], false);
    assert_eq!(last["data"]["next_cursor"], Value::Null);
    assert_eq!(items(last), [leaf, root]);

    // Lowercase selectors must not match invalid uppercase records, even with a
    // case-insensitive table collation. Scoped errors must redact malformed IDs.
    let noncanonical = id(26);
    let uppercase = noncanonical.to_ascii_uppercase();
    let unsafe_id = "synthetic'unsafe/id";
    writer
        .exec_drop(
            "INSERT INTO areas (id, slug, name, revision) VALUES (?, 'upper', 'Upper', ?)",
            (&uppercase, &revision),
        )
        .unwrap();
    writer
        .exec_drop(
            "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES (?, ?, 'unsafe', 'Unsafe', ?)",
            (unsafe_id, &child, &revision),
        )
        .unwrap();
    commit(&mut writer);
    assert_eq!(
        run("show", &[&noncanonical], 3)["error"]["code"],
        "NOT_FOUND"
    );
    assert_eq!(
        run("list", &["--parent", &noncanonical], 3)["error"]["code"],
        "NOT_FOUND"
    );
    for selector in [uppercase.as_str(), unsafe_id] {
        let report = run("show", &[selector], 2);
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert!(!report.to_string().contains(selector));
    }
    let invalid = run("list", &["--parent", &child], 4);
    assert_eq!(invalid["error"]["code"], "INVALID_RECORD");
    assert!(!invalid.to_string().contains(unsafe_id));
}

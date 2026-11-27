use mysql::prelude::Queryable;
use serde_json::{Value, json};

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
    assert_eq!(run("tree", &[], 0)["data"]["roots"], json!([]));
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
    assert_eq!(
        items(run("list", &["--parent", &child], 0)),
        [grandchild.as_str()]
    );
    let empty = run("list", &["--parent", &leaf], 0);
    assert_eq!(empty["data"]["truncated"], false);
    assert_eq!(empty["data"]["next_cursor"], Value::Null);
    assert!(items(empty).is_empty());

    let (adjacent_child, sibling) = (id(17), id(18));
    writer
        .query_drop(format!(
            "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES
             ('{adjacent_child}', '{leaf}', 'adjacent-child', 'Adjacent child', '{revision}'),
             ('{sibling}', '{root}', 'a-sibling', 'Sibling', '{revision}')"
        ))
        .unwrap();

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
    assert_eq!(items(last), [leaf.as_str(), root.as_str()]);

    // Read from the committed hierarchy with a staged move and a dirty edit.
    writer
        .exec_drop(
            "UPDATE areas SET parent_id = ? WHERE id = ?",
            (&root, &adjacent_child),
        )
        .unwrap();
    writer.query_drop("CALL DOLT_ADD('areas')").unwrap();
    writer
        .exec_drop("UPDATE areas SET name = 'Dirty' WHERE id = ?", (&root,))
        .unwrap();
    let forest = run("tree", &[], 0);
    assert_eq!(run("tree", &[], 0), forest);
    assert_eq!(forest["context"]["scope"], json!({"kind": "roots"}));
    let roots = forest["data"]["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0]["id"], leaf);
    assert_eq!(roots[0]["children"][0]["id"], adjacent_child);
    assert_eq!(roots[1]["id"], root);
    assert_eq!(roots[1]["name"], "Uncommitted");
    assert!(roots[1].get("description").is_none());
    let children = roots[1]["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0]["id"], sibling);
    assert_eq!(children[1]["id"], child);
    assert_eq!(children[1]["archived_at"], "2025-01-01T00:00:00.000001Z");
    assert_eq!(children[1]["children"][0]["id"], grandchild);
    let subtree = run("tree", &["--root", &root], 0);
    assert_eq!(
        subtree["context"]["scope"],
        json!({"kind": "area", "id": root})
    );
    assert_eq!(subtree["data"]["roots"], json!([roots[1]]));
    assert!(!subtree.to_string().contains(&adjacent_child));
    assert_eq!(
        run("tree", &["--root", &child], 0)["data"]["roots"],
        json!([children[1]])
    );
    let leaf_tree = run("tree", &["--root", &sibling], 0);
    assert_eq!(leaf_tree["data"]["roots"], json!([children[0]]));
    assert_eq!(leaf_tree["data"]["roots"][0]["children"], json!([]));
    assert_eq!(
        run("tree", &["--root", &removed], 3)["error"]["code"],
        "NOT_FOUND"
    );

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
        run("tree", &["--root", &noncanonical], 3)["error"]["code"],
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
        let report = run("tree", &["--root", selector], 2);
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert!(!report.to_string().contains(selector));
    }
    let invalid = run("list", &["--parent", &child], 4);
    assert_eq!(invalid["error"]["code"], "INVALID_RECORD");
    assert!(!invalid.to_string().contains(unsafe_id));
    let invalid = run("tree", &["--root", &child], 4);
    assert_eq!(invalid["error"]["code"], "INVALID_RECORD");
    assert!(!invalid.to_string().contains(unsafe_id));

    if collation != CASE_INSENSITIVE {
        for index in 100..132 {
            let parent = (index > 100).then(|| id(index - 1));
            writer.exec_drop(
                "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES (?, ?, 'deep', 'Deep', ?)",
                (id(index), parent, &revision),
            ).unwrap();
        }
        commit(&mut writer);
        run("tree", &["--root", &id(100)], 0);
        writer.exec_drop(
            "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES (?, ?, 'deep', 'Deep', ?)",
            (id(132), id(131), &revision),
        ).unwrap();
        commit(&mut writer);
        assert_eq!(
            run("tree", &["--root", &id(100)], 2)["error"]["code"],
            "CONTEXT_LIMIT"
        );

        writer
            .exec_drop(
                "UPDATE areas SET parent_id = ? WHERE id = ?",
                (&adjacent_child, &root),
            )
            .unwrap();
        commit(&mut writer);
        let cycle = run("tree", &["--root", &root], 4);
        assert_eq!(cycle["error"]["code"], "INVALID_RECORD");
        assert!(
            cycle["error"]["message"]
                .as_str()
                .unwrap()
                .contains("cycle")
        );

        for index in 200..300 {
            let parent = match index {
                200 => None,
                201 | 202 => Some(id(200)),
                _ => Some(id(201 + index % 2)),
            };
            writer.exec_drop(
                "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES (?, ?, ?, 'Wide', ?)",
                (id(index), parent, format!("wide-{index}"), &revision),
            ).unwrap();
        }
        commit(&mut writer);
        run("tree", &["--root", &id(200)], 0);
        writer.exec_drop(
            "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES (?, ?, 'wide-300', 'Wide', ?)",
            (id(300), id(201), &revision),
        ).unwrap();
        commit(&mut writer);
        assert_eq!(
            run("tree", &["--root", &id(200)], 2)["error"]["code"],
            "CONTEXT_LIMIT"
        );
    }
}

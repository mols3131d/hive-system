use mysql::prelude::Queryable;
use serde_json::{Value, json};

use super::{ADMIN, DATABASE, Fixture, commit, id, storage_state};

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn reads_committed_projects_and_bounded_cross_area_membership() {
    let fixture = Fixture::start();
    let mut writer = fixture.connect(ADMIN);
    let workspace = id(1);
    let port = fixture.port.to_string();
    let run = |action: &str, options: &[&str], exit| {
        let before = storage_state(&mut fixture.connect(ADMIN));
        let mut args = vec!["project", action];
        args.extend(options);
        args.extend([
            "--database",
            DATABASE,
            "--port",
            &port,
            "--workspace-id",
            &workspace,
            "--contract",
            "2",
            "--json",
        ]);
        let output = fixture.run_hive(&args, &fixture.password);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert!(output.stderr.is_empty());
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["ok"], exit == 0);
        assert_eq!(report["command"], format!("project {action}"));
        assert_eq!(report["context"]["workspace_id"], workspace);
        assert_eq!(report["context"]["db_commit"], before.0);
        assert_eq!(storage_state(&mut fixture.connect(ADMIN)), before);
        report
    };

    let empty = run("list", &[], 0);
    assert_eq!(empty["data"]["items"], json!([]));
    assert_eq!(empty["data"]["truncated"], false);
    assert_eq!(empty["data"]["next_cursor"], Value::Null);

    let (first_id, second_id, completed, archived) = (id(20), id(21), id(22), id(23));
    let revision = id(4);
    writer
        .query_drop(format!(
            "INSERT INTO projects (id, name, description, status, revision) VALUES
         ('{second_id}', 'Alpha', 'adjacent description', 'active', '{revision}'),
         ('{first_id}', 'Zulu', 'selected description', 'active', '{revision}'),
         ('{completed}', 'Completed', NULL, 'completed', '{revision}'),
         ('{archived}', 'Archived', NULL, 'archived', '{revision}')"
        ))
        .unwrap();

    // Issues #2 and #3 belong to different Areas and have different lifecycles.
    // Archive the second Area after releasing its fixture claim.
    let (child, prerequisite, dependent) = (id(11), id(12), id(13));
    writer
        .query_drop(format!(
            "DELETE FROM issue_claims WHERE issue_id = '{child}'"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "UPDATE issues SET status = 'todo' WHERE id = '{child}'"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "UPDATE areas SET archived_at = UTC_TIMESTAMP(6) WHERE id = '{}'",
            id(3)
        ))
        .unwrap();
    writer.query_drop(format!(
        "INSERT INTO project_issues VALUES
         ('{archived}', '{child}'), ('{archived}', '{prerequisite}'), ('{completed}', '{dependent}')"
    )).unwrap();
    writer
        .query_drop("UPDATE issues SET body = 'synthetic Issue body'")
        .unwrap();
    for n in 100..124 {
        let member = id(n);
        writer.query_drop(format!(
            "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
             VALUES ('{member}', {n}, '{}', 'Synthetic', 'task', 'backlog', '{revision}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            id(2)
        )).unwrap();
        writer
            .query_drop(format!(
                "INSERT INTO project_issues VALUES ('{archived}', '{member}')"
            ))
            .unwrap();
    }
    writer
        .query_drop("UPDATE workspace_counters SET next_value = 124")
        .unwrap();
    commit(&mut writer);

    let first = run("list", &["--status", "active", "--limit", "1"], 0);
    assert_eq!(first["context"]["scope"], json!({"kind": "all"}));
    assert_eq!(
        first["data"]["observed_revision"],
        first["context"]["db_commit"]
    );
    assert_eq!(
        first["data"]["items"],
        json!([
            {"id": first_id, "name": "Zulu", "status": "active", "revision": revision}
        ])
    );
    assert_eq!(first["data"]["truncated"], true);
    assert_eq!(
        first,
        run("list", &["--status", "active", "--limit", "1"], 0)
    );
    let cursor = first["data"]["next_cursor"].as_str().unwrap();

    writer
        .query_drop(format!(
            "UPDATE projects SET name = 'staged name' WHERE id = '{second_id}'"
        ))
        .unwrap();
    writer.query_drop("CALL DOLT_ADD('projects')").unwrap();
    writer
        .query_drop(format!(
            "UPDATE projects SET name = 'dirty name' WHERE id = '{second_id}'"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "DELETE FROM project_issues WHERE project_id = '{archived}' AND issue_id = '{child}'"
        ))
        .unwrap();
    let second = run(
        "list",
        &["--status", "active", "--limit", "1", "--cursor", cursor],
        0,
    );
    assert_eq!(
        second["data"]["items"],
        json!([
            {"id": second_id, "name": "Alpha", "status": "active", "revision": revision}
        ])
    );
    assert_eq!(second["data"]["truncated"], false);
    assert_eq!(second["data"]["next_cursor"], Value::Null);

    for (status, expected) in [("completed", &completed), ("archived", &archived)] {
        let filtered = run("list", &["--status", status], 0);
        assert_eq!(filtered["data"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["data"]["items"][0]["id"], *expected);
    }
    assert_eq!(
        run("list", &["--limit", "2", "--cursor", cursor], 5)["error"]["code"],
        "STALE_CURSOR"
    );
    assert_eq!(
        run(
            "list",
            &["--status", "active", "--limit", "2", "--cursor", cursor],
            5
        )["error"]["code"],
        "STALE_CURSOR"
    );
    assert_eq!(
        run(
            "list",
            &["--status", "archived", "--limit", "1", "--cursor", cursor],
            5
        )["error"]["code"],
        "STALE_CURSOR"
    );

    let summary = run("show", &[&first_id], 0);
    assert_eq!(
        summary["data"]["project"]["description"],
        "selected description"
    );
    assert_eq!(
        summary["data"]["issues"],
        json!({"items": [], "truncated": false})
    );
    let detail = run("show", &[&archived], 0);
    assert_eq!(
        detail["context"]["scope"],
        json!({"kind": "project", "id": archived})
    );
    let members: Vec<_> = [child, prerequisite]
        .into_iter()
        .chain((100..123).map(id))
        .collect();
    assert_eq!(
        detail["data"],
        json!({
            "project": {"id": archived, "name": "Archived", "description": null, "status": "archived", "revision": revision},
            "issues": {"items": members, "truncated": true},
        })
    );

    let case_mismatch = id(0x2a);
    writer
        .query_drop(format!(
            "INSERT INTO projects (id, name, status, revision) VALUES
         ('{}', 'Malformed identity', 'completed', '{revision}'),
         ('{}', 'Malformed status', 'Active', '{revision}')",
            case_mismatch.to_ascii_uppercase(),
            id(0x2b)
        ))
        .unwrap();
    commit(&mut writer);
    assert_eq!(
        run("show", &[&case_mismatch], 3)["error"]["code"],
        "NOT_FOUND"
    );
    assert_eq!(
        run("show", &[&id(0x2b)], 4)["error"]["code"],
        "INVALID_RECORD"
    );
    assert_eq!(
        run("list", &["--status", "completed", "--limit", "1"], 4)["error"]["code"],
        "INVALID_RECORD"
    );
    assert_eq!(
        run("list", &["--status", "active"], 0)["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        run(
            "list",
            &["--status", "active", "--limit", "1", "--cursor", cursor],
            5
        )["error"]["code"],
        "STALE_CURSOR"
    );
}

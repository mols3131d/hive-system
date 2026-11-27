use mysql::prelude::Queryable;
use serde_json::{Value, json};

use super::{ADMIN, DATABASE, Fixture, commit, id, storage_state};

fn run_view(fixture: &Fixture, view: &str, options: &[&str], exit: i32) -> Value {
    let before = storage_state(&mut fixture.connect(ADMIN));
    let workspace = id(1);
    let port = fixture.port.to_string();
    let mut args = vec![
        "--contract",
        "2",
        "--json",
        "issue",
        view,
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
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{view} {options:?}: {report}"
    );
    assert!(output.stderr.is_empty(), "{report}");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], format!("issue {view}"));
    assert_eq!(report["context"]["db_commit"], before.0);
    if exit == 0 {
        assert_eq!(report["data"]["observed_revision"], before.0);
    }
    assert!(!report.to_string().contains("private fixture"));
    assert_eq!(storage_state(&mut fixture.connect(ADMIN)), before);
    report
}

fn numbers(report: &Value) -> Vec<u64> {
    report["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["number"].as_u64().unwrap())
        .collect::<Vec<_>>()
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn issue_readiness_uses_exact_committed_scopes_and_filters_before_paging() {
    let fixture = Fixture::start();
    let mut writer = fixture.connect(ADMIN);
    let (root, child, revision) = (id(2), id(3), id(4));
    let (archived, inherited, cycle_a, cycle_b, orphan) = (id(20), id(21), id(22), id(23), id(24));
    let project = id(25);
    let label = id(26);
    let run = |view, options: &[&str], exit| run_view(&fixture, view, options, exit);

    writer
        .query_drop(format!(
            "INSERT INTO areas (id, parent_id, slug, name, revision, archived_at) VALUES
         ('{archived}', NULL, 'archived', 'Archived', '{revision}', UTC_TIMESTAMP(6)),
         ('{inherited}', '{archived}', 'inherited', 'Inherited', '{revision}', NULL),
         ('{cycle_a}', NULL, 'cycle-a', 'Cycle A', '{revision}', NULL),
         ('{cycle_b}', '{cycle_a}', 'cycle-b', 'Cycle B', '{revision}', NULL),
         ('{orphan}', NULL, 'orphan', 'Orphan', '{revision}', NULL)"
        ))
        .unwrap();
    writer
        .exec_drop(
            "UPDATE areas SET parent_id = ? WHERE id = ?",
            (&cycle_b, &cycle_a),
        )
        .unwrap();

    // Issue #4 in the base fixture depends on accepted #3 across Areas.
    // Give the ready controls the same priority and time so the ID breaks the tie.
    writer
        .exec_drop(
            "UPDATE issues SET priority = 1, created_at = '2025-01-01' WHERE number = 4",
            (),
        )
        .unwrap();
    for (number, area, kind, hold) in [
        (5, &root, "task", Some("private fixture hold")),
        (6, &child, "bug", None),
        (7, &root, "feature", None),
        (8, &child, "task", None),
        (9, &inherited, "task", None),
        (10, &inherited, "task", Some("private fixture hold")),
        (11, &child, "task", None),
        (12, &orphan, "task", None),
        (13, &cycle_a, "task", None),
        (14, &child, "task", None),
        (15, &root, "task", None),
        (16, &root, "task", None),
        (17, &root, "task", None),
    ] {
        writer.exec_drop(
            "INSERT INTO issues (id, number, area_id, title, body, type, status, priority, hold_reason,
                                 revision, created_at, updated_at)
             VALUES (?, ?, ?, 'Synthetic Issue', 'private fixture body', ?, 'todo', ?, ?, ?,
                     '2025-01-01', '2025-01-01')",
            (id(number + 30), number, area, kind, u8::from(number != 5), hold, &revision),
        ).unwrap();
    }
    for sql in [
        "UPDATE issues SET status = 'cancelled', closed_at = UTC_TIMESTAMP(6),
         closed_by_ref = 'fixture', resolution_note = 'private fixture cancelled' WHERE number = 15".to_owned(),
        "UPDATE issues SET status = 'done', closed_at = UTC_TIMESTAMP(6),
         closed_by_ref = '', resolution_note = 'private fixture invalid closure' WHERE number = 16".to_owned(),
        format!("INSERT INTO issue_claims VALUES
                 ('{}', '{}', 'private fixture holder', UTC_TIMESTAMP(6)),
                 ('{}', '{}', 'private fixture holder', UTC_TIMESTAMP(6))",
                 id(37), id(50), id(40), id(51)),
        format!("INSERT INTO projects (id, name, status, revision)
                 VALUES ('{project}', 'Selected', 'active', '{revision}')"),
        format!("INSERT INTO project_issues VALUES ('{project}', '{}'), ('{project}', '{}')", id(13), id(39)),
        format!("INSERT INTO labels (id, name, revision) VALUES ('{label}', 'selected', '{revision}')"),
        format!("INSERT INTO issue_labels VALUES ('{}', '{label}')", id(36)),
        "UPDATE workspace_counters SET next_value = 18 WHERE name = 'issue_number'".to_owned(),
        "SET SESSION foreign_key_checks=0".to_owned(),
        "SET SESSION dolt_force_transaction_commit=1".to_owned(),
        format!("INSERT INTO issue_dependencies VALUES
                 ('{}', '{}'), ('{}', '{}'), ('{}', '{}'), ('{}', '{}')",
                 id(38), id(45), id(40), id(45), id(41), id(99), id(44), id(46)),
        format!("UPDATE areas SET parent_id = '{}' WHERE id = '{orphan}'", id(98)),
    ] {
        writer.query_drop(&sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    writer
        .exec_drop(
            "UPDATE issues SET revision = ? WHERE number = 17",
            ("\t\u{2003}",),
        )
        .unwrap();
    commit(&mut writer);

    let first = run("ready", &["--all", "--limit", "1"], 0);
    assert_eq!(numbers(&first), [4]);
    assert!(first["data"]["items"][0].get("reasons").is_none());
    assert_eq!(first["data"]["truncated"], true);
    let cursor = first["data"]["next_cursor"].as_str().unwrap();

    let blocked = run("blocked", &["--all"], 0);
    assert_eq!(numbers(&blocked), [5, 7, 8, 9, 10, 11, 12, 13, 14, 17]);
    let first_blocked = run("blocked", &["--all", "--limit", "1"], 0);
    let blocked_cursor = first_blocked["data"]["next_cursor"].as_str().unwrap();
    assert_eq!(numbers(&first_blocked), [5]);
    assert_eq!(
        numbers(&run(
            "blocked",
            &["--all", "--limit", "1", "--cursor", blocked_cursor],
            0
        )),
        [7]
    );

    let reasons: Vec<_> = blocked["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| &item["reasons"])
        .collect();
    assert_eq!(
        reasons,
        [
            &json!(["HOLD"]),
            &json!(["CURRENT_CLAIM"]),
            &json!(["PREREQUISITE_NOT_DONE"]),
            &json!(["ARCHIVED_AREA"]),
            &json!([
                "HOLD",
                "CURRENT_CLAIM",
                "PREREQUISITE_NOT_DONE",
                "ARCHIVED_AREA"
            ]),
            &json!(["INVALID_PREREQUISITE"]),
            &json!(["INVALID_AREA_ANCESTRY"]),
            &json!(["INVALID_AREA_ANCESTRY"]),
            &json!(["INVALID_PREREQUISITE"]),
            &json!(["INVALID_ISSUE_METADATA"]),
        ]
    );
    assert_eq!(numbers(&run("ready", &["--area", &child], 0)), [4, 6]);
    assert!(numbers(&run("ready", &["--area", &root], 0)).is_empty());
    assert_eq!(numbers(&run("ready", &["--project", &project], 0)), [4]);
    assert_eq!(numbers(&run("blocked", &["--project", &project], 0)), [9]);
    assert_eq!(
        numbers(&run(
            "ready",
            &[
                "--all",
                "--status",
                "todo",
                "--type",
                "bug",
                "--priority",
                "1",
                "--label",
                "selected"
            ],
            0
        )),
        [6]
    );
    assert!(numbers(&run("blocked", &["--all", "--status", "done"], 0)).is_empty());

    for (view, flag, value, expected) in [
        ("ready", "--type", "task", &[4][..]),
        ("ready", "--label", "selected", &[6]),
        ("blocked", "--priority", "0", &[5]),
        ("ready", "--status", "done", &[]),
    ] {
        assert_eq!(numbers(&run(view, &["--all", flag, value], 0)), expected);
    }

    for view in ["list", "blocked"] {
        assert_eq!(
            run(view, &["--all", "--limit", "1", "--cursor", cursor], 5)["error"]["code"],
            "STALE_CURSOR"
        );
    }

    // Change every readiness input in the staged or working roots.
    writer
        .query_drop("UPDATE issues SET status = 'cancelled' WHERE number = 3")
        .unwrap();
    writer
        .exec_drop(
            "UPDATE areas SET archived_at = UTC_TIMESTAMP(6) WHERE id = ?",
            (&child,),
        )
        .unwrap();
    writer
        .query_drop("CALL DOLT_ADD('issues', 'areas')")
        .unwrap();
    writer
        .exec_drop(
            "INSERT INTO issue_claims VALUES (?, ?, 'private fixture holder', UTC_TIMESTAMP(6))",
            (id(36), id(52)),
        )
        .unwrap();
    writer
        .exec_drop(
            "INSERT INTO issue_dependencies VALUES (?, ?)",
            (id(36), id(45)),
        )
        .unwrap();
    writer
        .query_drop(
            "UPDATE issues SET hold_reason = 'private fixture working hold' WHERE number = 6",
        )
        .unwrap();
    assert_eq!(run("ready", &["--all", "--limit", "1"], 0), first);
    assert_eq!(run("blocked", &["--all"], 0), blocked);
    let second = run("ready", &["--all", "--limit", "1", "--cursor", cursor], 0);
    assert_eq!(numbers(&second), [6]);
    assert_eq!(second["data"]["next_cursor"], Value::Null);
    commit(&mut writer);
    assert_eq!(
        run("ready", &["--all", "--limit", "1", "--cursor", cursor], 5)["error"]["code"],
        "STALE_CURSOR"
    );
    assert!(numbers(&run("ready", &["--all"], 0)).is_empty());

    // Restore the controls so each following commit exercises one readiness input.
    writer
        .query_drop("UPDATE issues SET status = 'done' WHERE number = 3")
        .unwrap();
    writer
        .exec_drop(
            "UPDATE areas SET archived_at = NULL WHERE id = ?",
            (&child,),
        )
        .unwrap();
    writer
        .exec_drop("DELETE FROM issue_claims WHERE issue_id = ?", (id(36),))
        .unwrap();
    writer
        .exec_drop(
            "DELETE FROM issue_dependencies WHERE issue_id = ?",
            (id(36),),
        )
        .unwrap();
    writer
        .query_drop("UPDATE issues SET hold_reason = NULL WHERE number = 6")
        .unwrap();
    commit(&mut writer);

    for (column, blank, valid) in [
        ("closed_by_ref", "\t", "fixture-reviewer"),
        ("resolution_note", "\n", "Accepted"),
        ("revision", "\u{00A0}\t", revision.as_str()),
    ] {
        let sql = format!("UPDATE issues SET {column} = ? WHERE number = 3");
        assert_eq!(numbers(&run("ready", &["--project", &project], 0)), [4]);
        writer.exec_drop(&sql, (blank,)).unwrap();
        commit(&mut writer);
        assert!(numbers(&run("ready", &["--project", &project], 0)).is_empty());
        let blocked = run("blocked", &["--project", &project, "--type", "task"], 0);
        assert_eq!(numbers(&blocked), [4, 9]);
        assert_eq!(
            blocked["data"]["items"][0]["reasons"],
            json!(["INVALID_PREREQUISITE"])
        );
        writer.exec_drop(&sql, (valid,)).unwrap();
        commit(&mut writer);
    }

    let accepted = id(12);
    let candidate = id(36);
    let cancelled = id(45);
    for (scope, update, restore, number, reason) in [
        (
            &["--project", project.as_str(), "--priority", "1"][..],
            format!(
                "INSERT INTO issue_claims VALUES ('{accepted}', '{}', 'private fixture holder', UTC_TIMESTAMP(6))",
                id(53)
            ),
            format!("DELETE FROM issue_claims WHERE issue_id = '{accepted}'"),
            4,
            "INVALID_PREREQUISITE",
        ),
        (
            &["--all", "--label", "selected", "--type", "bug"],
            "UPDATE issues SET hold_reason = 'private fixture hold' WHERE number = 6".to_owned(),
            "UPDATE issues SET hold_reason = NULL WHERE number = 6".to_owned(),
            6,
            "HOLD",
        ),
        (
            &["--all", "--label", "selected", "--type", "bug"],
            format!(
                "INSERT INTO issue_claims VALUES ('{candidate}', '{}', 'private fixture holder', UTC_TIMESTAMP(6))",
                id(52)
            ),
            format!("DELETE FROM issue_claims WHERE issue_id = '{candidate}'"),
            6,
            "CURRENT_CLAIM",
        ),
        (
            &["--all", "--label", "selected", "--type", "bug"],
            format!("INSERT INTO issue_dependencies VALUES ('{candidate}', '{cancelled}')"),
            format!("DELETE FROM issue_dependencies WHERE issue_id = '{candidate}'"),
            6,
            "PREREQUISITE_NOT_DONE",
        ),
    ] {
        assert_eq!(numbers(&run("ready", scope, 0)), [number]);
        writer.query_drop(update).unwrap();
        commit(&mut writer);
        assert!(numbers(&run("ready", scope, 0)).is_empty());
        let report = run("blocked", scope, 0);
        assert_eq!(report["data"]["items"][0]["number"], number);
        assert_eq!(report["data"]["items"][0]["reasons"], json!([reason]));
        writer.query_drop(restore).unwrap();
        commit(&mut writer);
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn issue_readiness_crosses_candidate_batches_with_one_prerequisite_budget() {
    let fixture = Fixture::start();
    let mut writer = fixture.connect(ADMIN);
    let (root, revision, area) = (id(2), id(4), id(20));
    writer
        .exec_drop(
            "INSERT INTO areas (id, slug, name, revision) VALUES (?, 'batch', 'Batch', ?)",
            (&area, &revision),
        )
        .unwrap();
    writer
        .exec_batch(
            "INSERT INTO issues (id, number, area_id, title, type, status, priority, hold_reason,
                             revision, created_at, updated_at)
         VALUES (?, ?, ?, 'Candidate', 'task', 'todo', 1, ?, ?, '2025-01-01', '2025-01-01')",
            (1000..=1101).map(|number| {
                let hold = (!matches!(number, 1000 | 1100)).then_some("held");
                (id(number), number, &area, hold, &revision)
            }),
        )
        .unwrap();
    commit(&mut writer);

    let first = run_view(&fixture, "ready", &["--area", &area, "--limit", "1"], 0);
    assert_eq!(numbers(&first), [1000]);
    assert_eq!(first["data"]["truncated"], true);
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    let second = run_view(
        &fixture,
        "ready",
        &["--area", &area, "--limit", "1", "--cursor", cursor],
        0,
    );
    assert_eq!(numbers(&second), [1100]);
    assert_eq!(second["data"]["next_cursor"], Value::Null);

    // 99 held candidates in the first batch and one in the second each have
    // 1,001 accepted prerequisites. Each batch fits independently.
    writer.query_drop("START TRANSACTION").unwrap();
    writer
        .exec_batch(
            "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at,
                             updated_at, closed_at, closed_by_ref, resolution_note)
         VALUES (?, ?, ?, 'Prerequisite', 'task', 'done', ?, '2025-01-01',
                 '2025-01-01', '2025-01-01', 'fixture-reviewer', 'Accepted')",
            (2000..=3000).map(|number| (id(number), number, &root, &revision)),
        )
        .unwrap();
    // Bound each seed statement to five candidates under concurrent test load.
    writer
        .exec_batch(
            "INSERT INTO issue_dependencies (issue_id, prerequisite_id)
             SELECT candidate.id, prerequisite.id FROM issues AS candidate CROSS JOIN issues AS prerequisite
             WHERE candidate.area_id = ? AND candidate.hold_reason IS NOT NULL
               AND candidate.number BETWEEN ? AND ?
               AND prerequisite.number BETWEEN 2000 AND 3000",
            (1000..=1101).step_by(5).map(|first| (&area, first, first + 4)),
        )
        .unwrap();
    writer
        .query_drop("UPDATE workspace_counters SET next_value = 3001 WHERE name = 'issue_number'")
        .unwrap();
    writer.query_drop("COMMIT").unwrap();
    commit(&mut writer);

    let capped = run_view(&fixture, "ready", &["--area", &area, "--limit", "1"], 2);
    assert_eq!(capped["error"]["code"], "CONTEXT_LIMIT");
    assert_eq!(
        capped["error"]["message"],
        "The readiness query exceeds the prerequisite read limit."
    );
    assert!(capped.get("data").is_none());
}

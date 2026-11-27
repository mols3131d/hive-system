use mysql::Conn;
use mysql::prelude::Queryable;

use super::{ADMIN, Fixture, assert_diagnostics, commit, id, storage_state};

const ARTIFACT: u32 = 0x5a;
const PROJECT: u32 = 0x4a;
const RESOURCE: u32 = 0x4b;
const LINKS: [(&str, u32); 4] = [
    ("artifact_areas", 2),
    ("artifact_projects", PROJECT),
    ("artifact_issues", 12),
    ("artifact_resources", RESOURCE),
];

fn seed_targets(admin: &mut Conn) {
    admin
        .exec_drop(
            "INSERT INTO projects (id, name, status, revision) VALUES (?, 'Fixture', 'active', 'fixture-revision')",
            (id(PROJECT),),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO resources (id, kind, name, revision) VALUES (?, 'fixture-kind', 'Fixture', 'fixture-revision')",
            (id(RESOURCE),),
        )
        .unwrap();
}

fn insert_artifact(admin: &mut Conn, id: &str, kind: &str) {
    admin
        .exec_drop(
            "INSERT INTO artifacts (id, kind, title, body, author_ref, revision, created_at, substantive_updated_at)
             VALUES (?, ?, 'Fixture', 'Synthetic Artifact body', 'fixture-author', 'fixture-revision',
                     UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (id, kind),
        )
        .unwrap();
}

fn insert_link(admin: &mut Conn, table: &str, artifact: &str, target: &str) {
    admin
        .exec_drop(
            format!("INSERT INTO {table} VALUES (?, ?)"),
            (artifact, target),
        )
        .unwrap();
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn accepts_standalone_legacy_and_multi_scope_artifacts() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    seed_targets(&mut admin);
    for (number, kind) in (0x70..).zip([
        "research",
        "design",
        "decision",
        "report",
        "review",
        "handoff",
        "evidence",
        "work-note",
        "message",
        "checkpoint",
        "manifest",
    ]) {
        let artifact = id(number);
        insert_artifact(&mut admin, &artifact, kind);
        if kind == "manifest" {
            admin
                .exec_drop(
                    "UPDATE artifacts SET archived_at=UTC_TIMESTAMP(6), archive_reason='Fixture archive' WHERE id=?",
                    (&artifact,),
                )
                .unwrap();
        }
    }
    let artifact = id(ARTIFACT);
    insert_artifact(&mut admin, &artifact, "evidence");
    for (table, target) in LINKS
        .into_iter()
        .chain([("artifact_areas", 3), ("artifact_issues", 13)])
    {
        insert_link(&mut admin, table, &artifact, &id(target));
    }
    commit(&mut admin);
    fixture.check(0, &mut admin);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rejects_noncanonical_artifacts_and_link_ids() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    seed_targets(&mut admin);
    let artifact = id(ARTIFACT);
    let artifact_alias = artifact.to_ascii_uppercase();
    let project_alias = id(PROJECT).to_ascii_uppercase();
    let invalid_id = "fixture-invalid-artifact-id";
    let area = id(2);
    insert_artifact(&mut admin, &artifact, "REPORT");
    insert_artifact(&mut admin, invalid_id, "message");
    // The SQL foreign keys accept these aliases under the fixture's collation.
    insert_link(&mut admin, "artifact_areas", &artifact_alias, &area);
    insert_link(&mut admin, "artifact_projects", &artifact, &project_alias);
    commit(&mut admin);

    let report = fixture.check(4, &mut admin);
    assert_diagnostics(
        &report,
        &[
            ("INVALID_ARTIFACT_KIND", "artifacts", &[&artifact]),
            ("INVALID_ID", "artifacts", &[]),
            ("INVALID_ID", "artifact_areas", &[&area]),
            ("INVALID_ID", "artifact_projects", &[&artifact]),
        ],
    );
    let report_text = report.to_string();
    for value in [
        invalid_id,
        &artifact_alias,
        &project_alias,
        "Synthetic Artifact body",
    ] {
        assert!(!report_text.contains(value));
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn checks_missing_artifact_link_endpoints_at_the_committed_snapshot() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    seed_targets(&mut admin);
    let artifact = id(ARTIFACT);
    insert_artifact(&mut admin, &artifact, "report");
    commit(&mut admin);
    let baseline = fixture.check(0, &mut admin);
    let before = storage_state(&mut admin);

    let (missing_artifact, missing_target) = (id(0x90), id(0x91));
    let targets = LINKS.map(|(table, target)| (table, id(target)));
    let rows: Vec<_> = targets
        .iter()
        .flat_map(|(table, target)| {
            [
                (*table, [artifact.as_str(), missing_target.as_str()]),
                (*table, [missing_artifact.as_str(), target.as_str()]),
            ]
        })
        .collect();
    admin
        .query_drop("SET SESSION foreign_key_checks=0")
        .unwrap();
    for (table, [left, right]) in &rows {
        insert_link(&mut admin, table, left, right);
    }
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
    let expected: Vec<_> = rows
        .iter()
        .map(|(table, ids)| ("MISSING_REFERENCE", *table, ids.as_slice()))
        .collect();
    assert_diagnostics(&report, &expected);
    assert_eq!(fixture.check(4, &mut admin), report);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn reads_artifact_metadata_and_gates_committed_bodies_by_exact_scope() {
    let fixture = Fixture::start();
    let mut writer = fixture.connect(ADMIN);
    seed_targets(&mut writer);
    let (cross, standalone, direct, neighboring, large, malformed) =
        (id(0x5a), id(0x5b), id(0x5c), id(0x5d), id(0x5e), id(0x5f));
    let accented = id(0x6a);
    for artifact in [
        &cross,
        &standalone,
        &direct,
        &neighboring,
        &large,
        &malformed,
        &accented,
    ] {
        insert_artifact(&mut writer, artifact, "report");
    }
    let private_marker = "artifact-body-private-marker";
    let fixture_body =
        format!("Committed café\n{private_marker}\n[reference](file:///unread-fixture)");
    writer.exec_drop(
        "UPDATE artifacts SET title='Cross-linked', body=?, source_ref='hidden-source' WHERE id=?",
        (&fixture_body, &cross),
    ).unwrap();
    writer
        .exec_drop(
            "UPDATE artifacts SET title='Hidden neighboring title' WHERE id=?",
            (&neighboring,),
        )
        .unwrap();
    writer
        .exec_drop(
            "UPDATE artifacts SET body=REPEAT('é', 32769) WHERE id=?",
            (&large,),
        )
        .unwrap();
    let scopes: Vec<_> = LINKS
        .iter()
        .zip(["area", "project", "issue", "resource"])
        .map(|((table, endpoint), kind)| {
            insert_link(&mut writer, table, &cross, &id(*endpoint));
            format!("{kind}:{}", id(*endpoint))
        })
        .collect();
    insert_link(&mut writer, "artifact_areas", &cross, &id(3));
    insert_link(&mut writer, "artifact_areas", &direct, &id(2));
    insert_link(&mut writer, "artifact_areas", &neighboring, &id(3));
    insert_link(
        &mut writer,
        "artifact_areas",
        &malformed.to_ascii_uppercase(),
        &id(2),
    );
    let accented_alias = format!("{}á", accented.strip_suffix('a').unwrap());
    insert_link(&mut writer, "artifact_areas", &accented_alias, &id(2));
    commit(&mut writer);

    writer
        .exec_drop(
            "DELETE FROM artifact_resources WHERE artifact_id=?",
            (&cross,),
        )
        .unwrap();
    writer
        .query_drop("CALL DOLT_ADD('artifact_resources')")
        .unwrap();
    writer
        .exec_drop(
            "UPDATE artifacts SET body='Dirty body', title='Dirty title' WHERE id=?",
            (&cross,),
        )
        .unwrap();
    let working_resource = id(0xff);
    writer
        .exec_drop(
            "INSERT INTO resources (id, kind, name, revision) VALUES (?, 'fixture', 'Working only', 'fixture-revision')",
            (&working_resource,),
        )
        .unwrap();
    let before = storage_state(&mut writer);

    let port = fixture.port.to_string();
    let workspace = id(1);
    let run = |leaf: &str, options: &[&str], expected_exit| {
        let args = [
            &[
                "--contract",
                "2",
                "--json",
                "artifact",
                leaf,
                "--database",
                super::DATABASE,
                "--workspace-id",
                &workspace,
                "--port",
                &port,
            ][..],
            options,
        ]
        .concat();
        let output = fixture.run_hive(&args, &fixture.password);
        let report: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {:?}", output));
        assert_eq!(output.status.code(), Some(expected_exit), "{report}");
        assert!(output.stderr.is_empty(), "{report}");
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], format!("artifact {leaf}"));
        assert_eq!(report["context"]["db_commit"], before.0);
        report
    };

    let page = run("list", &["--scope", &scopes[0], "--limit", "1"], 0);
    assert_eq!(
        page["data"]["items"],
        serde_json::json!([{
            "id": cross, "kind": "report", "title": "Cross-linked", "revision": "fixture-revision"
        }])
    );
    for hidden in [
        "Hidden neighboring title",
        private_marker,
        "hidden-source",
        &id(3),
        &id(RESOURCE),
    ] {
        assert!(!page.to_string().contains(hidden));
    }
    assert_eq!(page["data"]["truncated"], true);
    let cursor = page["data"]["next_cursor"].as_str().unwrap();
    let second = run(
        "list",
        &["--scope", &scopes[0], "--limit", "1", "--cursor", cursor],
        0,
    );
    assert_eq!(second["data"]["items"][0]["id"], direct);
    assert_eq!(second["data"]["next_cursor"], serde_json::Value::Null);
    for scope in &scopes[1..] {
        let page = run("list", &["--scope", scope], 0);
        assert_eq!(page["data"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["data"]["items"][0]["id"], cross);
    }
    assert_eq!(
        run(
            "list",
            &["--scope", &scopes[1], "--limit", "1", "--cursor", cursor],
            5
        )["error"]["code"],
        "STALE_CURSOR"
    );
    let unknown = format!("resource:{working_resource}");
    assert_eq!(
        run("list", &["--scope", &unknown], 3)["error"]["code"],
        "NOT_FOUND"
    );
    assert_eq!(
        run("show", &[&standalone, "--scope", &unknown], 3)["error"]["code"],
        "NOT_FOUND"
    );

    let area_three = format!("area:{}", id(3));
    let mut selection = vec![cross.as_str(), "--scope", area_three.as_str()];
    for scope in &scopes[..3] {
        selection.extend(["--scope", scope]);
    }
    let denial = run("show", &selection, 6);
    assert_eq!(denial["error"]["code"], "UNAUTHORIZED");
    assert!(denial.get("data").is_none_or(serde_json::Value::is_null));
    for hidden in [
        &id(RESOURCE),
        "Cross-linked",
        private_marker,
        "hidden-source",
    ] {
        assert!(!denial.to_string().contains(hidden));
    }
    assert_eq!(run("show", &[&cross], 6)["error"]["code"], "UNAUTHORIZED");
    selection.extend(["--scope", &scopes[3]]);
    let shown = run("show", &selection, 0);
    assert_eq!(
        shown["data"]["artifact"],
        serde_json::json!({
            "id": cross, "title": "Cross-linked", "kind": "report", "revision": "fixture-revision",
            "body": fixture_body, "body_bytes": fixture_body.len(),
        })
    );
    assert_eq!(
        run("show", &[&standalone], 0)["data"]["artifact"]["body"],
        "Synthetic Artifact body"
    );
    assert_eq!(run("show", &[&large], 2)["error"]["code"], "CONTEXT_LIMIT");
    assert_eq!(
        run("show", &[&malformed, "--scope", &scopes[0]], 6)["error"]["code"],
        "UNAUTHORIZED"
    );
    assert_eq!(
        run("show", &[&accented], 6)["error"]["code"],
        "UNAUTHORIZED"
    );
    assert_eq!(storage_state(&mut writer), before);
}

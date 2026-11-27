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

use mysql::Conn;
use mysql::prelude::Queryable;

use super::{ADMIN, Fixture, assert_diagnostics, commit, id, storage_state};

const PROJECT: u32 = 0x3a;
const RESOURCE: u32 = 0x3b;
const LABEL: u32 = 0x3c;

fn seed_targets(admin: &mut Conn) {
    // Revisions are opaque, and a Resource URI is optional.
    let revision = "fixture-revision";
    admin
        .exec_drop(
            "INSERT INTO projects (id, name, status, revision) VALUES (?, 'Fixture', 'active', ?)",
            (id(PROJECT), revision),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO resources (id, kind, name, revision) VALUES (?, 'fixture-kind', 'Shared', ?)",
            (id(RESOURCE), revision),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO labels (id, name, revision) VALUES (?, 'Fíxture QA', ?)",
            (id(LABEL), revision),
        )
        .unwrap();
}

fn assert_sql_error(admin: &mut Conn, sql: &str, code: u16) {
    let Err(mysql::Error::MySqlError(error)) = admin.query_drop(sql) else {
        panic!("Expected SQL error {code} for {sql}");
    };
    assert_eq!(error.code, code, "{sql}: {error}");
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn accepts_cross_area_projects_and_shared_resources() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    seed_targets(&mut admin);

    let (area, other_area) = (id(2), id(5));
    let (issue, other_issue) = (id(10), id(13));
    let (project, completed, archived) = (id(PROJECT), id(40), id(41));
    let (resource, label, revision) = (id(RESOURCE), id(LABEL), id(4));
    for sql in [
        format!(
            "INSERT INTO areas (id, slug, name, revision)
                 VALUES ('{other_area}', 'other-root', 'Other root', '{revision}')"
        ),
        format!("UPDATE issues SET area_id='{other_area}' WHERE id='{other_issue}'"),
        format!(
            "INSERT INTO projects (id, name, status, revision) VALUES
                 ('{completed}', 'Completed', 'completed', '{revision}'),
                 ('{archived}', 'Archived', 'archived', '{revision}')"
        ),
        format!(
            "INSERT INTO project_issues VALUES ('{project}', '{issue}'),
                 ('{project}', '{other_issue}'), ('{completed}', '{other_issue}'),
                 ('{archived}', '{other_issue}')"
        ),
        format!(
            "INSERT INTO area_resources VALUES ('{area}', '{resource}'), ('{other_area}', '{resource}')"
        ),
        format!(
            "INSERT INTO issue_resources VALUES ('{issue}', '{resource}'), ('{other_issue}', '{resource}')"
        ),
        format!(
            "INSERT INTO issue_labels VALUES ('{issue}', '{label}'), ('{other_issue}', '{label}')"
        ),
    ] {
        admin.query_drop(sql).unwrap();
    }
    commit(&mut admin);
    fixture.check(0, &mut admin);

    let baseline = storage_state(&mut admin);
    // The unique index uses the fixture's case- and accent-insensitive SQL collation.
    assert_sql_error(
        &mut admin,
        &format!(
            "INSERT INTO labels (id, name, revision) VALUES ('{}', 'fixture qa', '{revision}')",
            id(42)
        ),
        1062,
    );
    for table in [
        "project_issues",
        "area_resources",
        "issue_resources",
        "issue_labels",
    ] {
        assert_sql_error(
            &mut admin,
            &format!("INSERT INTO {table} SELECT * FROM {table} LIMIT 1"),
            1062,
        );
    }
    assert_eq!(storage_state(&mut admin), baseline);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rejects_noncanonical_relation_ids_and_project_status() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    seed_targets(&mut admin);

    let project = id(PROJECT);
    let noncanonical_status = id(36);
    let (issue, other_issue) = (id(12), id(13));
    let (resource, label) = (id(RESOURCE), id(LABEL));
    let invalid_project = "invalid-project-fixture";
    let invalid_resource = resource.replace("-7000-", "-4000-");
    let invalid_label = label.to_ascii_uppercase();
    let project_alias = project.to_ascii_uppercase();
    let issue_alias = other_issue.to_ascii_uppercase();

    for sql in [
        format!(
            "INSERT INTO projects (id, name, status, revision) VALUES
                 ('{noncanonical_status}', 'Fixture', 'ACTIVE', 'fixture-revision'),
                 ('{invalid_project}', 'Fixture', 'active', 'fixture-revision')"
        ),
        format!("UPDATE resources SET id='{invalid_resource}' WHERE id='{resource}'"),
        format!("UPDATE labels SET id='{invalid_label}' WHERE id='{label}'"),
        // Case-insensitive FK comparisons accept these spellings of existing IDs.
        format!(
            "INSERT INTO project_issues VALUES ('{project_alias}', '{issue}'), ('{project}', '{issue_alias}')"
        ),
    ] {
        admin.query_drop(sql).unwrap();
    }
    commit(&mut admin);
    let report = fixture.check(4, &mut admin);
    assert_diagnostics(
        &report,
        &[
            (
                "INVALID_PROJECT_STATUS",
                "projects",
                &[&noncanonical_status],
            ),
            ("INVALID_ID", "projects", &[]),
            ("INVALID_ID", "resources", &[]),
            ("INVALID_ID", "labels", &[]),
            ("INVALID_ID", "project_issues", &[&issue]),
            ("INVALID_ID", "project_issues", &[&project]),
        ],
    );
    let report_text = report.to_string();
    for invalid_id in [
        invalid_project,
        &invalid_resource,
        &invalid_label,
        &project_alias,
        &issue_alias,
    ] {
        assert!(!report_text.contains(invalid_id));
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn reports_missing_join_endpoints_only_in_the_committed_snapshot() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    seed_targets(&mut admin);
    commit(&mut admin);
    let valid = fixture.check(0, &mut admin);
    let baseline = storage_state(&mut admin);

    let (project, resource, label) = (id(PROJECT), id(RESOURCE), id(LABEL));
    let (area, issue) = (id(2), id(12));
    let (missing_left, missing_right) = (id(90), id(91));
    let rows = [
        ("project_issues", &project, &issue),
        ("area_resources", &area, &resource),
        ("issue_resources", &issue, &resource),
        ("issue_labels", &issue, &label),
    ]
    .into_iter()
    .flat_map(|(table, left, right)| {
        [
            (table, [left.as_str(), missing_right.as_str()]),
            (table, [missing_left.as_str(), right.as_str()]),
        ]
    })
    .collect::<Vec<_>>();
    let inserts: Vec<_> = rows
        .iter()
        .map(|(table, [left, right])| format!("INSERT INTO {table} VALUES ('{left}', '{right}')"))
        .collect();
    for sql in &inserts {
        assert_sql_error(&mut admin, sql, 1452);
    }
    assert_eq!(storage_state(&mut admin), baseline);

    // Force a commit of referential corruption in this disposable database with the same DDL.
    admin
        .query_drop("SET SESSION foreign_key_checks=0")
        .unwrap();
    for sql in &inserts {
        admin.query_drop(sql).unwrap();
    }
    let dirty = storage_state(&mut admin);
    assert_ne!(dirty.1, baseline.1);
    assert_eq!(dirty.2, baseline.2);
    assert_eq!(fixture.check(0, &mut admin), valid);
    admin.query_drop("CALL DOLT_ADD('.')").unwrap();
    assert_eq!(storage_state(&mut admin).2, dirty.1);
    assert_eq!(fixture.check(0, &mut admin), valid);
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
fn validates_committed_label_names_and_redacts_malformed_values() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let boundary = "é".repeat(100);
    for (number, name) in [
        (0x31, "Fíxture QA"),
        (0x32, "버그 수정"),
        (0x33, "e\u{0301}tiquette"),
        (0x34, boundary.as_str()),
    ] {
        admin
            .exec_drop(
                "INSERT INTO labels (id, name, revision) VALUES (?, ?, 'fixture-revision')",
                (id(number), name),
            )
            .unwrap();
    }
    commit(&mut admin);
    let valid = fixture.check(0, &mut admin);
    let baseline = storage_state(&mut admin);

    let invalid_id = "invalid-label-id-fixture";
    let malformed = [
        (id(0x35), ""),
        (id(0x36), "   "),
        (id(0x37), "\u{00A0}leading"),
        (id(0x38), "trailing\u{3000}"),
        (id(0x39), "interior\tcontrol"),
        (invalid_id.to_owned(), "bad\ncontrol"),
    ];
    for (label_id, name) in &malformed {
        admin
            .exec_drop(
                "INSERT INTO labels (id, name, revision) VALUES (?, ?, 'fixture-revision')",
                (label_id, name),
            )
            .unwrap();
    }
    let dirty = storage_state(&mut admin);
    assert_ne!(dirty.1, baseline.1);
    assert_eq!(dirty.2, baseline.2);
    assert_eq!(fixture.check(0, &mut admin), valid);
    admin.query_drop("CALL DOLT_ADD('labels')").unwrap();
    assert_eq!(storage_state(&mut admin).2, dirty.1);
    assert_eq!(fixture.check(0, &mut admin), valid);

    commit(&mut admin);
    let report = fixture.check(4, &mut admin);
    let (id35, id36, id37, id38, id39) = (id(0x35), id(0x36), id(0x37), id(0x38), id(0x39));
    assert_diagnostics(
        &report,
        &[
            ("INVALID_LABEL_NAME", "labels", &[&id35]),
            ("INVALID_LABEL_NAME", "labels", &[&id36]),
            ("INVALID_LABEL_NAME", "labels", &[&id37]),
            ("INVALID_LABEL_NAME", "labels", &[&id38]),
            ("INVALID_LABEL_NAME", "labels", &[&id39]),
            ("INVALID_ID", "labels", &[]),
            ("INVALID_LABEL_NAME", "labels", &[]),
        ],
    );
    let report_text = report.to_string();
    assert!(!report_text.contains(invalid_id));
    for (_, name) in &malformed {
        if !name.trim().is_empty() {
            assert!(!report_text.contains(name));
        }
    }
    assert_eq!(fixture.check(4, &mut admin), report);

    for (label_id, _) in &malformed {
        admin
            .exec_drop("DELETE FROM labels WHERE id=?", (label_id,))
            .unwrap();
    }
    commit(&mut admin);
    let restored = fixture.check(0, &mut admin);
    assert_eq!(restored["data"], valid["data"]);
}

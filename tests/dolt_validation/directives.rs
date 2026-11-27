use mysql::Conn;
use mysql::prelude::Queryable;

use super::{ADMIN, Fixture, assert_diagnostics, commit, id, storage_state};

const PROJECT: u32 = 0x4a;

fn insert_project(admin: &mut Conn) {
    admin
        .exec_drop(
            "INSERT INTO projects (id, name, status, revision) VALUES (?, 'Fixture', 'archived', 'fixture-revision')",
            (id(PROJECT),),
        )
        .unwrap();
}

fn insert_directive(admin: &mut Conn, id: &str) {
    admin
        .exec_drop(
            "INSERT INTO directives (id, scope_kind, title, body, status, issuer_ref, source_ref, revision, created_at, updated_at)
             VALUES (?, 'workspace', 'Fixture', 'Synthetic Directive body', 'proposed', 'fixture-issuer',
                     'fixture-source', 'fixture-revision', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (id,),
        )
        .unwrap();
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn accepts_directive_scopes_statuses_and_optional_validity_bounds() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    insert_project(&mut admin);
    let (workspace, old_area, area, project, withdrawn) =
        (id(0x60), id(0x61), id(0x62), id(0x63), id(0x64));
    for directive in [&workspace, &old_area, &area, &project, &withdrawn] {
        insert_directive(&mut admin, directive);
    }
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='area', area_id=?, status='superseded', expires_at='2001-01-01' WHERE id=?",
            (id(2), &old_area),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='area', area_id=?, include_descendants=TRUE,
                    status='active', approval_ref='fixture-approval', supersedes_id=?,
                    valid_from='2001-01-01', expires_at='2002-01-01' WHERE id=?",
            (id(2), &old_area, &area),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='project', project_id=?, status='active',
                    approval_ref='fixture-approval', valid_from='2999-01-01' WHERE id=?",
            (id(PROJECT), &project),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET status='withdrawn', approval_ref='fixture-approval' WHERE id=?",
            (&withdrawn,),
        )
        .unwrap();
    commit(&mut admin);
    fixture.check(0, &mut admin);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rejects_noncanonical_directive_fields_and_reference_ids() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    insert_project(&mut admin);
    let (workspace, area, project, replacement, predecessor) =
        (id(0x60), id(0x61), id(0x62), id(0x63), id(0x6a));
    let project_alias = id(PROJECT).to_ascii_uppercase();
    let predecessor_alias = predecessor.to_ascii_uppercase();
    let invalid_id = "fixture-invalid-directive-id";
    for directive in [&workspace, &area, &project, &replacement, &predecessor] {
        insert_directive(&mut admin, directive);
    }
    insert_directive(&mut admin, invalid_id);
    admin.exec_drop(
        "UPDATE directives SET scope_kind='WORKSPACE', status='ACTIVE', approval_ref='fixture-approval' WHERE id=?",
        (&workspace,),
    ).unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='area', area_id=?, include_descendants=2 WHERE id=?",
            (id(2), &area),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='project', project_id=? WHERE id=?",
            (&project_alias, &project),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET supersedes_id=? WHERE id=?",
            (&predecessor_alias, &replacement),
        )
        .unwrap();
    commit(&mut admin);

    let report = fixture.check(4, &mut admin);
    assert_diagnostics(
        &report,
        &[
            ("INVALID_DIRECTIVE_SCOPE", "directives", &[&workspace]),
            ("INVALID_DIRECTIVE_STATUS", "directives", &[&workspace]),
            ("INVALID_DIRECTIVE_SCOPE", "directives", &[&area]),
            ("INVALID_ID", "directives", &[&project]),
            ("INVALID_ID", "directives", &[&replacement]),
            ("INVALID_ID", "directives", &[]),
        ],
    );
    let report_text = report.to_string();
    for value in [
        invalid_id,
        &project_alias,
        &predecessor_alias,
        "Synthetic Directive body",
        "fixture-approval",
    ] {
        assert!(!report_text.contains(value));
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn checks_directive_references_and_supersession_cycles_at_the_committed_snapshot() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let (area, project, dangling, first, second, third) =
        (id(0x60), id(0x61), id(0x62), id(0x63), id(0x64), id(0x65));
    for directive in [&area, &project, &dangling, &first, &second, &third] {
        insert_directive(&mut admin, directive);
    }
    commit(&mut admin);
    let baseline = fixture.check(0, &mut admin);
    let before = storage_state(&mut admin);

    let (missing_area, missing_project, missing_directive) = (id(0x90), id(0x91), id(0x92));
    admin
        .query_drop("SET SESSION foreign_key_checks=0")
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='area', area_id=? WHERE id=?",
            (&missing_area, &area),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='project', project_id=? WHERE id=?",
            (&missing_project, &project),
        )
        .unwrap();
    for (target, directive) in [
        (&missing_directive, &dangling),
        (&second, &first),
        (&third, &second),
        (&first, &third),
    ] {
        admin
            .exec_drop(
                "UPDATE directives SET supersedes_id=? WHERE id=?",
                (target, directive),
            )
            .unwrap();
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
    assert_diagnostics(
        &report,
        &[
            ("MISSING_REFERENCE", "directives", &[&area, &missing_area]),
            (
                "MISSING_REFERENCE",
                "directives",
                &[&project, &missing_project],
            ),
            (
                "MISSING_REFERENCE",
                "directives",
                &[&dangling, &missing_directive],
            ),
            (
                "DIRECTIVE_SUPERSESSION_CYCLE",
                "directives",
                &[&first, &second, &third],
            ),
        ],
    );
    assert_eq!(fixture.check(4, &mut admin), report);
}

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

fn read(fixture: &Fixture, args: &[&str], exit: i32) -> serde_json::Value {
    let mut command = vec!["directive"];
    command.extend_from_slice(args);
    let port = fixture.port.to_string();
    let workspace = id(1);
    command.extend([
        "--workspace-id",
        &workspace,
        "--database",
        super::DATABASE,
        "--port",
        &port,
        "--contract",
        "2",
        "--json",
    ]);
    let output = fixture.run_hive(&command, &fixture.password);
    assert!(output.stderr.is_empty(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(exit), "{report}");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], format!("directive {}", args[0]));
    assert_eq!(report["ok"], exit == 0);
    report
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn reads_exact_directive_scope_and_historical_status_from_committed_main() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    insert_project(&mut admin);
    let (root, child, project) = (id(2), id(3), id(PROJECT));
    let (workspace, historical, active, descendant, project_directive) =
        (id(0x60), id(0x61), id(0x62), id(0x63), id(0x64));
    for directive in [
        &workspace,
        &historical,
        &active,
        &descendant,
        &project_directive,
    ] {
        insert_directive(&mut admin, directive);
    }
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='area', area_id=?, status='withdrawn',
            source_ref='references/synthetic.md', body='Selected historical body' WHERE id=?",
            (&root, &historical),
        )
        .unwrap();
    admin.exec_drop(
        "UPDATE directives SET scope_kind='area', area_id=?, status='active',
            approval_ref='historical approval', include_descendants=TRUE, expires_at='2002-01-01' WHERE id=?",
        (&root, &active),
    ).unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='area', area_id=? WHERE id=?",
            (&child, &descendant),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='project', project_id=? WHERE id=?",
            (&project, &project_directive),
        )
        .unwrap();
    commit(&mut admin);
    let committed = storage_state(&mut admin).0;

    // Both the staged body and the working status differ from the committed record.
    admin
        .exec_drop(
            "UPDATE directives SET body='Staged body' WHERE id=?",
            (&historical,),
        )
        .unwrap();
    admin.query_drop("CALL DOLT_ADD('.')").unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET status='superseded' WHERE id=?",
            (&historical,),
        )
        .unwrap();
    let before = storage_state(&mut admin);

    let page_args = ["list", "--area", &root, "--status", "all", "--limit", "1"];
    let first = read(&fixture, &page_args, 0);
    assert_eq!(first, read(&fixture, &page_args, 0));
    assert_eq!(first["context"]["db_commit"], committed);
    assert_eq!(first["data"]["items"][0]["id"], historical);
    assert_eq!(first["data"]["items"][0]["status"], "withdrawn");
    assert_eq!(first["data"]["truncated"], true);
    for key in [
        "body",
        "issuer_ref",
        "source_ref",
        "approval_ref",
        "applicable",
    ] {
        assert!(first["data"]["items"][0].get(key).is_none());
    }
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    let second = read(
        &fixture,
        &[&page_args[..], &["--cursor", cursor]].concat(),
        0,
    );
    assert_eq!(second["data"]["items"][0]["id"], active);
    assert_eq!(second["data"]["items"][0]["include_descendants"], true);
    assert_eq!(second["data"]["next_cursor"], serde_json::Value::Null);
    assert_eq!(
        read(&fixture, &["list", "--area", &root], 0)["data"]["items"][0]["id"],
        active
    );
    assert_eq!(
        read(
            &fixture,
            &[
                "list", "--area", &child, "--status", "all", "--limit", "1", "--cursor", cursor
            ],
            5
        )["error"]["code"],
        "STALE_CURSOR",
    );

    for (scope, expected) in [
        (vec!["--workspace-scope"], &workspace),
        (vec!["--area", &child], &descendant),
        (vec!["--project", &project], &project_directive),
    ] {
        let list = read(
            &fixture,
            &[&["list", "--status", "all"][..], &scope].concat(),
            0,
        );
        assert_eq!(list["data"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(list["data"]["items"][0]["id"], *expected);
        assert_eq!(
            read(&fixture, &[&["show", &historical][..], &scope].concat(), 3)["error"]["code"],
            "NOT_FOUND",
        );
    }
    let detail = read(&fixture, &["show", &historical, "--area", &root], 0);
    let record = &detail["data"]["directive"];
    assert_eq!(record["body"], "Selected historical body");
    assert_eq!(record["status"], "withdrawn");
    assert_eq!(record["source_ref"], "references/synthetic.md");
    assert_eq!(record["issuer_ref"], "fixture-issuer");
    assert!(!detail.to_string().contains("Synthetic Directive body"));
    assert_eq!(storage_state(&mut admin), before);

    commit(&mut admin);
    let advanced = storage_state(&mut admin);
    assert_ne!(advanced.0, before.0);
    let stale = read(
        &fixture,
        &[&page_args[..], &["--cursor", cursor]].concat(),
        5,
    );
    assert_eq!(stale["error"]["code"], "STALE_CURSOR");
    assert_eq!(storage_state(&mut admin), advanced);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn refuses_missing_corrupt_and_oversize_directive_reads() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let (large, corrupt) = (id(0x61), id(0x62));
    insert_directive(&mut admin, &large);
    insert_directive(&mut admin, &corrupt);
    admin
        .exec_drop(
            "UPDATE directives SET body=REPEAT('x', 65537) WHERE id=?",
            (&large,),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE directives SET scope_kind='area', area_id=?, include_descendants=2 WHERE id=?",
            (id(2), &corrupt),
        )
        .unwrap();
    commit(&mut admin);
    let before = storage_state(&mut admin);
    let page = read(
        &fixture,
        &["list", "--workspace-scope", "--status", "all"],
        0,
    );
    assert_eq!(page["data"]["items"][0]["id"], large);
    for (args, exit, code) in [
        (
            vec!["show", &large, "--workspace-scope"],
            2,
            "CONTEXT_LIMIT",
        ),
        (
            vec!["show", &corrupt, "--area", &id(2)],
            4,
            "INVALID_RECORD",
        ),
        (
            vec!["list", "--area", &id(2), "--status", "all"],
            4,
            "INVALID_RECORD",
        ),
        (vec!["list", "--project", &id(0x90)], 3, "NOT_FOUND"),
        (vec!["show", &id(0x90), "--workspace-scope"], 3, "NOT_FOUND"),
    ] {
        let result = read(&fixture, &args, exit);
        assert_eq!(result["error"]["code"], code);
        assert!(result.get("data").is_none());
    }
    for (workspace, password, exit, code) in [
        (
            id(0x90),
            fixture.password.as_str(),
            7,
            "AUTHORITY_UNVERIFIED",
        ),
        (id(1), "wrong-fixture-password", 6, "UNAUTHORIZED"),
    ] {
        let output = fixture.run_hive(
            &[
                "directive",
                "list",
                "--workspace-scope",
                "--workspace-id",
                &workspace,
                "--database",
                super::DATABASE,
                "--port",
                &fixture.port.to_string(),
                "--contract",
                "2",
                "--json",
            ],
            password,
        );
        assert_eq!(output.status.code(), Some(exit), "{output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["error"]["code"], code);
        assert!(report.get("data").is_none());
    }
    assert_eq!(storage_state(&mut admin), before);
}

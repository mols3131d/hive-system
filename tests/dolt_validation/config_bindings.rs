use std::fs;

use mysql::prelude::Queryable;
use serde_json::{Value, json};

use super::{ADMIN, DATABASE, Fixture, commit, id, storage_state};

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn verifies_resolved_binding_against_committed_scope() {
    let fixture = Fixture::start();
    let mut writer = fixture.connect(ADMIN);
    let (workspace, area, child, resource, project, revision) =
        (id(1), id(2), id(3), id(20), id(21), id(22));
    let single_resource = id(23);
    let standalone_resource = id(24);
    writer
        .query_drop(format!(
            "INSERT INTO resources VALUES
             ('{resource}', 'directory', 'Shared', NULL, '{revision}'),
             ('{single_resource}', 'directory', 'Single', NULL, '{revision}'),
             ('{standalone_resource}', 'directory', 'Standalone', NULL, '{revision}')"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO area_resources VALUES
             ('{area}', '{resource}'), ('{area}', '{single_resource}')"
        ))
        .unwrap();
    writer
        .query_drop(format!(
            "INSERT INTO projects (id, name, status, revision)
             VALUES ('{project}', 'Fixture', 'archived', '{revision}')"
        ))
        .unwrap();
    for number in 3..=19 {
        let linked = id(number);
        if number > 3 {
            writer
                .exec_drop(
                    "INSERT INTO areas (id, slug, name, revision) VALUES (?, ?, 'Fixture', ?)",
                    (&linked, format!("area-{number}"), &revision),
                )
                .unwrap();
        }
        writer
            .exec_drop(
                "INSERT INTO area_resources VALUES (?, ?)",
                (&linked, &resource),
            )
            .unwrap();
    }
    // Area 25 is a valid committed Area without a committed link to the Resource.
    let unlinked = id(25);
    writer
        .exec_drop(
            "INSERT INTO areas (id, slug, name, revision) VALUES (?, 'unlinked', 'Unlinked', ?)",
            (&unlinked, &revision),
        )
        .unwrap();
    commit(&mut writer);
    writer
        .exec_drop(
            "INSERT INTO area_resources VALUES (?, ?)",
            (&unlinked, &resource),
        )
        .unwrap();
    writer
        .query_drop("CALL DOLT_ADD('area_resources')")
        .unwrap();
    writer
        .exec_drop("DELETE FROM projects WHERE id = ?", (&project,))
        .unwrap();
    let before = storage_state(&mut writer);

    let external = fixture.root.join("external");
    fs::create_dir(&external).unwrap();
    let external_file = external.join("fixture.txt");
    fs::write(&external_file, "synthetic external file").unwrap();
    let descriptor = fixture.root.join("hive-workspace.yml");
    let descriptor_contents = format!(
        "version: 2\nworkspace_id: {workspace}\nschema_version: 1\npolicy_revision: {}\n",
        "a".repeat(40)
    );
    fs::write(&descriptor, &descriptor_contents).unwrap();
    let config = fixture.root.join("home/.config/hive/v2");
    fs::create_dir_all(&config).unwrap();
    let binding_path = config.join("bindings.yml");
    let set_binding = |resource: &str, defaults: &str| {
        fs::write(
            &binding_path,
            format!(
                "version: 2\nbindings:\n  - path: {}\n    workspace: {}\n    resource_id: {resource}\n{defaults}",
                external.display(),
                fixture.root.display(),
            ),
        )
        .unwrap();
    };
    set_binding(&resource, &format!("    project_id: {project}\n"));

    let port = fixture.port.to_string();
    let run = |expected_id: &str, options: &[&str], password: &str, exit| {
        let args = [
            "--contract",
            "2",
            "--json",
            "config",
            "show",
            "--resource",
            external.to_str().unwrap(),
            "--workspace-id",
            expected_id,
            "--database",
            DATABASE,
            "--port",
            &port,
        ];
        let binding_contents = fs::read(&binding_path).unwrap();
        let output = fixture.run_hive(&[&args[..], options].concat(), password);
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {output:?}"));
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["ok"], exit == 0);
        assert!(output.stderr.is_empty());
        assert!(!report.to_string().contains(&fixture.password));
        assert_eq!(fs::read(&binding_path).unwrap(), binding_contents);
        report
    };
    let read = |options: &[&str], exit| run(&workspace, options, &fixture.password, exit);
    let ambiguous = read(&[], 2);
    assert_eq!(ambiguous["error"]["code"], "AMBIGUOUS_SCOPE");
    assert_eq!(
        ambiguous["error"]["input"],
        json!({"kind": "resource", "source": "binding"})
    );
    let details = &ambiguous["error"]["details"];
    assert_eq!(details["truncated"], true);
    let candidates = details["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 16);
    assert_eq!(candidates[0], json!({"id": area, "slug": "root"}));
    assert_eq!(candidates[1], json!({"id": child, "slug": "child"}));
    assert_eq!(candidates[15], json!({"id": id(17), "slug": "area-17"}));

    let selected = read(&["--area", &child], 0);
    let data = &selected["data"];
    assert_eq!(data["area_id"], child);
    assert_eq!(data["project_id"], project);
    assert_eq!(data["resource_id"], resource);
    assert_eq!(data["dolt_commit"], before.0);
    assert_eq!(
        data["sources"],
        json!({"workspace":"binding","resource":"binding","area":"cli","project":"binding","output_format":"cli"})
    );
    assert_eq!(
        data["verification"],
        json!({"descriptor":"verified","committed_identity":"verified","scope_membership":"verified","runtime":"unverified"})
    );
    let missing_project = id(99);
    let project_failure = read(&["--area", &child, "--project", &missing_project], 3);
    assert_eq!(project_failure["error"]["code"], "NOT_FOUND");
    assert_eq!(
        project_failure["error"]["input"],
        json!({"kind": "project", "source": "cli"})
    );

    set_binding(&resource, &format!("    area_id: {unlinked}\n"));
    for (options, source) in [(&[][..], "binding"), (&["--area", &unlinked][..], "cli")] {
        let failure = read(options, 3);
        assert_eq!(failure["error"]["code"], "NOT_FOUND");
        assert_eq!(
            failure["error"]["input"],
            json!({"kind": "area", "source": source})
        );
        assert!(failure.get("data").is_none());
        let encoded = failure.to_string();
        for private in [
            unlinked.as_str(),
            resource.as_str(),
            workspace.as_str(),
            fixture.root.to_str().unwrap(),
        ] {
            assert!(!encoded.contains(private));
        }
    }
    assert_eq!(
        read(&["--area", &child], 0)["data"]["sources"]["area"],
        "cli"
    );
    set_binding(&single_resource, "");
    let inferred = read(&[], 0);
    assert_eq!(inferred["data"]["area_id"], area);
    assert_eq!(inferred["data"]["sources"]["area"], "binding");
    set_binding(&standalone_resource, "");
    assert!(read(&[], 0)["data"].get("area_id").is_none());
    set_binding(&id(99), "");
    let missing_resource = read(&[], 3);
    assert_eq!(missing_resource["error"]["code"], "NOT_FOUND");
    assert_eq!(
        missing_resource["error"]["input"],
        json!({"kind": "resource", "source": "binding"})
    );
    assert_eq!(
        run(&id(99), &[], "", 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    let bad_auth = run(&workspace, &[], "incorrect-synthetic-password", 6);
    assert_eq!(bad_auth["error"]["code"], "UNAUTHORIZED");
    assert!(
        !bad_auth
            .to_string()
            .contains("incorrect-synthetic-password")
    );

    assert_eq!(storage_state(&mut writer), before);
    assert_eq!(fs::read_to_string(descriptor).unwrap(), descriptor_contents);
    assert_eq!(fs::read_dir(&external).unwrap().count(), 1);
    assert_eq!(
        fs::read_to_string(external_file).unwrap(),
        "synthetic external file"
    );
}

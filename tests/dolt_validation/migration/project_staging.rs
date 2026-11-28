use super::{Command, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, sha256_hex};

#[test]
#[ignore = "requires HIVE_STAGING_INSTALLED_BIN and pinned Dolt and Git"]
fn installed_cli_stages_explicit_cross_area_project_memberships() {
    const POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
    const GENERAL: &str = "tasks/_general/backlog/same/README.md";
    const APP: &str = "tasks/sample-app/backlog/same/README.md";
    const UNLINKED: &str = "tasks/sample-app/backlog/unlinked/README.md";
    const HELD: &str = "tasks/sample-app/active/waiting/README.md";
    const EXTERNAL: &str = "tasks/external-team/backlog/foreign/README.md";
    const DUPLICATE: &str = "tasks/_general/backlog/duplicate/README.md";
    const README: &str = "tasks/sample-app/README.md";
    const URL: &str = "https://forge.example/synthetic/projects";
    const NAME: &str = "Synthetic Project Name Sentinel";
    const DESCRIPTION: &str = "Synthetic Project Description with café and 'quotes'.";
    const CREATED: &str = "2024-02-01T12:00:00Z";
    const UPDATED: &str = "2024-02-02T12:00:00Z";

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let home = root.join("home");
    let source = root.join("source");
    let config = root.join("config");
    let target = root.join("inactive");
    let mapping_file = root.join("mapping.json");
    let hive = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").unwrap();
    let dolt = std::env::var_os("HIVE_DOLT_BIN").unwrap();
    let path = std::env::var_os("PATH").unwrap();
    fs::create_dir(&home).unwrap();
    init_git(
        &source,
        &home,
        "Synthetic Operator",
        "operator@example.invalid",
    );

    let body = format!("# Synthetic Task\n\nCreated: {CREATED}\nUpdated: {UPDATED}\n");
    let record = |fields| format!("---\ndescription: Synthetic Task\n{fields}---\n{body}");
    for (name, contents) in [
        ("AGENTS.md", format!("hive-system@{POLICY_PIN}\n")),
        (
            "hive.yml",
            "version: 1\ncoordination:\n  mode: local\n".into(),
        ),
        (README, format!("# Sample scope\n\n{URL}\n")),
        (GENERAL, record("")),
        (APP, record("title: Sample App Task\n")),
        (UNLINKED, record("title: Independent Sample Task\n")),
        (
            HELD,
            record("status: todo\nclaim: {id: synthetic-claim, holder: fixture-holder}\n"),
        ),
        (EXTERNAL, record("")),
        (DUPLICATE, record("")),
        (
            "tasks/_general/archive/duplicate/README.md",
            record("status: done\n"),
        ),
    ] {
        let file = source.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, contents).unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic Project membership"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let index = fs::read(source.join(".git/index")).unwrap();
    let oid = |name: &str| git_text(&source, &home, &["rev-parse", &format!("{commit}:{name}")]);
    let range = |value: &str| {
        let start = body.find(value).unwrap();
        json!({"start":start, "end":start + value.len()})
    };
    let workspace = id(1);
    let general = id(2);
    let app = id(3);
    let project = id(4);
    let duplicate = id(5);
    let conflict = id(6);
    let independent = id(7);
    let alternate = id(8);
    let missing = id(9);
    let key = |scope: &str, work_id: &str| json!({"scope":scope, "work_id":work_id});
    let task_review = |key: Value, path: &str, area: &str| {
        json!({
            "legacy_key":key, "source_git_blob_sha":oid(path), "area_id":area, "relations":"no_edges",
            "created_at":range(CREATED), "updated_at":range(UPDATED)
        })
    };
    let membership = |id: &str, key: Value, oid: &str, decision: &str| {
        json!({
            "project_id":id, "legacy_key":key, "source_git_blob_sha":oid, "decision":decision
        })
    };
    let good_general = key("_general", "same");
    let good_app = key("sample-app", "same");
    let unlinked = key("sample-app", "unlinked");
    let repeated = membership(&independent, unlinked.clone(), &oid(UNLINKED), "link");
    let repeated_project = json!({"id":duplicate,"name":"Duplicated Project","status":"active"});
    let manifest = json!({
        "version":1, "source_commit":commit, "policy_pin":POLICY_PIN, "workspace_id":workspace,
        "scopes":[
            {"scope":"_general","decision":{"kind":"area","area":{"id":general,"slug":"general","name":"General"}}},
            {"scope":"sample-app","decision":{"kind":"area","area":{"id":app,"slug":"app","name":"App"},
                "project_evidence":[{"path":README,"canonical_ref":URL}]}},
            {"scope":"external-team","decision":{"kind":"external"}}
        ],
        "task_reviews":[
            task_review(good_general.clone(), GENERAL, &general),
            task_review(good_app.clone(), APP, &app),
            task_review(unlinked.clone(), UNLINKED, &app)
        ],
        "project_membership":{
            "projects":[
                {"id":project,"name":NAME,"description":DESCRIPTION,"status":"active"},
                {"id":independent,"name":"Independent Project","status":"completed"},
                {"id":alternate,"name":"Archived Project","status":"archived"},
                repeated_project.clone(), repeated_project,
                {"id":conflict,"name":"First Definition","status":"active"},
                {"id":conflict,"name":"Second Definition","status":"active"}
            ],
            "memberships":[
                membership(&project, good_general.clone(), &oid(GENERAL), "link"),
                membership(&project, good_app.clone(), &oid(APP), "link"),
                membership(&project, key("sample-app","waiting"), &oid(HELD), "link"),
                membership(&project, key("external-team","foreign"), &oid(EXTERNAL), "link"),
                membership(&project, key("_general","duplicate"), &oid(DUPLICATE), "link"),
                membership(&project, unlinked.clone(), &oid(GENERAL), "link"),
                membership(&independent, good_general.clone(), &oid(GENERAL), "unresolved"),
                membership(&independent, good_app.clone(), &oid(APP), "exclude"),
                repeated.clone(), repeated,
                membership(&alternate, good_app.clone(), &oid(APP), "link"),
                membership(&alternate, good_app.clone(), &oid(APP), "exclude"),
                membership(&project, key("sample-app","absent"), &"0".repeat(commit.len()), "link"),
                membership(&missing, good_app.clone(), &oid(APP), "link"),
                membership(&duplicate, good_app, &oid(APP), "link")
            ]
        }
    });
    let run = |action, tasks, mode: Option<&str>, mapping: &str, status| {
        fs::write(&mapping_file, mapping).unwrap();
        let mut command = Command::new(&hive);
        command
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &path)
            .env("TMPDIR", root)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .args(["--contract", "2", "--json", "workspace", action, "--source"])
            .arg(&source)
            .args([
                "--source-commit",
                &commit,
                "--workspace-id",
                &workspace,
                "--mapping-file",
            ])
            .arg(&mapping_file);
        if tasks {
            command.arg("--tasks");
        }
        if let Some(mode) = mode {
            command
                .args(["--mode", mode, "--policy-revision", POLICY, "--target"])
                .arg(&target);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let plan = |tasks, mapping: &Value| {
        run("migration-plan", tasks, None, &mapping.to_string(), 0)["data"].clone()
    };
    let stage = |mode, tasks| {
        run(
            "migration-stage",
            tasks,
            Some(mode),
            &manifest.to_string(),
            0,
        )["data"]
            .clone()
    };

    let mut prior = manifest.clone();
    prior.as_object_mut().unwrap().remove("project_membership");
    let prior = plan(true, &prior);
    assert!(prior.get("project_membership").is_none());
    let no_tasks = plan(false, &manifest);
    assert!(no_tasks.get("task_intake").is_none());
    assert_eq!(
        no_tasks["project_membership"]["counts"]["project_issues"]["prepared"],
        0
    );
    let combined = plan(true, &manifest);
    assert_eq!(prior["task_intake"], combined["task_intake"]);
    assert_eq!(prior["scopes"], combined["scopes"]);

    let section = &combined["project_membership"];
    assert_eq!(
        section["counts"]["projects"],
        json!({"inventoried":7,"prepared":3,"held":4,"excluded_non_record":0})
    );
    assert_eq!(
        section["counts"]["project_issues"],
        json!({"inventoried":15,"prepared":2,"held":13,"excluded_non_record":0})
    );
    assert_eq!(
        section["projects"][0]["name_sha256"],
        sha256_hex(NAME.as_bytes())
    );
    assert_eq!(
        section["projects"][3]["reason_code"],
        "duplicate_project_definition"
    );
    assert_eq!(
        section["projects"][6]["reason_code"],
        "conflicting_project_definition"
    );
    let reasons = [
        "unresolved_project_membership",
        "excluded_project_membership",
        "ambiguous_project_member_task",
        "project_membership_binding_mismatch",
        "unresolved_project_membership",
        "excluded_project_membership",
        "duplicate_project_membership",
        "duplicate_project_membership",
        "conflicting_project_membership",
        "conflicting_project_membership",
        "unresolved_project_membership",
        "unresolved_project_membership",
        "unresolved_project_membership",
    ];
    for (row, reason) in section["memberships"].as_array().unwrap()[2..]
        .iter()
        .zip(reasons)
    {
        assert_eq!(row["status"], "held");
        assert_eq!(row["reason_code"], reason);
        assert!(row.get("issue_id").is_none());
    }
    for row in &section["memberships"].as_array().unwrap()[..2] {
        assert_eq!(row["status"], "prepared");
        assert!(row.get("issue_id").is_none());
    }

    for (pointer, bad) in [
        ("/project_membership", Value::Null),
        ("/project_membership", json!([[], []])),
        (
            "/project_membership/projects/0",
            json!([project, NAME, DESCRIPTION, "active"]),
        ),
        (
            "/project_membership/memberships/0/legacy_key",
            json!(["_general", "same"]),
        ),
        ("/project_membership/memberships/0/decision", json!("guess")),
    ] {
        let mut malformed = manifest.clone();
        *malformed.pointer_mut(pointer).unwrap() = bad;
        let rejected = run(
            "migration-stage",
            true,
            Some("staged-import"),
            &malformed.to_string(),
            2,
        );
        assert_eq!(rejected["error"]["code"], "INVALID_CONFIG", "{pointer}");
        assert!(rejected.get("data").is_none());
        assert!(!target.exists());
    }
    let raw = manifest.to_string();
    for replacement in [
        "\"decision\":\"link\",\"decision\":\"link\"",
        "\"decision\":\"link\",\"unknown\":true",
    ] {
        let malformed = raw.replacen("\"decision\":\"link\"", replacement, 1);
        assert_ne!(malformed, raw);
        let rejected = run("migration-plan", true, None, &malformed, 2);
        assert_eq!(rejected["error"]["code"], "INVALID_CONFIG");
    }

    let project_only = stage("dry-run", false);
    assert_eq!(project_only["coverage"], "scopes_and_projects");
    assert_eq!(project_only["counts"]["projects"]["imported"], 3);
    assert_eq!(project_only["counts"]["project_issues"]["imported"], 0);
    assert!(project_only["counts"].get("tasks").is_none());
    assert_eq!(project_only["target"]["issue_number_next_value"], 1);
    assert!(!target.exists());

    let dry = stage("dry-run", true);
    assert_eq!(dry["coverage"], "mixed");
    assert_eq!(dry["included_surfaces"], json!(["tasks", "projects"]));
    assert_eq!(dry["counts"]["project_issues"]["imported"], 2);
    assert_eq!(dry["target"]["issue_number_next_value"], 1);
    assert_eq!(dry["target"]["projected_issue_number_next_value"], 4);
    assert_eq!(dry["checks"]["transaction"], "rolled_back");
    assert_eq!(dry["checks"]["target_project_rows"], 0);
    assert_eq!(dry["checks"]["target_project_issue_rows"], 0);
    assert!(!target.exists());

    let staged = stage("staged-import", true);
    assert_eq!(staged["counts"], dry["counts"]);
    assert_eq!(staged["checks"]["target_project_rows"], 3);
    assert_eq!(staged["checks"]["target_project_issue_rows"], 2);
    assert_eq!(staged["target"]["issue_number_next_value"], 4);
    assert_eq!(staged["checks"]["committed_record_validation"], "passed");
    let report = staged.to_string();
    for excluded in [
        NAME,
        DESCRIPTION,
        GENERAL,
        APP,
        HELD,
        "fixture-holder",
        root.to_str().unwrap(),
    ] {
        assert!(!report.contains(excluded), "{excluded}");
    }
    let key_digest = |scope, name| sha256_hex(format!("{scope}\0{name}").as_bytes());
    let assigned = staged["id_map"].as_array().unwrap();
    assert!(
        !assigned
            .iter()
            .any(|row| { row["legacy_key_sha256"] == key_digest("sample-app", "waiting") })
    );
    assert!(assigned.iter().any(|row| {
        row["legacy_key_sha256"] == key_digest("sample-app", "unlinked") && row["issue_number"] == 3
    }));
    let ids = &staged["project_membership"]["memberships"];
    assert_eq!(ids[0]["legacy_key_sha256"], key_digest("_general", "same"));
    assert_eq!(
        ids[1]["legacy_key_sha256"],
        key_digest("sample-app", "same")
    );
    assert_ne!(ids[0]["issue_id"], ids[1]["issue_id"]);
    for row in &ids.as_array().unwrap()[2..] {
        assert_eq!(row["status"], "held");
        assert!(row.get("issue_id").is_none());
        assert!(row.get("issue_number").is_none());
    }

    let snapshot = staged["target"]["confirmed_dolt_commit"].as_str().unwrap();
    let baseline = staged["target"]["base_dolt_commit"].as_str().unwrap();
    let query = |query: &str| {
        let output = Command::new(&dolt)
            .env_clear()
            .env("HOME", &home)
            .env("DOLT_ROOT_PATH", &home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true")
            .current_dir(target.join("db"))
            .args(["sql", "-r", "json", "-q", query])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["rows"].clone()
    };
    assert_eq!(
        query(&format!(
            "SELECT name, description, status FROM projects AS OF '{snapshot}' WHERE id = '{project}'"
        )),
        json!([{"name":NAME,"description":DESCRIPTION,"status":"active"}])
    );
    assert_eq!(
        query(&format!(
            "SELECT p.project_id, p.issue_id, i.area_id, i.number FROM project_issues AS OF '{snapshot}' AS p JOIN issues AS OF '{snapshot}' AS i ON p.issue_id=i.id ORDER BY i.number"
        )),
        json!([
            {"project_id":project,"issue_id":ids[0]["issue_id"],"area_id":general,"number":1},
            {"project_id":project,"issue_id":ids[1]["issue_id"],"area_id":app,"number":2}
        ])
    );
    assert_eq!(
        query(&format!(
            "SELECT number FROM issues AS OF '{snapshot}' ORDER BY number"
        )),
        json!([{"number":1},{"number":2},{"number":3}])
    );
    assert_eq!(
        query(&format!(
            "SELECT COUNT(*) AS links FROM project_issues AS OF '{baseline}'"
        )),
        json!([{"links":0}])
    );
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
    for dir in [&config, &root.join("data"), &root.join("state")] {
        assert!(!dir.exists());
    }
}

#[test]
#[ignore = "requires HIVE_STAGING_INSTALLED_BIN and pinned Git"]
fn installed_cli_requires_project_status_and_membership_decision_strings_at_both_pins() {
    const EARLIER_PIN: &str = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";
    const TARGET_POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
    const TASK: &str = "tasks/_general/backlog/example/README.md";
    const DATE: &str = "2024-02-01T12:00:00Z";

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let home = root.join("home");
    let source = root.join("source");
    let target = root.join("inactive");
    let mapping_file = root.join("mapping.json");
    let hive = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").unwrap();
    let path = std::env::var_os("PATH").unwrap();

    fs::create_dir(&home).unwrap();
    init_git(
        &source,
        &home,
        "Synthetic Operator",
        "operator@example.invalid",
    );
    let task = source.join(TASK);
    fs::create_dir_all(task.parent().unwrap()).unwrap();
    fs::write(
        &task,
        format!("---\ndescription: Synthetic Task\n---\n{DATE}\n"),
    )
    .unwrap();
    let workspace = id(1);
    let area = id(2);
    let project = id(3);

    for pin in [EARLIER_PIN, POLICY_PIN] {
        fs::write(source.join("AGENTS.md"), format!("hive-system@{pin}\n")).unwrap();
        if pin == POLICY_PIN {
            fs::write(
                source.join("hive.yml"),
                "version: 1\ncoordination:\n  mode: local\n",
            )
            .unwrap();
        }
        git(&source, &home, &["add", "."]);
        git(
            &source,
            &home,
            &["commit", "-qm", "Synthetic Project enum policy"],
        );
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let blob = git_text(&source, &home, &["rev-parse", &format!("{commit}:{TASK}")]);
        let legacy_key = json!({"scope":"_general","work_id":"example"});
        let dates = json!({"start":0,"end":DATE.len()});
        let manifest = json!({
            "version":1, "source_commit":commit, "policy_pin":pin, "workspace_id":workspace,
            "scopes":[{"scope":"_general","decision":{"kind":"area","area":{"id":area,"slug":"general","name":"General"}}}],
            "task_reviews":[{
                "legacy_key":legacy_key, "source_git_blob_sha":blob, "area_id":area, "relations":"no_edges",
                "created_at":dates, "updated_at":dates
            }],
            "project_membership":{
                "projects":[{"id":project,"name":"Synthetic Project","status":"active"}],
                "memberships":[{"project_id":project,"legacy_key":legacy_key,"source_git_blob_sha":blob,"decision":"link"}]
            }
        });
        let run = |action: &str, mapping: &Value| {
            fs::write(&mapping_file, mapping.to_string()).unwrap();
            let mut command = Command::new(&hive);
            command
                .current_dir(&home)
                .env_clear()
                .env("HOME", &home)
                .env("PATH", &path)
                .env("TMPDIR", root)
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_STATE_HOME", root.join("state"))
                .args(["--contract", "2", "--json", "workspace", action, "--source"])
                .arg(&source)
                .args([
                    "--source-commit",
                    &commit,
                    "--workspace-id",
                    &workspace,
                    "--tasks",
                    "--mapping-file",
                ])
                .arg(&mapping_file);
            if action == "migration-stage" {
                command
                    .args([
                        "--mode",
                        "staged-import",
                        "--policy-revision",
                        TARGET_POLICY,
                        "--target",
                    ])
                    .arg(&target);
            }
            let output = command.output().unwrap();
            assert!(output.stderr.is_empty(), "{pin} {action}: {output:?}");
            let response = serde_json::from_slice::<Value>(&output.stdout).unwrap();
            (output.status.code(), response)
        };

        let (status, positive) = run("migration-plan", &manifest);
        assert_eq!(status, Some(0), "{pin}: {positive}");
        assert_eq!(positive["data"]["source"]["policy_pin"], pin);
        let prepared = &positive["data"]["project_membership"];
        assert_eq!(prepared["projects"][0]["status"], "prepared", "{pin}");
        assert_eq!(prepared["projects"][0]["project_status"], "active", "{pin}");
        assert_eq!(prepared["memberships"][0]["status"], "prepared", "{pin}");

        for (field, tagged) in [
            (
                "/project_membership/projects/0/status",
                json!({"active":null}),
            ),
            (
                "/project_membership/memberships/0/decision",
                json!({"link":null}),
            ),
        ] {
            let mut malformed = manifest.clone();
            *malformed.pointer_mut(field).unwrap() = tagged;
            for action in ["migration-plan", "migration-stage"] {
                let (status, rejected) = run(action, &malformed);
                assert_eq!(status, Some(2), "{pin} {action} {field}: {rejected}");
                assert_eq!(
                    rejected["error"]["code"], "INVALID_CONFIG",
                    "{pin} {action} {field}"
                );
                assert!(rejected.get("data").is_none(), "{pin} {action} {field}");
                assert!(!target.exists(), "{pin} {action} {field}");
            }
        }
        assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    }
}

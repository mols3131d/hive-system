use super::{Command, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, sha256_hex};

const OLDER_PIN: &str = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";
const TARGET_POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
const REPO: &str = "https://forge.example/synthetic/issue-links";
const OTHER_REPO: &str = "https://forge.example/synthetic/other-app";
const READY: &str = "tasks/synthetic-app/backlog/ready/README.md";
const STALE: &str = "tasks/synthetic-app/backlog/stale/README.md";
const HELD: &str = "tasks/synthetic-app/backlog/held/README.md";
const UNLINKED: &str = "tasks/synthetic-app/backlog/unlinked/README.md";
const OTHER: &str = "tasks/other-app/backlog/ready/README.md";
const README: &str = "tasks/synthetic-app/README.md";
const OTHER_README: &str = "tasks/other-app/README.md";

#[test]
#[ignore = "requires installed Hive and pinned Dolt and Git"]
fn installed_issue_resource_links_on_current_v1_pin() {
    verify_issue_resource_links(POLICY_PIN);
}

#[test]
#[ignore = "requires installed Hive and pinned Dolt and Git"]
fn installed_issue_resource_links_on_older_v1_pin() {
    verify_issue_resource_links(OLDER_PIN);
}

fn verify_issue_resource_links(pin: &str) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let home = root.join("home");
    let source = root.join("source");
    let target = root.join("inactive");
    let mapping_file = root.join("reviewed.json");
    let hive = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").unwrap();
    let dolt = std::env::var_os("HIVE_DOLT_BIN").unwrap();
    let path = std::env::var_os("PATH").unwrap();
    fs::create_dir(&home).unwrap();
    init_git(
        &source,
        &home,
        "Synthetic Operator",
        "fixture@example.invalid",
    );

    let created = "2024-02-01T12:00:00Z";
    let updated = "2024-02-02T12:00:00Z";
    let body = format!(
        "# Synthetic exact Issue label\n\nCreated: {created}\nUpdated: {updated}\n\nSee {REPO}.\n"
    );
    let task = format!("---\ndescription: Synthetic Issue Resource fixture\n---\n{body}");
    let mut fixtures = vec![
        ("AGENTS.md", format!("hive-system@{pin}\n")),
        (README, format!("# Synthetic App\n\n{REPO}\n")),
        (OTHER_README, format!("# Synthetic Other\n\n{OTHER_REPO}\n")),
        (READY, task.clone()),
        (STALE, task.clone()),
        (HELD, task.clone()),
        (UNLINKED, task.clone()),
        (OTHER, task),
    ];
    if pin == POLICY_PIN {
        fixtures.push((
            "hive.yml",
            "version: 1\ncoordination:\n  mode: local\n".to_owned(),
        ));
    }
    for (name, contents) in &fixtures {
        let file = source.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, contents).unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(&source, &home, &["commit", "-qm", "Synthetic link reviews"]);
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let source_index = fs::read(source.join(".git/index")).unwrap();
    let oid = |path: &str| git_text(&source, &home, &["rev-parse", &format!("{commit}:{path}")]);
    let task_oid = oid(READY);
    assert_eq!(task_oid, oid(OTHER));
    let workspace = id(1);
    let area = id(2);
    let other_area = id(3);
    let resource = id(4);
    let stale_resource = id(5);
    let missing_resource = id(6);
    let bad_oid = "0".repeat(commit.len());
    let key = |scope, work_id| json!({"scope":scope, "work_id":work_id});
    let range = |literal: &str| {
        let start = body.find(literal).unwrap();
        json!({"start":start, "end":start + literal.len()})
    };
    let area_mapping = |scope, area, readme, canonical_ref| {
        json!({
            "scope":scope, "decision":{"kind":"area", "area":{"id":area, "slug":scope, "name":scope},
                "project_evidence":[{"path":readme, "canonical_ref":canonical_ref}]}
        })
    };
    let task_review = |scope, work, area| {
        json!({
            "legacy_key":key(scope, work), "source_git_blob_sha":task_oid, "area_id":area,
            "created_at":range(created), "updated_at":range(updated), "relations":"no_edges",
        })
    };
    let resource_binding = |id, name, blob| {
        json!({
            "scope":"synthetic-app", "area_id":area, "resource":{"id":id, "kind":"git-repo", "name":name},
            "evidence_path":README, "evidence_blob_oid":blob,
        })
    };
    let link = |scope, work, blob, resource_id| {
        json!({
            "legacy_key":key(scope, work), "source_git_blob_sha":blob, "resource_id":resource_id,
        })
    };
    let mapping = json!({
        "version":1, "source_commit":commit, "policy_pin":pin, "workspace_id":workspace,
        "scopes":[area_mapping("synthetic-app", &area, README, REPO), area_mapping("other-app", &other_area, OTHER_README, OTHER_REPO)],
        "task_reviews":[
            task_review("synthetic-app", "ready", &area),
            task_review("synthetic-app", "stale", &area),
            task_review("synthetic-app", "unlinked", &area),
            task_review("other-app", "ready", &other_area),
        ],
        "resource_bindings":{"version":1, "bindings":[
            resource_binding(&resource, "Reviewed private Resource label", &oid(README)),
            resource_binding(&stale_resource, "Stale private Resource label", &bad_oid),
        ]},
        "issue_resource_bindings":[
            link("synthetic-app", "ready", &task_oid, &resource),
            link("synthetic-app", "ready", &task_oid, &stale_resource),
            link("synthetic-app", "ready", &task_oid, &missing_resource),
            link("synthetic-app", "stale", &bad_oid, &resource),
            link("synthetic-app", "held", &task_oid, &resource),
            link("synthetic-app", "missing", &task_oid, &resource),
            link("other-app", "ready", &task_oid, &resource),
        ],
    });

    let command = |action, tasks| {
        let mut command = Command::new(&hive);
        command
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &path)
            .env("TMPDIR", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_DATA_HOME", root.join("data"))
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
        command
    };
    let run = |command: &mut Command, expected| {
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    fs::write(&mapping_file, mapping.to_string()).unwrap();
    let plan = run(&mut command("migration-plan", true), 0);
    let plan_links = plan["data"]["issue_resource_bindings"].as_array().unwrap();
    assert_eq!(plan_links[0]["status"], "prepared");
    let task_key = sha256_hex(b"synthetic-app\0ready");
    assert_eq!(plan_links[0]["legacy_key_sha256"], task_key);
    let reasons = [
        "resource_unprepared",
        "resource_unprepared",
        "task_blob_mismatch",
        "task_unprepared",
        "task_unprepared",
        "resource_area_mismatch",
    ];
    assert_eq!(plan_links.len(), 1 + reasons.len());
    for (outcome, reason) in plan_links[1..].iter().zip(reasons) {
        assert_eq!(outcome["status"], "held");
        assert_eq!(outcome["reason_code"], reason);
    }
    assert_eq!(
        run(&mut command("migration-plan", false), 2)["error"]["code"],
        "INVALID_CONFIG"
    );
    if pin == POLICY_PIN {
        let valid = &mapping["issue_resource_bindings"][0];
        let mut bad_key = valid.clone();
        bad_key["legacy_key"] = json!(["synthetic-app", "ready"]);
        let mut wrong_oid = valid.clone();
        wrong_oid["source_git_blob_sha"] = json!("not-an-object-id");
        let mut extra = valid.clone();
        extra["issue_id"] = json!(id(99));
        for bindings in [
            Value::Null,
            json!([null]),
            json!({}),
            json!([[valid["legacy_key"], task_oid, resource]]),
            json!([bad_key]),
            json!([wrong_oid]),
            json!([extra]),
            json!([valid, valid]),
        ] {
            let mut invalid = mapping.clone();
            invalid["issue_resource_bindings"] = bindings;
            fs::write(&mapping_file, invalid.to_string()).unwrap();
            assert_eq!(
                run(&mut command("migration-plan", true), 2)["error"]["code"],
                "INVALID_CONFIG"
            );
        }
        let repeated = mapping.to_string().replace(
            "\"issue_resource_bindings\":[",
            "\"issue_resource_bindings\":[],\"issue_resource_bindings\":[",
        );
        fs::write(&mapping_file, repeated).unwrap();
        assert_eq!(
            run(&mut command("migration-plan", true), 2)["error"]["code"],
            "INVALID_CONFIG"
        );
        fs::write(&mapping_file, mapping.to_string()).unwrap();
    }

    let stage = |mode| {
        let data = run(
            command("migration-stage", true)
                .args([
                    "--mode",
                    mode,
                    "--policy-revision",
                    TARGET_POLICY,
                    "--target",
                ])
                .arg(&target),
            0,
        )["data"]
            .clone();
        assert_eq!(data["source"]["policy_pin"], pin);
        assert_eq!(data["source"]["source_commit"], commit);
        assert_eq!(data["checks"]["source_unchanged"], true);
        assert_eq!(
            data["counts"]["issue_resources"],
            json!({"inventoried":7, "imported":1, "held":6})
        );
        let links = data["issue_resource_bindings"].as_array().unwrap();
        assert_eq!(links.len(), 7);
        assert_eq!(links[0]["resource_id"], resource);
        assert_eq!(links[0]["legacy_key_sha256"], task_key);
        assert_eq!(links[0]["status"], "imported");
        assert_eq!(&links[1..], &plan_links[1..]);
        for private in [
            READY,
            OTHER,
            HELD,
            STALE,
            UNLINKED,
            REPO,
            OTHER_REPO,
            "Reviewed private Resource label",
            "Synthetic exact Issue label",
            root.to_str().unwrap(),
        ] {
            assert!(!data.to_string().contains(private), "{private}");
        }
        data
    };

    let dry = stage("dry-run");
    assert_eq!(dry["checks"]["transaction"], "rolled_back");
    assert_eq!(dry["checks"]["target_issue_resource_rows"], 0);
    assert!(dry["target"]["confirmed_dolt_commit"].is_null());
    assert!(!target.exists());

    let staged = stage("staged-import");
    assert_eq!(staged["checks"]["published_inactive"], true);
    assert_eq!(staged["checks"]["target_issue_resource_rows"], 1);
    let commit_db = staged["target"]["confirmed_dolt_commit"].as_str().unwrap();
    let baseline = staged["target"]["base_dolt_commit"].as_str().unwrap();
    let issue = staged["id_map"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["legacy_key_sha256"] == task_key)
        .unwrap();
    assert_eq!(issue["area_id"], area);
    assert_eq!(
        staged["issue_resource_bindings"][0]["issue_id"],
        issue["issue_id"]
    );
    let sql = |query: &str| {
        let result = Command::new(&dolt)
            .env_clear()
            .env("HOME", &home)
            .env("DOLT_ROOT_PATH", &home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true")
            .current_dir(target.join("db"))
            .args(["sql", "-r", "json", "-q", query])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["rows"].clone()
    };
    assert_eq!(
        sql(&format!(
            "SELECT issue_id, resource_id FROM issue_resources AS OF '{commit_db}'"
        )),
        json!([{"issue_id":issue["issue_id"], "resource_id":resource}])
    );
    assert_eq!(
        sql(&format!(
            "SELECT area_id, resource_id FROM area_resources AS OF '{commit_db}'"
        )),
        json!([{"area_id":area, "resource_id":resource}])
    );
    assert_eq!(
        sql(&format!(
            "SELECT COUNT(*) AS links FROM issue_resources AS OF '{baseline}'"
        )),
        json!([{"links":0}])
    );
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), source_index);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    for (path, original) in fixtures {
        assert_eq!(fs::read_to_string(source.join(path)).unwrap(), original);
    }
    assert!(
        git(
            &source,
            &home,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .is_empty()
    );
    assert!(!root.join("state").exists());
}

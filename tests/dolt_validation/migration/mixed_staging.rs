use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::{Command, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, sha256_hex};

#[test]
#[ignore = "requires installed Hive and pinned Dolt and Git"]
fn installed_cli_stages_mixed_reviewed_surfaces_in_one_inactive_target() {
    const POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
    const REPO_URL: &str = "https://forge.example/synthetic/mixed";
    const RECORD_REF: &str = "https://forge.example/synthetic/history";
    const TASK: &str = "tasks/synthetic-app/backlog/ready/README.md";
    const CLAIMED: &str = "tasks/synthetic-app/active/claimed/README.md";
    const HELD_WORK_ARTIFACT: &str = "tasks/synthetic-app/active/claimed/artifacts/report.md";
    const README: &str = "tasks/synthetic-app/README.md";
    const DIRECTIVE: &str = "directives/synthetic-app/proposal.md";
    const UNAPPROVED: &str = "directives/synthetic-app/unapproved.md";
    const ARTIFACT: &str = "artifacts/report.md";
    const UNCLASSIFIED: &str = "artifacts/unclassified.md";
    const SOUL: &str = "bots/synthetic-queen/SOUL.md";
    const INVALID_SOUL: &str = "bots/invalid-queen/SOUL.md";
    const MEMORY: &str = "bots/synthetic-queen/memory/synthetic-app/MEMORY.md";
    const SOUL_TEXT: &str = "---\r\ndescription: Synthetic Manager Soul\r\n---\r\n# Queen\r\n\r\n## Identity\r\n\r\n- ID: synthetic-queen\r\n- Name: Synthetic Queen\r\n- Layer: 1 — Manager\r\n\r\n## Purpose\r\n\r\nSynthetic Soul content sentinel.\r\n";
    const CREATED: &str = "2024-02-01T12:00:00Z";
    const UPDATED: &str = "2024-02-02T12:00:00Z";
    const RECORD_BODY: &str = "# Synthetic migrated content\r\n\r\nExact café bytes.  \r\n";
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let home = root.join("home");
    let source = root.join("v1");
    let target = root.join("inactive");
    let config = root.join("config");
    let mapping_file = root.join("reviewed.json");
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

    let task_body = format!("# Synthetic Task\n\nCreated: {CREATED}\nUpdated: {UPDATED}\n");
    let record = |fields| {
        format!("---\r\ndescription: Synthetic record\r\n{fields}\r\n---\r\n{RECORD_BODY}")
    };
    let reference_doc = format!("# Synthetic Project\n\n{REPO_URL}\n");
    let fixtures = [
        ("AGENTS.md", format!("hive-system@{POLICY_PIN}\n")),
        (
            "hive.yml",
            "version: 1\ncoordination:\n  mode: local\n".into(),
        ),
        (README, reference_doc.clone()),
        ("directives/synthetic-app/README.md", reference_doc.clone()),
        (MEMORY, reference_doc),
        ("tasks/_hive/README.md", "# Unsplit legacy scope\n".into()),
        (
            TASK,
            format!("---\ndescription: Synthetic Task record\npriority: high\n---\n{task_body}"),
        ),
        (
            CLAIMED,
            format!(
                "---\ndescription: Synthetic claim\nstatus: todo\nclaim: {{id: sample-claim, holder: sample-holder}}\n---\n{task_body}"
            ),
        ),
        (HELD_WORK_ARTIFACT, record("kind: report")),
        (SOUL, SOUL_TEXT.into()),
        (INVALID_SOUL, "# Missing Queen identity\n".into()),
        (DIRECTIVE, record("status: proposed")),
        (UNAPPROVED, record("status: active")),
        (ARTIFACT, record("kind: report")),
        (UNCLASSIFIED, record("status: parked")),
    ];
    for (path, contents) in &fixtures {
        let file = source.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, contents).unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic mixed migration"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let index = fs::read(source.join(".git/index")).unwrap();
    let oid = |path: &str| git_text(&source, &home, &["rev-parse", &format!("{commit}:{path}")]);
    let range = |value: &str| {
        let start = task_body.find(value).unwrap();
        json!({"start":start, "end":start + value.len()})
    };
    let workspace = id(1);
    let area = id(2);
    let resource = id(3);
    let queen = id(4);
    let directive = id(5);
    let artifact = id(6);
    let record_review = |path: &str, id: &str, provenance| json!({"source_path":path, "source_git_blob_sha":oid(path), "target_id":id, "provenance":provenance});
    let project_evidence = [README, "directives/synthetic-app/README.md", MEMORY]
        .map(|path| json!({"path":path, "canonical_ref": REPO_URL}));
    let mapping = json!({
        "version": 1, "source_commit": commit, "policy_pin": POLICY_PIN, "workspace_id": workspace,
        "scopes": [{"scope":"synthetic-app","decision":{"kind":"area",
            "area":{"id":area,"slug":"synthetic-app","name":"Synthetic Area"},
            "project_evidence":project_evidence
        }}],
        "task_reviews":[{"legacy_key":{"scope":"synthetic-app","work_id":"ready"},"source_git_blob_sha":oid(TASK),
            "area_id":area, "created_at":range(CREATED),"updated_at":range(UPDATED),"relations":"no_edges"}],
        "resource_bindings":{"version":1, "bindings":[
            {"scope":"synthetic-app","area_id":area,"resource":{"id":resource,"kind":"git-repo","name":"Synthetic Repo"},
             "evidence_path":README,"evidence_blob_oid":oid(README)},
            {"scope":"synthetic-app","area_id":area,"resource":{"id":id(7),"kind":"git-repo","name":"Stale Repo"},
             "evidence_path":README,"evidence_blob_oid":"0".repeat(commit.len())}
        ]},
        "queen_assets":[
            {"slug":"synthetic-queen","source_git_blob_sha":oid(SOUL)},
            {"slug":"invalid-queen","source_git_blob_sha":oid(INVALID_SOUL)}
        ],
        "queen_preparation":[{"slug":"synthetic-queen","source_git_blob_sha":oid(SOUL),
            "source_raw_sha256":sha256_hex(SOUL_TEXT.as_bytes()),"bot_id":queen,"display_name":"Reviewed Synthetic Queen"}],
        "reviewed_records":[
            record_review(DIRECTIVE, &directive, json!({"kind":"proposed_directive","area_id":area,"issuer_ref":"human:synthetic-operator",
                "source_ref":RECORD_REF,"created_at":CREATED,"updated_at":UPDATED,"supersession":"none","approval_and_validity":"none"})),
            record_review(ARTIFACT, &artifact, json!({"kind":"standalone_artifact","artifact_kind":"report","author_ref":"bot:synthetic-author",
                "source_ref":RECORD_REF,"created_at":CREATED,"substantive_updated_at":UPDATED,"links":"none","archive":"none"}))
        ]
    });
    let mapping_bytes = serde_json::to_vec(&mapping).unwrap();
    fs::write(&mapping_file, &mapping_bytes).unwrap();
    fs::create_dir_all(config.join("hive/v2")).unwrap();
    let v1_config = format!("workspace: {}\n", source.display());
    let v2_config = format!("version: 2\nworkspace: {}\n", target.display());
    fs::write(config.join("hive/config.yml"), &v1_config).unwrap();
    fs::write(config.join("hive/v2/config.yml"), &v2_config).unwrap();
    let cli = |contract| {
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
            .args(["--contract", contract, "--json"]);
        command
    };
    let migration = |action| {
        let mut command = cli("2");
        command
            .args(["workspace", action, "--tasks", "--source"])
            .arg(&source)
            .args([
                "--source-commit",
                &commit,
                "--workspace-id",
                &workspace,
                "--mapping-file",
            ])
            .arg(&mapping_file);
        command
    };
    let stage = |mode| {
        let mut command = migration("migration-stage");
        command
            .args(["--mode", mode, "--policy-revision", POLICY, "--target"])
            .arg(&target);
        command
    };
    let response = |command: &mut Command, status| {
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let read_v1 = || response(cli("1").args(["config", "show", "--scope", "workspace"]), 0);
    let v1 = read_v1();
    let plan = response(
        migration("migration-plan").arg("--include-directives-artifacts"),
        0,
    );
    for key in [
        "task_intake",
        "queen_preparation",
        "resource_bindings",
        "directive_artifact_intake",
    ] {
        assert!(plan["data"].get(key).is_some(), "{key}");
    }

    let dry = response(&mut stage("dry-run"), 0)["data"].clone();
    assert_eq!(dry["coverage"], "mixed");
    assert_eq!(dry["checks"]["transaction"], "rolled_back");
    for surface in [
        "area",
        "issue",
        "bot",
        "resource",
        "area_resource",
        "directive",
        "artifact",
    ] {
        assert_eq!(dry["checks"][format!("target_{surface}_rows")], 0);
    }
    assert_eq!(dry["target"]["issue_number_next_value"], 1);
    assert!(dry["target"]["confirmed_dolt_commit"].is_null());
    assert!(!target.exists());

    // Inject a source edit while verifying the private committed Dolt snapshot.
    let drift = source.join("synthetic-drift.md");
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let bin = root.join("fixture-bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("dolt");
    fs::write(&wrapper, format!(
        "#!/bin/sh\nif [ \"$1\" = fsck ]; then printf '%s\\n' synthetic > {}; fi\nexec {} \"$@\"\n",
        quote(&drift), quote(Path::new(&dolt)),
    )).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let injected =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).unwrap();
    let refused = response(stage("staged-import").env("PATH", injected), 5);
    assert_eq!(refused["error"]["code"], "DIRTY_CONFLICT");
    assert!(!target.exists());
    fs::remove_file(drift).unwrap();

    let staged = response(&mut stage("staged-import"), 0)["data"].clone();
    for key in [
        "counts",
        "holds",
        "exclusions",
        "queen_inventory",
        "queen_preparation",
        "resource_bindings",
    ] {
        assert_eq!(staged[key], dry[key], "{key}");
    }
    assert_eq!(staged["checks"]["committed_record_validation"], "passed");
    assert_eq!(staged["checks"]["published_inactive"], true);
    assert_eq!(staged["mapping_sha256"], sha256_hex(&mapping_bytes));
    assert_eq!(staged["source"]["source_commit"], commit);
    for (key, held) in [
        ("scopes", 1),
        ("tasks", 1),
        ("resources", 1),
        ("directives", 1),
        ("artifacts_standalone", 1),
    ] {
        assert_eq!(staged["counts"][key]["imported"], 1);
        assert_eq!(staged["counts"][key]["held"], held);
    }
    assert_eq!(staged["counts"]["artifacts_work_scoped"]["held"], 1);
    let holds = staged["holds"].as_array().unwrap();
    for (path, reason) in [
        (CLAIMED, "execution_needs_reconciliation"),
        (UNAPPROVED, "missing_directive_approval"),
        (UNCLASSIFIED, "unclassified_artifact_kind"),
        (HELD_WORK_ARTIFACT, "unresolved_parent_task"),
    ] {
        let locator = sha256_hex(path.as_bytes());
        assert!(holds.iter().any(|row| row["source_path_sha256"] == locator && row.to_string().contains(reason)), "{path}: {holds:?}");
    }
    assert_eq!(
        staged["resource_bindings"]["outcomes"][1]["reason_code"],
        "unverified_resource_evidence"
    );
    assert!(
        staged["queen_inventory"]["holds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["reason_code"] == "curated_memory_deferred"
                && row["source_path_sha256"] == sha256_hex(MEMORY.as_bytes()))
    );
    assert_eq!(staged["queen_inventory"]["soul_held"], 1);
    let report = staged.to_string();
    for redacted in [
        TASK,
        CLAIMED,
        SOUL,
        MEMORY,
        DIRECTIVE,
        ARTIFACT,
        REPO_URL,
        RECORD_REF,
        "Exact café bytes",
        "Synthetic Soul content sentinel",
        "sample-holder",
        root.to_str().unwrap(),
    ] {
        assert!(!report.contains(redacted), "{redacted}");
    }

    let db = staged["target"]["confirmed_dolt_commit"].as_str().unwrap();
    let assets = staged["target"]["asset_git_commit"].as_str().unwrap();
    let sql = |query: &str| {
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
    let task_path_sha256 = sha256_hex(TASK.as_bytes());
    let issue = staged["id_map"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "task" && row["source_path_sha256"] == task_path_sha256)
        .unwrap();
    assert_eq!(
        sql(&format!(
            "SELECT id, number, area_id, status, priority, body FROM issues AS OF '{db}'"
        )),
        json!([{"id":issue["issue_id"], "number":1, "area_id":area, "status":"backlog", "priority":0, "body":task_body}])
    );
    assert_eq!(
        sql(&format!(
            "SELECT resource_id, area_id FROM area_resources AS OF '{db}'"
        )),
        json!([{"resource_id":resource,"area_id":area}])
    );
    assert_eq!(
        sql(&format!("SELECT id, uri FROM resources AS OF '{db}'")),
        json!([{"id":resource,"uri":REPO_URL}])
    );
    assert_eq!(
        sql(&format!("SELECT id, slug FROM bots AS OF '{db}'")),
        json!([{"id":queen,"slug":"synthetic-queen"}])
    );
    assert_eq!(
        sql(&format!("SELECT id, body FROM directives AS OF '{db}'")),
        json!([{"id":directive,"body":RECORD_BODY}])
    );
    assert_eq!(
        sql(&format!("SELECT id, body FROM artifacts AS OF '{db}'")),
        json!([{"id":artifact,"body":RECORD_BODY}])
    );
    assert_eq!(
        sql(&format!(
            "SELECT (SELECT COUNT(*) FROM issue_claims AS OF '{db}') AS claims, (SELECT COUNT(*) FROM bot_memories AS OF '{db}') AS memories, (SELECT next_value FROM workspace_counters AS OF '{db}' WHERE name='issue_number') AS next_number"
        )),
        json!([{"claims":0,"memories":0,"next_number":2}])
    );
    assert_eq!(
        git_text(&target.join("assets"), &home, &["rev-parse", "HEAD"]),
        assets
    );
    assert_eq!(
        git(
            &target.join("assets"),
            &home,
            &["show", &format!("{assets}:bots/synthetic-queen/SOUL.md")]
        ),
        SOUL_TEXT.as_bytes()
    );
    assert_eq!(
        sql("SELECT DOLT_HASHOF('HEAD') AS head"),
        json!([{"head":db}])
    );
    assert_eq!(
        response(&mut stage("staged-import"), 5)["error"]["code"],
        "ALREADY_EXISTS"
    );
    assert_eq!(
        sql("SELECT DOLT_HASHOF('HEAD') AS head"),
        json!([{"head":db}])
    );

    assert_eq!(read_v1(), v1);
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(
        git(
            &source,
            &home,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .is_empty()
    );
    for (path, contents) in fixtures {
        assert_eq!(fs::read_to_string(source.join(path)).unwrap(), contents);
    }
    assert_eq!(
        fs::read_to_string(config.join("hive/config.yml")).unwrap(),
        v1_config
    );
    assert_eq!(
        fs::read_to_string(config.join("hive/v2/config.yml")).unwrap(),
        v2_config
    );
    assert!(!root.join("data").exists());
    assert!(!root.join("state").exists());
    assert!(!fs::read_dir(root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".hive-init-incomplete-")
    }));
}

use std::path::Path;

use super::{
    AREA_GENERAL, Command, EARLIER_POLICY_PIN, POLICY_PIN, Value, fs, git, git_text, id, init_git,
    json, sha256_hex,
};

#[test]
#[ignore = "requires installed Hive, pinned Dolt 2.7.6, and native Git"]
fn stages_explicit_directive_scopes_in_one_inactive_target() {
    const BODY: &str = "# Synthetic directive\r\n\r\nPrivate fixture body.\r\n";
    const REFERENCE: &str = "https://forge.example/synthetic/review";
    const APPROVAL: &str = "human:synthetic-reviewer";
    const UPDATED: &str = "2024-02-02T12:00:00.123456Z";
    const V2_POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let home = root.path().join("home");
    let config = root.path().join("config");
    let target = root.path().join("inactive");
    let manifest = root.path().join("mapping.json");
    let hive = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").expect("installed Hive");
    let dolt = std::env::var_os("HIVE_DOLT_BIN").expect("pinned Dolt");
    let search_path = std::env::join_paths(
        std::iter::once(Path::new(&dolt).parent().unwrap().to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    fs::create_dir(&home).unwrap();
    init_git(
        &source,
        &home,
        "Synthetic operator",
        "fixture@example.invalid",
    );
    fs::write(
        source.join("AGENTS.md"),
        format!("hive-system@{POLICY_PIN}\n"),
    )
    .unwrap();
    fs::write(
        source.join("hive.yml"),
        "version: 1\ncoordination:\n  mode: local\n",
    )
    .unwrap();

    let names = [
        "older",
        "replacement",
        "default",
        "project",
        "absent",
        "held",
        "wrong-area",
        "unapproved",
    ];
    let statuses = [
        "superseded",
        "active",
        "proposed",
        "proposed",
        "proposed",
        "proposed",
        "proposed",
        "active",
    ];
    let path = |name| format!("directives/_general/{name}.md");
    fs::create_dir_all(source.join("directives/_general")).unwrap();
    for (name, status) in names.into_iter().zip(statuses) {
        fs::write(
            source.join(path(name)),
            format!("---\r\ndescription: Synthetic fixture\r\nstatus: {status}\r\n---\r\n{BODY}"),
        )
        .unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic Directive scopes"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let index = fs::read(source.join(".git/index")).unwrap();

    let workspace = id(1);
    let area = id(AREA_GENERAL);
    let project = id(0x720);
    let missing_project = id(0x721);
    let held_project = id(0x722);
    let project_scope = |project: &str| json!({"kind": "project", "project_id": project});
    let baseline = |kind| {
        json!({
            "kind": kind, "area_id": area, "issuer_ref": "human:synthetic-issuer", "source_ref": REFERENCE,
            "created_at": "2024-02-01T12:00:00Z", "updated_at": UPDATED,
            "supersession": "none", "approval_and_validity": "none",
        })
    };
    let history = |status, approval, supersession| {
        let mut row = baseline("directive_history");
        row["status"] = json!(status);
        row["approval_and_validity"] = approval;
        row["supersession"] = supersession;
        row["scope"] = json!({"kind": "workspace"});
        row
    };
    let proposal = |scope| {
        let mut row = baseline("proposed_directive");
        row["scope"] = scope;
        row
    };
    let mut wrong_area = proposal(json!({"kind": "workspace"}));
    wrong_area["area_id"] = json!(id(0x723));
    let mut default = history(
        "proposed",
        json!({"state": "reviewed"}),
        json!({"state": "reviewed"}),
    );
    default.as_object_mut().unwrap().remove("scope");
    let provenance = [
        history(
            "superseded",
            json!({"state": "reviewed"}),
            json!({"state":"reviewed", "replacement":"directives/_general/replacement"}),
        ),
        history(
            "active",
            json!({"state":"reviewed", "approval_ref":APPROVAL, "valid_from":UPDATED,
            "expires_at":"2024-02-04T12:00:00Z"}),
            json!({"state":"reviewed"}),
        ),
        default,
        proposal(project_scope(&project)),
        proposal(project_scope(&missing_project)),
        proposal(project_scope(&held_project)),
        wrong_area,
        history(
            "active",
            json!({"state":"reviewed"}),
            json!({"state":"reviewed"}),
        ),
    ];
    let reviews: Vec<_> = names.into_iter().zip(provenance).enumerate().map(|(i, (name, review))| {
        let path = path(name);
        let blob = git_text(&source, &home, &["rev-parse", &format!("HEAD:{path}")]);
        json!({"source_path":path, "source_git_blob_sha":blob, "target_id":id(0x740 + i as u32), "provenance":review})
    }).collect();
    let mapping = json!({
        "version":1, "source_commit":commit, "policy_pin":POLICY_PIN, "workspace_id":workspace,
        "directive_observed_at":"2024-02-03T12:00:00Z", "reviewed_records":reviews,
        "project_membership":{"projects":[
            {"id":project, "name":"Synthetic Project", "status":"active"},
            {"id":held_project, "name":"Repeated Project", "status":"active"},
            {"id":held_project, "name":"Repeated Project", "status":"active"},
        ], "memberships":[]},
        "scopes":[{"scope":"_general", "decision":{"kind":"area","area":{"id":area,"slug":"general","name":"General"}}}]
    });
    let migration = |action: &str, mapping: &Value| {
        fs::write(&manifest, serde_json::to_vec(mapping).unwrap()).unwrap();
        let mut cmd = Command::new(&hive);
        cmd.current_dir(&home)
            .env_clear()
            .env("PATH", &search_path)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config)
            .args(["--contract", "2", "--json", "workspace", action, "--source"])
            .arg(&source)
            .args([
                "--source-commit",
                mapping["source_commit"].as_str().unwrap(),
                "--workspace-id",
                &workspace,
                "--mapping-file",
            ])
            .arg(&manifest);
        cmd
    };
    let response = |command: &mut Command, status| {
        let output = command.output().unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.status.code(),
            Some(status),
            "{text} {:?}",
            output.stderr
        );
        assert!(output.stderr.is_empty());
        for raw in [
            REFERENCE,
            APPROVAL,
            "Private fixture body",
            root.path().to_str().unwrap(),
        ] {
            assert!(!text.contains(raw), "{raw}");
        }
        serde_json::from_str::<Value>(&text).unwrap()
    };
    let plan = |mapping: &Value, status| {
        response(
            migration("migration-plan", mapping).arg("--include-directives-artifacts"),
            status,
        )
    };
    let stage_command = |mode: &str, mapping: &Value, target: &Path| {
        let mut cmd = migration("migration-stage", mapping);
        cmd.arg("--target")
            .arg(target)
            .args(["--policy-revision", V2_POLICY, "--mode", mode]);
        cmd
    };
    let outcome = |records: &Value, locator: &str, key: &str| {
        records
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[key] == locator)
            .unwrap()["outcome"]
            .clone()
    };
    let planned = plan(&mapping, 0);
    let records = |report: &Value| report["data"]["directive_artifact_intake"]["records"].clone();
    let planned_scope =
        |name| outcome(&records(&planned), &path(name), "source_path")["candidate"].clone();
    for (name, kind) in [
        ("replacement", "workspace"),
        ("default", "area"),
        ("project", "project"),
    ] {
        let candidate = planned_scope(name);
        assert_eq!(candidate["scope_kind"], kind);
        assert_eq!(candidate["include_descendants"], false);
    }
    assert!(planned_scope("replacement")["area_id"].is_null());
    assert_eq!(planned_scope("default")["area_id"], area);
    assert_eq!(planned_scope("project")["project_id"], project);
    for (name, reason) in [
        ("absent", "unresolved_directive_project"),
        ("held", "unresolved_directive_project"),
        ("wrong-area", "ambiguous_scope_or_collision"),
        ("unapproved", "missing_directive_approval"),
    ] {
        let held = outcome(&records(&planned), &path(name), "source_path");
        assert_eq!(held["status"], "held");
        assert_eq!(held["reason_codes"], json!([reason]));
    }
    let mut bad = mapping.clone();
    for scope in [
        Value::Null,
        json!(["project", project]),
        json!({"kind":"project"}),
        json!({"kind":"project","project_id":"malformed"}),
        json!({"kind":"workspace","project_id":project}),
        json!({"kind":"area","include_descendants":true}),
    ] {
        bad["reviewed_records"][3]["provenance"]["scope"] = scope;
        assert_eq!(plan(&bad, 2)["error"]["code"], "INVALID_CONFIG");
    }
    let mut wrong_workspace = mapping.clone();
    wrong_workspace["workspace_id"] = json!(id(0x724));
    assert_eq!(plan(&wrong_workspace, 2)["error"]["code"], "INVALID_CONFIG");

    let mut chain = mapping.clone();
    chain["reviewed_records"][0]["provenance"]["scope"] = project_scope(&project);
    let cross_scope = records(&plan(&chain, 0));
    for name in ["older", "replacement"] {
        let held = outcome(&cross_scope, &path(name), "source_path");
        assert_eq!(held["status"], "held");
        assert_eq!(held["reason_codes"], json!(["invalid_supersession_chain"]));
    }
    chain["reviewed_records"][1]["provenance"]["scope"] = project_scope(&project);
    let same_scope = records(&plan(&chain, 0));
    assert_eq!(
        outcome(&same_scope, &path("replacement"), "source_path")["provenance"]["supersedes_id"],
        id(0x740)
    );
    chain["reviewed_records"][0]["provenance"]["scope"] = project_scope(&held_project);
    let held_endpoint = records(&plan(&chain, 0));
    assert_eq!(
        outcome(&held_endpoint, &path("replacement"), "source_path")["reason_codes"],
        json!(["invalid_supersession_chain"])
    );
    assert!(!target.exists());

    let dry = response(&mut stage_command("dry-run", &mapping, &target), 0);
    assert_eq!(dry["data"]["checks"]["transaction"], "rolled_back");
    assert_eq!(dry["data"]["checks"]["target_directive_rows"], 0);
    assert!(!target.exists());
    let applied = response(&mut stage_command("staged-import", &mapping, &target), 0);
    let data = &applied["data"];
    assert_eq!(data["checks"]["committed_record_validation"], "passed");
    assert_eq!(
        data["counts"]["directives"],
        json!({"inventoried":8,"imported":4,"held":4,"excluded_non_record":0})
    );
    for name in ["replacement", "default", "project"] {
        let locator = sha256_hex(path(name).as_bytes());
        assert_eq!(
            outcome(&data["id_map"], &locator, "source_path_sha256")["candidate"],
            planned_scope(name)
        );
    }
    let confirmed = data["target"]["confirmed_dolt_commit"].as_str().unwrap();
    let query = Command::new(&dolt)
        .current_dir(target.join("db")).env_clear().env("HOME", &home)
        .env("DOLT_DISABLE_EVENT_FLUSH", "true").env("DOLT_DISABLE_VERSION_CHECKS", "true")
        .args(["sql", "-r", "json", "-q", &format!(
            "SELECT id, status, scope_kind, area_id, project_id, IF(include_descendants, 1, 0) AS descendants,
              supersedes_id, approval_ref, DATE_FORMAT(valid_from, '%Y-%m-%dT%H:%i:%s.%fZ') AS validity
             FROM directives AS OF '{confirmed}' ORDER BY id"
        )]).output().unwrap();
    assert!(query.status.success(), "{query:?}");
    let rows = serde_json::from_slice::<Value>(&query.stdout).unwrap()["rows"].clone();
    assert_eq!(
        rows,
        json!([
            {"id":id(0x740), "status":"superseded", "scope_kind":"workspace", "descendants":0},
            {"id":id(0x741), "status":"active", "scope_kind":"workspace", "descendants":0,
                "supersedes_id":id(0x740), "approval_ref":APPROVAL, "validity":UPDATED},
            {"id":id(0x742), "status":"proposed", "scope_kind":"area", "area_id":area, "descendants":0},
            {"id":id(0x743), "status":"proposed", "scope_kind":"project", "project_id":project, "descendants":0},
        ])
    );
    assert!(!config.exists());
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());

    let invalid_target = root.path().join("invalid-target");
    for pin in [POLICY_PIN, EARLIER_POLICY_PIN] {
        let mut current = mapping.clone();
        if pin == EARLIER_POLICY_PIN {
            fs::write(source.join("AGENTS.md"), format!("hive-system@{pin}\n")).unwrap();
            fs::remove_file(source.join("hive.yml")).unwrap();
            git(&source, &home, &["add", "."]);
            git(
                &source,
                &home,
                &["commit", "-qm", "Earlier synthetic policy pin"],
            );
            current["policy_pin"] = json!(pin);
            current["source_commit"] = json!(git_text(&source, &home, &["rev-parse", "HEAD"]));
        }
        let source_index = fs::read(source.join(".git/index")).unwrap();
        let valid = records(&plan(&current, 0));
        assert_eq!(
            outcome(&valid, &path("default"), "source_path")["status"],
            "prepared"
        );
        for (field, malformed) in [
            ("status", json!({"proposed": null})),
            (
                "approval_and_validity",
                json!({"state": "unresolved", "approval_ref": APPROVAL}),
            ),
            (
                "supersession",
                json!({"state": "unresolved", "replacement": "directives/_general/replacement"}),
            ),
        ] {
            let mut invalid = current.clone();
            invalid["reviewed_records"][2]["provenance"][field] = malformed;
            assert_eq!(
                plan(&invalid, 2)["error"]["code"],
                "INVALID_CONFIG",
                "{pin}: {field}"
            );
            let staged = response(
                &mut stage_command("staged-import", &invalid, &invalid_target),
                2,
            );
            assert_eq!(staged["error"]["code"], "INVALID_CONFIG", "{pin}: {field}");
            assert!(!invalid_target.exists());
        }
        for (field, reason) in [
            ("approval_and_validity", "unverified_directive_validity"),
            ("supersession", "invalid_supersession_chain"),
        ] {
            let mut undecided = current.clone();
            undecided["reviewed_records"][2]["provenance"][field] = json!({"state": "unresolved"});
            let held = records(&plan(&undecided, 0));
            let held = outcome(&held, &path("default"), "source_path");
            assert_eq!(held["status"], "held");
            assert_eq!(held["reason_codes"], json!([reason]), "{pin}: {field}");
        }
        assert_eq!(fs::read(source.join(".git/index")).unwrap(), source_index);
        assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    }
}

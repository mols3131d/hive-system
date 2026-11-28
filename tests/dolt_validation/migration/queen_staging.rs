use super::{
    AREA_APP, AREA_GENERAL, AREA_USERDATA, Command, POLICY_PIN, Path, Value, fs, git, git_text, id,
    init_source, json, sha256_hex,
};

#[test]
#[ignore = "requires HIVE_STAGING_INSTALLED_BIN, pinned Dolt at HIVE_DOLT_BIN, and native Git"]
fn installed_cli_stages_scopes_without_queen_reviews() {
    check_installed_staging(false);
}

#[test]
#[ignore = "requires HIVE_STAGING_INSTALLED_BIN, pinned Dolt at HIVE_DOLT_BIN, and native Git"]
fn installed_cli_stages_reviewed_queen_souls_and_scopes() {
    check_installed_staging(true);
}

fn check_installed_staging(include_queens: bool) {
    const V2_POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
    const REFERENCE: &str = "https://forge.example/synthetic/application";
    const QUEEN_NAME: &str = "Reviewed Queen name sentinel '); DROP TABLE bots; -- 🐝";
    const SOUL: &str = "---\r\ndescription: \"Original frontmatter sentinel.\"\r\n---\r\n# Queen\r\n\r\n## Identity\r\n\r\n- ID: queen-synth\r\n- Name: Source Queen name sentinel\r\n- Layer: 1 — Manager\r\n\r\n## Purpose\r\n\r\nOriginal café Soul sentinel.\r\n";
    const NAME: &str = "Synthetic café '); DROP TABLE areas; -- 🐝";
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let config = root.join("config");
    let state = root.join("state");
    let data = root.join("data");
    let source = root.join("source");
    let target = root.join("inactive workspace");
    let mapping_file = root.join("reviewed.json");
    let workspace_id = id(1);
    let git_at_source = |args: &[&str]| git_text(&source, &home, args);
    init_source(&source, &home);
    let app_sources = [
        "tasks/synth-app/README.md",
        "directives/synth-app/README.md",
        "bots/queen-synth/memory/synth-app/MEMORY.md",
    ];
    for path in app_sources {
        fs::write(source.join(path), format!("# Application\n\n{REFERENCE}\n")).unwrap();
    }
    let conflicting_soul =
        SOUL.replace("queen-synth", "conflict-queen") + "\r\n- **Role:** Coordinator\r\n";
    if include_queens {
        fs::write(source.join("bots/queen-synth/SOUL.md"), SOUL).unwrap();
        fs::create_dir(source.join("bots/conflict-queen")).unwrap();
        fs::write(
            source.join("bots/conflict-queen/SOUL.md"),
            &conflicting_soul,
        )
        .unwrap();
        fs::write(
            source.join("bots/queen-synth/AGENTS.md"),
            "Agent harness sentinel.",
        )
        .unwrap();
        fs::write(
            source.join("bots/queen-synth/README.md"),
            "Routing sentinel.",
        )
        .unwrap();
        fs::write(source.join("bots/queen-synth/MEMORY.md"),
            "---\ndescription: \"Memory navigation.\"\n---\n# Memory\n\n- [synth-app](memory/synth-app/MEMORY.md)\n",
        ).unwrap();
    }
    git_at_source(&["add", "."]);
    git_at_source(&["commit", "-qm", "Review synthetic scope identity"]);
    let commit = git_at_source(&["rev-parse", "HEAD"]);
    let verify_source = || {
        assert_eq!(git_at_source(&["rev-parse", "HEAD"]), commit);
        assert!(git_at_source(&["status", "--porcelain", "--untracked-files=all"]).is_empty());
    };
    let sources = [
        "AGENTS.md",
        "hive.yml",
        ".git/index",
        ".git/config",
        ".git/HEAD",
    ];
    let source_bytes = sources.map(|name| fs::read(source.join(name)).unwrap());

    fs::create_dir_all(config.join("hive/v2")).unwrap();
    let v1_config = config.join("hive/config.yml");
    let v2_config = config.join("hive/v2/config.yml");
    let v1_config_bytes = format!("workspace: {}\n", source.display());
    let v2_config_bytes = format!("version: 2\nworkspace: {}\n", target.display());
    fs::write(&v1_config, &v1_config_bytes).unwrap();
    fs::write(&v2_config, &v2_config_bytes).unwrap();
    let installed = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").unwrap();
    let cli = |contract: &str| {
        let mut command = Command::new(&installed);
        command
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_DATA_HOME", &data)
            .env("TMPDIR", root)
            .args(["--contract", contract, "--json"]);
        command
    };
    let response = |command: &mut Command, status| {
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let v1 = || response(cli("1").args(["config", "show", "--scope", "workspace"]), 0);
    let before = v1();
    assert_eq!(before["data"]["policy_revision"], POLICY_PIN);

    let general = id(AREA_GENERAL);
    let userdata = id(AREA_USERDATA);
    let app = id(AREA_APP);
    let mut mapping = json!({
        "version": 1, "source_commit": commit, "policy_pin": POLICY_PIN, "workspace_id": workspace_id,
        "scopes": [
            {"scope": "_hive-userdata", "decision": {"kind": "area",
             "area": {"id": userdata, "parent_id": app, "slug": "workspace-operations", "name": "Operations"}}},
            {"scope": "synth-app", "decision": {"kind": "area",
             "area": {"id": app, "parent_id": general, "slug": "application", "name": NAME},
             "project_evidence": app_sources.map(|path| json!({"path": path, "canonical_ref": REFERENCE}))}},
            {"scope": "_general", "decision": {"kind": "area",
             "area": {"id": general, "slug": "general", "name": "General"}}},
            {"scope": "external-team", "decision": {"kind": "external"}},
        ],
    });
    let bot_id = id(0x150);
    if include_queens {
        let oid = |slug: &str| git_at_source(&["rev-parse", &format!("HEAD:bots/{slug}/SOUL.md")]);
        mapping["queen_assets"] = json!([
            {"slug": "queen-synth", "source_git_blob_sha": oid("queen-synth")},
            {"slug": "conflict-queen", "source_git_blob_sha": oid("conflict-queen")},
        ]);
        mapping["queen_preparation"] = json!([
            {
                "slug": "queen-synth", "bot_id": bot_id, "display_name": QUEEN_NAME,
                "source_git_blob_sha": oid("queen-synth"),
                "source_raw_sha256": sha256_hex(SOUL.as_bytes()),
            },
            {
                "slug": "conflict-queen", "bot_id": id(0x151), "display_name": "Conflicting name sentinel",
                "source_git_blob_sha": oid("conflict-queen"),
                "source_raw_sha256": sha256_hex(conflicting_soul.as_bytes()),
            },
        ]);
    }
    let stage_command = |mode: &str, policy: &str| {
        let mut command = cli("2");
        command
            .args([
                "workspace",
                "migration-stage",
                "--mode",
                mode,
                "--policy-revision",
                policy,
                "--source",
            ])
            .arg(&source)
            .args([
                "--source-commit",
                &commit,
                "--workspace-id",
                &workspace_id,
                "--mapping-file",
            ])
            .arg(&mapping_file)
            .arg("--target")
            .arg(&target);
        command
    };
    let stage = |mode: &str, policy: &str, input: &Value, status| {
        fs::write(&mapping_file, serde_json::to_vec(input).unwrap()).unwrap();
        let report = response(&mut stage_command(mode, policy), status);
        assert_eq!(report["command"], "workspace migration-stage");
        assert_eq!(report["ok"], status == 0);
        for private in [
            root.to_str().unwrap(),
            NAME,
            REFERENCE,
            "synth-app",
            "workspace-operations",
            "external-team",
            "Synthetic persona bytes.",
            "sentinel",
        ] {
            assert!(!report.to_string().contains(private));
        }
        report
    };
    let reject = |policy, input: &Value, status, code| {
        let error = stage("staged-import", policy, input, status)["error"].clone();
        assert_eq!(error["code"], code);
        error
    };
    reject(POLICY_PIN, &mapping, 4, "INCOMPATIBLE_POLICY");
    for (field, wrong) in [
        ("workspace_id", id(2)),
        ("policy_pin", V2_POLICY.to_owned()),
    ] {
        let mut invalid = mapping.clone();
        invalid[field] = json!(wrong);
        reject(V2_POLICY, &invalid, 2, "INVALID_CONFIG");
    }
    let mut missing = mapping.clone();
    missing["scopes"][1]["decision"]["area"]["parent_id"] = json!(id(999));
    assert!(
        reject(V2_POLICY, &missing, 4, "INVALID_RECORD")["message"]
            .as_str()
            .unwrap()
            .contains("missing or held")
    );
    let mut held_parent = mapping.clone();
    held_parent["scopes"][1]["decision"]["project_evidence"] = json!([]);
    reject(V2_POLICY, &held_parent, 4, "INVALID_RECORD");
    let mut cyclic = mapping.clone();
    cyclic["scopes"][2]["decision"]["area"]["parent_id"] = json!(userdata);
    assert!(
        reject(V2_POLICY, &cyclic, 4, "INVALID_RECORD")["message"]
            .as_str()
            .unwrap()
            .contains("cycle")
    );
    assert!(!target.exists());

    let dry_run = stage("dry-run", V2_POLICY, &mapping, 0)["data"].clone();
    assert_eq!(dry_run["mode"], "dry_run");
    assert_eq!(
        dry_run["coverage"],
        if include_queens {
            "scopes_and_queen_souls"
        } else {
            "scope_only"
        }
    );
    assert_eq!(
        dry_run["counts"]["scopes"],
        json!({"inventoried": 5, "imported": 3, "held": 1, "excluded_non_record": 1})
    );
    assert_eq!(dry_run["holds"][0]["reason_code"], "unsplit_legacy_scope");
    assert_eq!(dry_run["holds"][0]["scope_sha256"], sha256_hex(b"_hive"));
    assert_eq!(
        dry_run["exclusions"][0]["reason_code"],
        "external_backend_owned"
    );
    assert_eq!(dry_run["target"]["confirmed_dolt_commit"], Value::Null);
    let mut expected_checks = json!({"transaction": "rolled_back", "target_area_rows": 0,
        "source_unchanged": true, "area_ancestry": "verified", "committed_record_validation": "not_run",
        "asset_commit_verified": true, "published_inactive": false});
    if include_queens {
        expected_checks["target_bot_rows"] = json!(0);
        assert_eq!(dry_run["counts"]["prepared_queens"], 1);
        assert_eq!(dry_run["queen_preparation"][0]["bot_id"], bot_id);
        assert_eq!(
            dry_run["queen_preparation"][0]["source_raw_sha256"],
            sha256_hex(SOUL.as_bytes())
        );
        assert_eq!(
            dry_run["queen_preparation"][1]["reason"],
            "invalid_soul_or_slug"
        );
        assert_eq!(dry_run["queen_inventory"]["soul_candidates"], 1);
        assert!(
            dry_run["queen_inventory"]["curated_memory_deferred"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(dry_run["queen_inventory"]["navigation_only"], 1);
    }
    assert_eq!(dry_run["checks"], expected_checks);
    assert!(!target.exists());
    assert!(!state.exists());
    assert!(!data.exists());
    verify_source();

    fs::create_dir(&target).unwrap();
    reject(V2_POLICY, &mapping, 5, "ALREADY_EXISTS");
    assert!(fs::read_dir(&target).unwrap().next().is_none());
    fs::remove_dir(&target).unwrap();
    let registration_dir = state.join("hive/v2/workspaces").join(&workspace_id);
    fs::create_dir_all(&registration_dir).unwrap();
    let registration = registration_dir.join("active-root.json");
    let registration_bytes = serde_json::to_vec(&json!({
        "version": 2, "workspace_id": workspace_id, "workspace_root": target,
    }))
    .unwrap();
    fs::write(&registration, &registration_bytes).unwrap();
    reject(V2_POLICY, &mapping, 7, "AUTHORITY_UNVERIFIED");
    assert_eq!(fs::read(&registration).unwrap(), registration_bytes);
    assert!(!target.exists());
    fs::remove_dir_all(&state).unwrap();

    let worktree_edit = source.join("untracked-private-record.md");
    fs::write(&worktree_edit, "synthetic untracked record").unwrap();
    reject(V2_POLICY, &mapping, 5, "DIRTY_CONFLICT");
    assert!(!target.exists());
    fs::remove_file(&worktree_edit).unwrap();

    // Dolt's fsck runs after the private import commit and before publication.
    // Simulate a separate process changing the source while Hive is staging.
    use std::os::unix::fs::PermissionsExt;
    let dolt = std::env::var_os("HIVE_DOLT_BIN").unwrap();
    let hooks = root.join("fixture-bin");
    fs::create_dir(&hooks).unwrap();
    let wrapper = hooks.join("dolt");
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    fs::write(&wrapper, format!(
        "#!/bin/sh\nif [ \"$1\" = fsck ]; then printf '%s\\n' synthetic > {}; fi\nexec {} \"$@\"\n",
        quote(&worktree_edit), quote(Path::new(&dolt))
    )).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let search_path = std::env::var_os("PATH").unwrap();
    let injected_path = std::env::join_paths(
        std::iter::once(hooks.clone()).chain(std::env::split_paths(&search_path)),
    )
    .unwrap();
    if include_queens {
        let pinned_git = std::env::split_paths(&search_path)
            .map(|path| path.join("git"))
            .find(|path| path.is_file())
            .unwrap();
        let git_wrapper = hooks.join("git");
        let before_insert = root.join("asset-verified-before-sql");
        let drift_wrapper = fs::read(&wrapper).unwrap();
        fs::write(&wrapper, format!(
            "#!/bin/sh\nif [ \"$*\" = 'sql -r json' ]; then\n  {} -C ../assets cat-file -e HEAD:bots/queen-synth/SOUL.md || exit 93\n  printf '%s\\n' inspected > {}\n  exit 92\nfi\nexec {} \"$@\"\n",
            quote(&pinned_git), quote(&before_insert), quote(Path::new(&dolt)),
        )).unwrap();
        fs::write(&git_wrapper, format!(
            "#!/bin/sh\ncase \"$*\" in *'commit --no-gpg-sign -m Import reviewed Hive v1 Queen Souls') exit 91;; esac\nexec {} \"$@\"\n",
            quote(&pinned_git),
        )).unwrap();
        fs::set_permissions(&git_wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let fail = || {
            response(
                stage_command("staged-import", V2_POLICY).env("PATH", &injected_path),
                7,
            )
        };
        let git_failure = fail();
        assert_eq!(git_failure["error"]["code"], "IO_ERROR");
        assert!(
            git_failure["error"]["message"]
                .as_str()
                .unwrap()
                .contains("91")
        );
        assert!(!before_insert.exists());
        assert!(!target.exists());
        fs::remove_file(git_wrapper).unwrap();

        let db_failure = fail();
        assert_eq!(db_failure["error"]["code"], "IO_ERROR");
        assert!(
            db_failure["error"]["message"]
                .as_str()
                .unwrap()
                .contains("92")
        );
        assert_eq!(fs::read_to_string(before_insert).unwrap(), "inspected\n");
        assert!(!target.exists());
        fs::write(&wrapper, drift_wrapper).unwrap();
    }
    let late_drift = response(
        stage_command("staged-import", V2_POLICY).env("PATH", injected_path),
        5,
    );
    assert_eq!(late_drift["error"]["code"], "DIRTY_CONFLICT");
    assert!(
        !late_drift
            .to_string()
            .contains(worktree_edit.to_str().unwrap())
    );
    assert!(!target.exists());
    fs::remove_file(worktree_edit).unwrap();

    let imported = stage("staged-import", V2_POLICY, &mapping, 0)["data"].clone();
    assert_eq!(imported["mode"], "staged_import");
    for key in [
        "counts",
        "id_map",
        "source",
        "mapping_sha256",
        "holds",
        "exclusions",
    ] {
        assert_eq!(imported[key], dry_run[key]);
    }
    assert_eq!(imported["checks"]["committed_record_validation"], "passed");
    assert_eq!(imported["checks"]["published_inactive"], true);
    let db_commit = imported["target"]["confirmed_dolt_commit"]
        .as_str()
        .unwrap();
    assert_ne!(db_commit, imported["target"]["base_dolt_commit"]);
    let v2 = response(cli("2").args(["config", "show", "--scope", "workspace"]), 0);
    assert_eq!(v2["data"]["workspace_id"], workspace_id);
    assert_eq!(v2["data"]["policy_revision"], V2_POLICY);
    assert_eq!(v1(), before);
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
            "SELECT id, parent_id, name FROM areas AS OF '{db_commit}' ORDER BY id"
        )),
        json!([
            {"id": general, "name": "General"}, {"id": userdata, "parent_id": app, "name": "Operations"},
            {"id": app, "parent_id": general, "name": NAME},
        ])
    );
    let baseline = imported["target"]["base_dolt_commit"].as_str().unwrap();
    assert_eq!(
        sql(&format!(
            "SELECT COUNT(*) AS n FROM areas AS OF '{baseline}'"
        )),
        json!([{"n": 0}])
    );
    assert_eq!(
        sql("SELECT next_value FROM workspace_counters"),
        json!([{"next_value": 1}])
    );
    assert_eq!(
        git_text(&target.join("assets"), &home, &["rev-parse", "HEAD"]),
        imported["target"]["asset_git_commit"]
    );
    if include_queens {
        let asset_root = target.join("assets");
        let asset_commit = imported["target"]["asset_git_commit"].as_str().unwrap();
        assert_eq!(imported["checks"]["target_bot_rows"], 1);
        assert_eq!(imported["queen_preparation"], dry_run["queen_preparation"]);
        assert_eq!(
            sql(&format!(
                "SELECT id, slug, display_name, retired_at FROM bots AS OF '{db_commit}'"
            )),
            json!([{"id": bot_id, "slug": "queen-synth", "display_name": QUEEN_NAME}])
        );
        assert_eq!(
            sql(&format!(
                "SELECT COUNT(*) AS n FROM bots AS OF '{baseline}'"
            )),
            json!([{"n": 0}])
        );
        assert_eq!(
            sql(
                "SELECT (SELECT COUNT(*) FROM bot_memories) AS memories, (SELECT COUNT(*) FROM artifacts) AS artifacts"
            ),
            json!([{"memories": 0, "artifacts": 0}])
        );
        let tree = git_text(&asset_root, &home, &["ls-tree", "-r", asset_commit]);
        let oid = git_text(
            &asset_root,
            &home,
            &[
                "rev-parse",
                &format!("{asset_commit}:bots/queen-synth/SOUL.md"),
            ],
        );
        assert_eq!(tree, format!("100644 blob {oid}\tbots/queen-synth/SOUL.md"));
        assert_eq!(
            git(&asset_root, &home, &["cat-file", "blob", &oid]),
            SOUL.as_bytes()
        );
        let soul = asset_root.join("bots/queen-synth/SOUL.md");
        assert_eq!(fs::read(&soul).unwrap(), SOUL.as_bytes());
        assert_eq!(
            fs::metadata(soul).unwrap().permissions().mode() & 0o777,
            0o600
        );
    } else {
        assert!(git_text(&target.join("assets"), &home, &["ls-tree", "HEAD"]).is_empty());
    }
    assert!(
        fs::read_dir(target.join("conversations"))
            .unwrap()
            .next()
            .is_none()
    );
    reject(V2_POLICY, &mapping, 5, "ALREADY_EXISTS");
    verify_source();
    assert_eq!(
        sources.map(|name| fs::read(source.join(name)).unwrap()),
        source_bytes
    );
    assert_eq!(fs::read_to_string(v1_config).unwrap(), v1_config_bytes);
    assert_eq!(fs::read_to_string(v2_config).unwrap(), v2_config_bytes);
    assert!(!state.exists());
    assert!(!data.exists());
    assert!(!fs::read_dir(root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".hive-init-incomplete-")
    }));
}

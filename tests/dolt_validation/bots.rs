use mysql::Conn;
use mysql::prelude::Queryable;

use super::{ADMIN, Fixture, assert_diagnostics, commit, id, storage_state};

fn insert_bot(admin: &mut Conn, bot_id: &str, slug: &str, display_name: &str, retired: bool) {
    admin
        .exec_drop(
            "INSERT INTO bots (id, slug, display_name, revision, retired_at)
             VALUES (?, ?, ?, 'opaque-bot-revision', IF(?, UTC_TIMESTAMP(6), NULL))",
            (bot_id, slug, display_name, retired),
        )
        .unwrap();
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn validates_committed_queen_registry_and_redacts_malformed_values() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    // Multiple active and retired Queens are valid, and existing issue_claims do not bind bots.
    insert_bot(&mut admin, &id(0x90), "queen-alpha", "Queen Alpha", false);
    insert_bot(&mut admin, &id(0x91), "queen-2-beta", "Queen Beta", false);
    insert_bot(
        &mut admin,
        &id(0x92),
        "queen-retired",
        "Retired Queen",
        true,
    );
    commit(&mut admin);
    let valid = fixture.check(0, &mut admin);
    let baseline = storage_state(&mut admin);

    let (id93, id94, id95, id96) = (id(0x93), id(0x94), id(0x95), id(0x96));
    let uppercase_id = id(0x9a).to_ascii_uppercase();
    let uuidv4_id = id(0x9b).replace("-7000-", "-4000-");
    let invalid_id = "invalid-bot-id-fixture";

    let malformed = [
        (uuidv4_id.as_str(), "queen-v4-id", "Retired V4 Queen", true),
        (id93.as_str(), "Queen-Upper", "Upper Slug Queen", false),
        (id94.as_str(), "queen_under", "Retired Under Queen", true),
        (id95.as_str(), "queen--double", "Double Hyphen Queen", false),
        (id96.as_str(), "../queen", "Traversal Queen", false),
        (uppercase_id.as_str(), "queen-upper-id", "Upper ID", false),
        (invalid_id, "bad/slug", "Invalid Bot Row", true),
    ];
    for (bot_id, slug, display_name, retired) in malformed {
        insert_bot(&mut admin, bot_id, slug, display_name, retired);
    }

    let dirty = storage_state(&mut admin);
    assert_ne!(dirty.1, baseline.1);
    assert_eq!(dirty.2, baseline.2);
    assert_eq!(fixture.check(0, &mut admin), valid);

    admin.query_drop("CALL DOLT_ADD('bots')").unwrap();
    assert_eq!(storage_state(&mut admin).2, dirty.1);
    assert_eq!(fixture.check(0, &mut admin), valid);

    commit(&mut admin);
    let report = fixture.check(4, &mut admin);
    assert_diagnostics(
        &report,
        &[
            ("INVALID_ID", "bots", &[]),
            ("INVALID_BOT_SLUG", "bots", &[&id93]),
            ("INVALID_BOT_SLUG", "bots", &[&id94]),
            ("INVALID_BOT_SLUG", "bots", &[&id95]),
            ("INVALID_BOT_SLUG", "bots", &[&id96]),
            ("INVALID_ID", "bots", &[]),
            ("INVALID_ID", "bots", &[]),
            ("INVALID_BOT_SLUG", "bots", &[]),
        ],
    );
    let report_text = report.to_string();
    for sensitive in [
        invalid_id,
        &uppercase_id,
        &uuidv4_id,
        "Queen-Upper",
        "queen_under",
        "queen--double",
        "../queen",
        "bad/slug",
        "opaque-bot-revision",
        "Queen Alpha",
    ] {
        assert!(!report_text.contains(sensitive), "{sensitive}");
    }
    assert_eq!(fixture.check(4, &mut admin), report);

    for (bot_id, _, _, _) in malformed {
        admin
            .exec_drop("DELETE FROM bots WHERE id=?", (bot_id,))
            .unwrap();
    }
    commit(&mut admin);
    let restored = fixture.check(0, &mut admin);
    assert_eq!(restored["data"], valid["data"]);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn inspects_committed_bot_metadata_and_pins_page_continuations() {
    use serde_json::{Value, json};

    let fixture = Fixture::start_empty();
    let mut admin = fixture.connect(ADMIN);
    let active = id(0x9a);
    let second = id(0x9b);
    let retired = id(0x9c);
    let staged = id(0x9d);
    let case_mismatched = id(0x9e);

    let run = |command: &[&str], exit: i32| {
        let before = storage_state(&mut fixture.connect(ADMIN));
        let workspace = id(1);
        let port = fixture.port.to_string();
        let mut args = vec!["--contract", "2", "--json", "bot"];
        args.extend(command);
        args.extend([
            "--workspace-id",
            &workspace,
            "--database",
            super::DATABASE,
            "--port",
            &port,
        ]);
        let output = fixture.run_hive(&args, &fixture.password);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{args:?} {report}");
        assert!(output.stderr.is_empty());
        assert_eq!(report["command"], format!("bot {}", command[0]));
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["context"]["workspace_id"], workspace);
        assert_eq!(report["context"]["db_commit"], before.0);
        assert_eq!(report["ok"], exit == 0);
        if exit == 0 {
            assert_eq!(report["data"]["observed_revision"], before.0);
        }
        assert_eq!(storage_state(&mut fixture.connect(ADMIN)), before);
        report
    };

    let empty = run(&["list"], 0);
    assert_eq!(empty["data"]["items"], json!([]));
    assert_eq!(empty["data"]["next_cursor"], Value::Null);
    insert_bot(&mut admin, &retired, "queen-retired", "Retired Queen", true);
    insert_bot(&mut admin, &second, "queen-alpha", "Second Queen", false);
    insert_bot(&mut admin, &active, "queen-zeta", "First Queen", false);
    // The inherited collation is case-insensitive.
    insert_bot(
        &mut admin,
        &case_mismatched.to_ascii_uppercase(),
        "queen-invalid",
        "Malformed Queen",
        true,
    );
    commit(&mut admin);

    let detail = run(&["show", &retired], 0);
    assert_eq!(detail["data"]["bot"]["status"], "retired");
    assert!(
        detail["data"]["bot"]["retired_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    let malformed = run(&["list", "--status", "retired", "--limit", "1"], 4);
    assert_eq!(malformed["error"]["code"], "INVALID_RECORD");
    assert!(!malformed.to_string().contains("queen-invalid"));
    assert_eq!(
        run(&["show", &case_mismatched], 3)["error"]["code"],
        "NOT_FOUND"
    );
    admin
        .exec_drop("DELETE FROM bots WHERE id = ?", (&case_mismatched,))
        .unwrap();
    commit(&mut admin);

    assert_eq!(
        run(&["list", "--status", "retired"], 0)["data"]["items"],
        json!([detail["data"]["bot"]])
    );
    let all = run(&["list", "--status", "all", "--limit", "100"], 0);
    assert_eq!(all["data"]["items"].as_array().unwrap().len(), 3);

    let first = run(&["list", "--limit", "1"], 0);
    assert_eq!(
        first["data"]["items"],
        json!([{
            "id": active, "slug": "queen-zeta", "display_name": "First Queen",
            "revision": "opaque-bot-revision", "retired_at": null, "status": "active", "role": "queen"
        }])
    );
    assert_eq!(first["data"]["truncated"], true);
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    assert_eq!(
        run(&["list", "--limit", "1", "--status", "active"], 0),
        first
    );

    insert_bot(
        &mut admin,
        &staged,
        "queen-uncommitted",
        "Staged Queen",
        false,
    );
    admin.query_drop("CALL DOLT_ADD('bots')").unwrap();
    admin
        .exec_drop(
            "UPDATE bots SET display_name = 'Working Queen' WHERE id = ?",
            (&second,),
        )
        .unwrap();

    assert_eq!(run(&["list", "--limit", "1"], 0), first);
    let continuation = ["list", "--limit", "1", "--cursor", cursor];
    let next = run(&continuation, 0);
    assert_eq!(next["data"]["items"][0]["id"], second);
    assert_eq!(next["data"]["items"][0]["display_name"], "Second Queen");
    assert_eq!(next["data"]["truncated"], false);
    assert_eq!(next["data"]["next_cursor"], Value::Null);
    assert_eq!(run(&["show", &staged], 3)["error"]["code"], "NOT_FOUND");
    for args in [
        vec!["list", "--limit", "2", "--cursor", cursor],
        vec![
            "list", "--limit", "1", "--status", "all", "--cursor", cursor,
        ],
    ] {
        assert_eq!(run(&args, 5)["error"]["code"], "STALE_CURSOR");
    }

    commit(&mut admin);
    assert_eq!(run(&continuation, 5)["error"]["code"], "STALE_CURSOR");
    assert_eq!(run(&["show", &staged], 0)["data"]["bot"]["id"], staged);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn inspects_only_the_explicit_committed_soul() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::process::Command;

    use serde_json::Value;

    let fixture = Fixture::start_empty();
    let mut admin = fixture.connect(ADMIN);
    let queen = id(0x90);
    let workspace = id(1);
    let policy = "c66b3c50150e18426485d134de92c6b311dac125";
    insert_bot(&mut admin, &queen, "queen-alpha", "Queen Alpha", true);
    commit(&mut admin);
    admin.query_drop("UPDATE bots SET slug = 'staged'").unwrap();
    admin.query_drop("CALL DOLT_ADD('bots')").unwrap();
    admin.query_drop("UPDATE bots SET slug = 'dirty'").unwrap();
    let db_before = storage_state(&mut admin);

    let root = fixture.root.join("copy");
    let assets = root.join("assets");
    let directory = assets.join("bots/queen-alpha");
    let soul = directory.join("SOUL.md");
    fs::create_dir_all(&directory).unwrap();
    let descriptor = |id: &str| {
        format!("version: 2\nworkspace_id: {id}\nschema_version: 1\npolicy_revision: {policy}\n")
    };
    let marker = root.join("hive-workspace.yml");
    fs::write(&marker, descriptor(&workspace)).unwrap();
    // The conversation root need not even be readable for Soul inspection.
    symlink("missing-conversations", root.join("conversations")).unwrap();

    let git = |args: &[&str]| {
        let output = Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", fixture.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(&assets)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let save = || {
        git(&["add", "--all"]);
        git(&["commit", "-m", "Synthetic assets"]);
        git(&["rev-parse", "HEAD"])
    };
    git(&["init", "--initial-branch=main"]);
    fs::write(directory.join("README.md"), "management-only-readme").unwrap();
    fs::write(directory.join("AGENTS.md"), "management-only-agents").unwrap();
    let missing = save();
    let body = "# Synthetic Queen 기록\r\nKeep exact bytes.";
    fs::write(
        &soul,
        format!(
            "---\r\nworkspace_id: {workspace}\r\nbot_id: {queen}\r\nslug: queen-alpha\r\nrole: queen\r\n---\r\n{body}"
        ),
    )
    .unwrap();
    let selected = save();
    fs::write(&soul, "later-commit").unwrap();
    let other = save();
    fs::write(&soul, "staged-only").unwrap();
    git(&["add", "--all"]);
    fs::write(&soul, "dirty-only").unwrap();
    let index = fs::read(assets.join(".git/index")).unwrap();
    let head = git(&["rev-parse", "HEAD"]);

    let run = |contract: &str, expected_workspace: &str, id: &str, revision: &str| {
        let port = fixture.port.to_string();
        let output = fixture.run_hive(
            &[
                "--contract",
                contract,
                "--json",
                "bot",
                "show",
                id,
                "--workspace",
                "copy",
                "--workspace-id",
                expected_workspace,
                "--database",
                super::DATABASE,
                "--port",
                &port,
                "--asset-commit",
                revision,
            ],
            &fixture.password,
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.success(), report["ok"] == true, "{report}");
        assert!(output.stderr.is_empty());
        assert_eq!(storage_state(&mut fixture.connect(ADMIN)), db_before);
        report
    };
    let inspect = |revision: &str| run("2", &workspace, &queen, revision);
    let report = inspect(&selected);
    assert_eq!(report["data"]["soul"]["body"], body);
    assert_eq!(
        report["data"]["soul"]["path"],
        "assets/bots/queen-alpha/SOUL.md"
    );
    assert_eq!(report["data"]["bot"]["id"], queen);
    assert_eq!(report["data"]["bot"]["status"], "retired");
    assert_eq!(report["context"]["db_commit"], db_before.0);
    assert_eq!(report["context"]["asset_commit"], selected);
    assert_eq!(report["context"]["policy_revision"], policy);
    assert_eq!(report["context"]["workspace_id"], workspace);
    assert_eq!(inspect(&other)["data"]["soul"]["body"], "later-commit");
    assert_eq!(inspect(&missing)["error"]["code"], "NOT_FOUND");
    assert_eq!(
        run("1", &workspace, &queen, &selected)["error"]["code"],
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        run("2", &workspace, &id(0x91), &selected)["error"]["code"],
        "NOT_FOUND"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), descriptor(&workspace));
    assert_eq!(
        run("2", &id(2), &queen, &selected)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    fs::write(&marker, descriptor(&id(2))).unwrap();
    assert_eq!(
        run("2", &id(2), &queen, &selected)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    fs::write(&marker, descriptor(&workspace)).unwrap();
    fs::write(root.join("hive.yml"), "version: 1\n").unwrap();
    assert_eq!(inspect(&selected)["error"]["code"], "SCHEMA_INCOMPATIBLE");
    fs::remove_file(root.join("hive.yml")).unwrap();

    assert_eq!(fs::read(assets.join(".git/index")).unwrap(), index);
    assert_eq!(git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read_to_string(&soul).unwrap(), "dirty-only");
}

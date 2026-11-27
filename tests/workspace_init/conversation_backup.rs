use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};

const ACTIVE: &str = "01900000-0000-7000-8000-000000000021";
const RETIRED: &str = "01900000-0000-7000-8000-000000000022";
const UNREGISTERED: &str = "01900000-0000-7000-8000-000000000023";
const SESSION: &str = "synthetic-shared-session";
const BODY_MARKER: &str = "synthetic-private-history";

fn private_dir(path: &Path) {
    DirBuilder::new().mode(0o700).create(path).unwrap();
}

fn private_file(path: &Path, text: &[u8]) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn checked(command: &mut Command) {
    let result = command.output().unwrap();
    assert!(result.status.success(), "{result:?}");
}

#[test]
#[ignore = "requires installed Dolt 2.7.6 and Git 2.56.1"]
fn conversation_archives_restore_only_explicit_verified_queen_history() {
    let fixture = Fixture::new();
    let root = fixture.dir("workspace");
    let state = fixture.dir("state");
    let home = fixture.dir("offline-home");
    let bin = fixture.dir("bin");
    let shared = fixture.dir("shared");
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o2700)).unwrap();
    assert_eq!(fs::metadata(&shared).unwrap().mode() & 0o7777, 0o2700);
    let empty_archive = "shared/empty-archive";
    let included_archive = "shared/archive";
    let restored_history = "shared/history";
    let dolt = PathBuf::from(std::env::var_os("HIVE_DOLT_BIN").unwrap());
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    symlink(&dolt, bin.join("dolt")).unwrap();
    symlink(&git, bin.join("git")).unwrap();
    let initialized = response(
        fixture
            .init(&root)
            .arg("--register-runtime")
            .env("XDG_STATE_HOME", &state)
            .output()
            .unwrap(),
        0,
    );
    let workspace_id = initialized["data"]["workspace_id"].as_str().unwrap();
    let backup = |destination: &str, include: bool| {
        let mut command = fixture.hive();
        command
            .env("XDG_STATE_HOME", &state)
            .args(["--contract", "2", "--json", "--workspace"])
            .arg(&root)
            .args(["backup", "create", "--destination", destination]);
        if include {
            command.arg("--include-conversations");
        }
        command.output().unwrap()
    };
    let restore_state = fixture.0.join("unused-restore-state");
    let restore = |source: &str, target: &str, include: bool| {
        let mut command = fixture.hive();
        command.env("XDG_STATE_HOME", &restore_state).args([
            "--contract",
            "2",
            "--json",
            "backup",
            "restore",
            "--source",
            source,
            "--target",
            target,
        ]);
        if include {
            command.arg("--include-conversations");
        }
        command.output().unwrap()
    };
    let assert_empty = |destination: &str| {
        assert_eq!(
            fs::read_dir(fixture.0.join(destination).join("conversations"))
                .unwrap()
                .count(),
            0
        );
    };
    let empty = response(backup(empty_archive, true), 0);
    assert_eq!(empty["data"]["manifest"]["format_version"], 2);
    assert_eq!(empty["data"]["manifest"]["conversations_included"], true);
    assert_eq!(
        empty["data"]["manifest"]["conversations"]["files"],
        json!([])
    );
    let empty_restore = response(restore(empty_archive, "empty-history", true), 0);
    assert_eq!(empty_restore["data"]["active_root"], "inactive");
    assert_eq!(empty_restore["data"]["conversations_restored"], true);
    assert_empty("empty-history");

    let db = || {
        let mut command = Command::new(&dolt);
        command
            .current_dir(root.join("db"))
            .env_clear()
            .env("HOME", &home)
            .env("DOLT_ROOT_PATH", &home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true");
        command
    };
    let assets = root.join("assets");
    let git = || {
        let mut command = Command::new(&git);
        command
            .current_dir(&assets)
            .env_clear()
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Synthetic")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Synthetic")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
        command
    };
    checked(db().args(["sql", "-q", &format!(
        "INSERT INTO bots VALUES
        ('{ACTIVE}', 'active-queen', 'Synthetic Active', '01900000-0000-7000-8000-000000000031', NULL),
        ('{RETIRED}', 'retired-queen', 'Synthetic Retired', '01900000-0000-7000-8000-000000000032', UTC_TIMESTAMP(6))"
    )]));
    checked(db().args(["add", "."]));
    checked(db().args([
        "commit",
        "--author",
        "Synthetic <test@example.invalid>",
        "-m",
        "Register synthetic Queens",
    ]));

    let bots = root.join("conversations/bots");
    private_dir(&bots);
    let mut histories = Vec::new();
    for (index, (id, slug)) in [(ACTIVE, "active-queen"), (RETIRED, "retired-queen")]
        .into_iter()
        .enumerate()
    {
        let soul = assets.join(format!("bots/{slug}/SOUL.md"));
        fs::create_dir_all(soul.parent().unwrap()).unwrap();
        fs::write(
            &soul,
            format!("---\nworkspace_id: {workspace_id}\nbot_id: {id}\nbot_slug: {slug}\n---\nSynthetic identity\n"),
        ).unwrap();
        let queen = bots.join(id);
        private_dir(&queen);
        fs::write(queen.join(".tmp-unpublished.md"), "unpublished").unwrap();
        for revision in 1..=2 {
            let conversation = format!("01900000-0000-7000-8000-{:012x}", index * 2 + revision);
            let relative = format!("conversations/bots/{id}/{conversation}.md");
            let body = format!("# 기록\r\n{BODY_MARKER} {slug} {revision}");
            let bytes = format!(
                "---\nversion: 1\nworkspace_id: {workspace_id}\nbot_id: {id}\nbot_slug: {slug}\n\
                 conversation_id: {conversation}\nimported_at: \"2026-11-27T09:00:00Z\"\n\
                 provenance:\n  source: file\n  session_ref: {SESSION}\n  body_sha256: {}\n---\n{body}",
                digest(body.as_bytes())
            ).into_bytes();
            private_file(&root.join(&relative), &bytes);
            histories.push((relative, id, slug, conversation, bytes));
        }
    }
    checked(git().args(["add", "."]));
    checked(git().args(["commit", "-m", "Record synthetic Soul identities"]));
    let orphan = bots.join(UNREGISTERED);
    fs::create_dir(&orphan).unwrap();
    fs::write(orphan.join("unregistered.md"), "unregistered history").unwrap();

    let (first, _, _, _, first_bytes) = &histories[0];
    let first_source = root.join(first);
    fs::set_permissions(&first_source, fs::Permissions::from_mode(0o644)).unwrap();
    let default = response(backup("paired", false), 0);
    let paired = &default["data"]["manifest"];
    assert_eq!(paired["format_version"], 1);
    assert_eq!(paired["conversations_included"], false);
    assert!(paired.get("conversations").is_none());
    assert!(!fixture.0.join("paired/conversations").exists());
    response(backup("nonprivate", true), 7);
    assert!(!fixture.0.join("nonprivate").exists());
    private_file(&first_source, b"malformed private file");
    response(backup("malformed-source", true), 4);
    assert!(!fixture.0.join("malformed-source").exists());
    private_file(&first_source, first_bytes);
    let missing = response(restore("paired", "missing-section", true), 7);
    assert!(
        missing["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no conversation archive section")
    );
    assert!(!fixture.0.join("missing-section").exists());
    response(restore("paired", "paired-restored", false), 0);
    assert_empty("paired-restored");

    let captured = response(backup(included_archive, true), 0);
    let manifest = &captured["data"]["manifest"];
    assert_eq!(manifest["format_version"], 2);
    assert_eq!(manifest["conversations_included"], true);
    assert_eq!(manifest["dolt_commit"], paired["dolt_commit"]);
    assert_eq!(manifest["asset_git_commit"], paired["asset_git_commit"]);
    assert!(!captured.to_string().contains(BODY_MARKER));
    assert!(!captured.to_string().contains(SESSION));
    let archive = fixture.0.join(included_archive);
    assert_eq!(fs::metadata(&archive).unwrap().mode() & 0o7777, 0o700);
    assert_eq!(
        fs::metadata(archive.join("conversations")).unwrap().mode() & 0o7777,
        0o700
    );
    let manifest_path = archive.join("manifest.json");
    let original_manifest = fs::read(&manifest_path).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&original_manifest).unwrap(),
        *manifest
    );
    let files = manifest["conversations"]["files"].as_array().unwrap();
    assert_eq!(files.len(), histories.len());
    for (relative, id, slug, conversation, bytes) in &histories {
        let entry = files
            .iter()
            .find(|entry| entry["path"] == *relative)
            .unwrap();
        assert_eq!(entry["workspace_id"], workspace_id);
        assert_eq!(entry["bot_id"], *id);
        assert_eq!(entry["bot_slug"], *slug);
        assert_eq!(entry["conversation_id"], *conversation);
        assert_eq!(entry["bytes"], bytes.len());
        assert_eq!(entry["sha256"], digest(bytes));
        assert_eq!(fs::read(archive.join(relative)).unwrap(), *bytes);
    }
    assert!(
        !archive
            .join("conversations/bots")
            .join(UNREGISTERED)
            .exists()
    );
    assert!(
        !archive
            .join("conversations/bots")
            .join(ACTIVE)
            .join(".tmp-unpublished.md")
            .exists()
    );

    let restored = response(restore(included_archive, restored_history, true), 0);
    assert_eq!(restored["data"]["active_root"], "inactive");
    assert_eq!(restored["data"]["conversations_restored"], true);
    assert_eq!(
        fs::metadata(fixture.0.join(restored_history))
            .unwrap()
            .mode()
            & 0o7777,
        0o700
    );
    for (relative, _, _, _, bytes) in &histories {
        let restored_file = fixture.0.join(restored_history).join(relative);
        assert_eq!(fs::read(&restored_file).unwrap(), *bytes);
        assert_eq!(restored_file.metadata().unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            restored_file.parent().unwrap().metadata().unwrap().mode() & 0o777,
            0o700
        );
    }

    let target = fixture.dir("rejected");
    let rejected = |status| {
        let result = response(restore(included_archive, "rejected", true), status);
        assert!(!result.to_string().contains(BODY_MARKER));
        assert!(!result.to_string().contains(SESSION));
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        assert!(fixture.stages().is_empty());
    };
    let entry_index = files
        .iter()
        .position(|entry| entry["path"] == *first)
        .unwrap();
    for (field, value) in [
        ("path", json!("../outside.md")),
        ("workspace_id", json!(UNREGISTERED)),
        ("bot_id", json!(UNREGISTERED)),
        ("bot_slug", json!("retired-queen")),
        ("conversation_id", json!("invalid")),
        ("sha256", json!("0".repeat(64))),
        ("bytes", json!(5 * 1024 * 1024)),
    ] {
        let mut changed = manifest.clone();
        changed["conversations"]["files"][entry_index][field] = value;
        fs::write(&manifest_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        rejected(7);
    }
    let mut changed = manifest.clone();
    changed["conversations"]["files"]
        .as_array_mut()
        .unwrap()
        .push(files[entry_index].clone());
    fs::write(&manifest_path, serde_json::to_vec(&changed).unwrap()).unwrap();
    rejected(7);
    let archived_file = archive.join(first);
    let mut corrupt_body = first_bytes.clone();
    corrupt_body.push(b'!');
    for bad in [corrupt_body, [first_bytes.as_slice(), b"\xff"].concat()] {
        let mut changed = manifest.clone();
        changed["conversations"]["files"][entry_index]["bytes"] = json!(bad.len());
        changed["conversations"]["files"][entry_index]["sha256"] = json!(digest(&bad));
        fs::write(&manifest_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        private_file(&archived_file, &bad);
        rejected(4);
    }
    fs::write(&manifest_path, &original_manifest).unwrap();
    fs::remove_file(&archived_file).unwrap();
    rejected(7);
    symlink(&first_source, &archived_file).unwrap();
    let symlink_error = restore(included_archive, "rejected", true);
    assert!(!symlink_error.status.success(), "{symlink_error:?}");
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    fs::remove_file(&archived_file).unwrap();
    private_file(&archived_file, first_bytes);
    fs::set_permissions(&archived_file, fs::Permissions::from_mode(0o644)).unwrap();
    rejected(7);
    private_file(&archived_file, first_bytes);
    fs::OpenOptions::new()
        .write(true)
        .open(&archived_file)
        .unwrap()
        .set_len(5 * 1024 * 1024)
        .unwrap();
    rejected(7);
    private_file(&archived_file, first_bytes);

    let conversation_root = archive.join("conversations");
    let parked = fixture.0.join("parked-conversations");
    fs::rename(&conversation_root, &parked).unwrap();
    symlink(root.join("conversations"), &conversation_root).unwrap();
    let mut changed = manifest.clone();
    changed["conversations"]["files"] = json!("invalid entries");
    fs::write(&manifest_path, serde_json::to_vec(&changed).unwrap()).unwrap();
    rejected(7);
    let omitted = response(restore(included_archive, "omitted", false), 0);
    assert_eq!(omitted["data"]["conversations_restored"], false);
    assert_empty("omitted");
    fs::remove_file(&conversation_root).unwrap();
    fs::rename(parked, conversation_root).unwrap();
    fs::write(&manifest_path, &original_manifest).unwrap();
    assert!(!restore_state.exists());
    assert_eq!(fs::read(manifest_path).unwrap(), original_manifest);
    for (relative, _, _, _, bytes) in histories {
        assert_eq!(fs::read(root.join(&relative)).unwrap(), bytes);
        assert_eq!(fs::read(archive.join(relative)).unwrap(), bytes);
    }
}

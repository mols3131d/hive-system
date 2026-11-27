use super::*;

const POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
const PRIVATE_BODY_MARKER: &str = "private-conversation-fixture-body";
const BODY: &str = concat!(
    "\r\n# Synthetic 기록\r\n---\n",
    "private-conversation-fixture-body",
    "\nLast line without a newline"
);

fn descriptor(workspace_id: &str) -> String {
    format!(
        "version: 2\nworkspace_id: {workspace_id}\nschema_version: 1\npolicy_revision: {POLICY}\n"
    )
}

fn published(workspace_id: &str, bot: &str, slug: &str, conversation: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(BODY.as_bytes()));
    format!(
        "---\nversion: 1\nworkspace_id: {workspace_id}\nbot_id: {bot}\nbot_slug: {slug}\n\
         conversation_id: {conversation}\nimported_at: \"2026-11-27T09:00:00Z\"\ntitle: Synthetic title\n\
         provenance:\n  source: file\n  session_ref: synthetic-session\n  body_sha256: {digest}\n---\n{BODY}"
    )
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn conversation_reads_only_the_explicit_verified_copy() {
    let fixture = Fixture::start();
    let workspace_id = id(1);
    let retired = id(16);
    let conversation = id(30);
    let mut admin = fixture.connect(ADMIN);
    admin
        .exec_drop(
            "INSERT INTO bots (id, slug, display_name, revision, retired_at)
         VALUES (?, 'retired', 'Synthetic retired Queen', ?, UTC_TIMESTAMP(6))",
            (&retired, id(3)),
        )
        .unwrap();
    commit(&mut admin);
    // The working set disagrees with the committed registry.
    admin
        .query_drop("UPDATE bots SET slug = 'working-only' WHERE slug = 'fixture'")
        .unwrap();
    let before = storage_state(&mut admin);

    let copy = fixture.root.join("copy");
    fs::create_dir(&copy).unwrap();
    let marker = copy.join("hive-workspace.yml");
    fs::write(&marker, descriptor(&workspace_id)).unwrap();
    let port = fixture.port.to_string();
    let command =
        |bot: &str, expected_workspace: &str, password: &str, destination: Option<&str>| {
            let mut args = vec![
                "conversation",
                if destination.is_some() {
                    "export"
                } else {
                    "show"
                },
                "--workspace",
                "copy",
                "--workspace-id",
                expected_workspace,
                "--database",
                DATABASE,
                "--port",
                &port,
                "--bot",
                bot,
                "--id",
                &conversation,
                "--contract",
                "2",
                "--json",
            ];
            if let Some(destination) = destination {
                args.extend(["--destination", destination]);
            }
            fixture.hive_command(&args, password)
        };
    let show = |bot, expected_workspace, password| {
        command(bot, expected_workspace, password, None)
            .output()
            .unwrap()
    };
    let report = |output: Output, status: i32| {
        let json: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(status), "{json}");
        assert!(output.stderr.is_empty());
        assert_eq!(json["schema_version"], 2);
        assert_eq!(json["command"], "conversation show");
        assert_eq!(json["ok"], status == 0);
        if status != 0 {
            assert!(json.get("data").is_none());
            let response = String::from_utf8(output.stdout).unwrap();
            for private in [
                PRIVATE_BODY_MARKER,
                "Synthetic title",
                "synthetic-session",
                "private-header-value",
            ] {
                assert!(!response.contains(private));
            }
        }
        json
    };
    #[cfg(target_os = "linux")]
    let export =
        |bot: &str, expected_workspace: &str, password: &str, destination: &str, status| {
            let output = command(bot, expected_workspace, password, Some(destination))
                .output()
                .unwrap();
            let json: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(output.status.code(), Some(status), "{json}");
            assert!(output.stderr.is_empty());
            assert_eq!(json["schema_version"], 2);
            assert_eq!(json["command"], "conversation export");
            assert_eq!(json["ok"], status == 0);
            let stdout = String::from_utf8(output.stdout).unwrap();
            for private in [
                PRIVATE_BODY_MARKER,
                "Synthetic title",
                "synthetic-session",
                destination,
            ] {
                assert!(!stdout.contains(private));
            }
            json
        };
    let active = id(15);
    for (bot, slug) in [(&active, "fixture"), (&retired, "retired")] {
        let folder = copy.join("conversations/bots").join(bot);
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{conversation}.md"));
        let content = published(&workspace_id, bot, slug, &conversation);
        fs::write(&path, &content).unwrap();
        let json = report(show(bot, &workspace_id, &fixture.password), 0);
        assert_eq!(json["context"]["workspace_id"], workspace_id);
        assert_eq!(json["context"]["db_commit"], before.0);
        assert_eq!(json["data"]["metadata"]["bot_slug"], slug);
        assert_eq!(json["data"]["metadata"]["conversation_id"], conversation);
        assert_eq!(json["data"]["body"], BODY);
        #[cfg(target_os = "linux")]
        {
            let destination = format!("export-{slug}.md");
            let json = export(bot, &workspace_id, &fixture.password, &destination, 0);
            assert_eq!(json["context"]["db_commit"], before.0);
            assert_eq!(json["data"]["conversation_id"], conversation);
            assert_eq!(json["data"]["bot_id"], *bot);
            assert_eq!(
                fs::read(fixture.root.join(&destination)).unwrap(),
                BODY.as_bytes()
            );
            let json = export(bot, &workspace_id, &fixture.password, &destination, 5);
            assert_eq!(json["error"]["code"], "ALREADY_EXISTS");
            assert_eq!(
                fs::read(fixture.root.join(&destination)).unwrap(),
                BODY.as_bytes()
            );
        }
        assert_eq!(fs::read(&path).unwrap(), content.as_bytes());
    }
    let folder = copy.join("conversations/bots").join(&active);
    let path = folder.join(format!("{conversation}.md"));
    let content = fs::read_to_string(&path).unwrap();
    let other_workspace = id(98);
    let different_id = id(99);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;

        let blocked = [
            folder.join("copy.md").to_str().unwrap().to_owned(),
            format!("copy/conversations/./bots/{retired}/../{active}/{different_id}.md"),
        ];
        for destination in blocked {
            let json = export(&active, &workspace_id, &fixture.password, &destination, 2);
            assert_eq!(json["error"]["code"], "INVALID_ARGUMENT");
            assert!(!fixture.root.join(&destination).exists());
        }

        let list = fixture.run_hive(
            &[
                "conversation",
                "list",
                "--workspace",
                "copy",
                "--workspace-id",
                &workspace_id,
                "--database",
                DATABASE,
                "--port",
                &port,
                "--bot",
                &active,
                "--contract",
                "2",
                "--json",
            ],
            &fixture.password,
        );
        assert!(
            list.status.success(),
            "{}",
            String::from_utf8_lossy(&list.stdout)
        );
        assert!(list.stderr.is_empty());
        let json: Value = serde_json::from_slice(&list.stdout).unwrap();
        let items = json["data"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["conversation_id"], conversation);

        let destination = "synthetic-stdout-failure.md";
        let full = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let output = command(&active, &workspace_id, &fixture.password, Some(destination))
            .stdout(Stdio::from(full))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "error: The conversation export was published, but its success response could not be written.\n"
        );
        let saved = fixture.root.join(destination);
        assert_eq!(fs::read(&saved).unwrap(), BODY.as_bytes());
        assert_eq!(
            fs::metadata(&saved).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let bad_headers = [
        content.replace("Last line", "Changed line"),
        content.replace(&workspace_id, &other_workspace),
        content.replace(&active, &retired),
        content.replace("bot_slug: fixture", "bot_slug: working-only"),
        content.replace(&conversation, &different_id),
        content.replace(
            "title: Synthetic title",
            "title: private-header-value\nunknown: private-header-value",
        ),
        content.replace("2026-11-27T09:00:00Z", "2026-02-30T09:00:00Z"),
        content.replacen("---\n", "--- \n", 1),
    ];
    for bad in bad_headers {
        fs::write(&path, bad).unwrap();
        assert_eq!(
            report(show(&active, &workspace_id, &fixture.password), 4)["error"]["code"],
            "INVALID_RECORD"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let destination = "failed-export.md";
        fs::write(&path, content.replace("Last line", "Changed line")).unwrap();
        let json = export(&active, &workspace_id, &fixture.password, destination, 4);
        assert_eq!(json["error"]["code"], "INVALID_RECORD");
        let json = export(&active, &other_workspace, &fixture.password, destination, 7);
        assert_eq!(json["error"]["code"], "AUTHORITY_UNVERIFIED");
        let json = export(
            &active,
            &workspace_id,
            "invalid-fixture-password",
            destination,
            6,
        );
        assert_eq!(json["error"]["code"], "UNAUTHORIZED");
        assert!(!fixture.root.join(destination).exists());
    }
    fs::write(&path, &content).unwrap();

    assert_eq!(
        report(show(&active, &other_workspace, &fixture.password), 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    fs::write(&marker, descriptor(&other_workspace)).unwrap();
    assert_eq!(
        report(show(&active, &other_workspace, &fixture.password), 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    fs::write(&marker, descriptor(&workspace_id)).unwrap();
    assert_eq!(
        report(show(&active, &workspace_id, "invalid-fixture-password"), 6)["error"]["code"],
        "UNAUTHORIZED"
    );
    assert_eq!(
        report(show(&different_id, &workspace_id, &fixture.password), 3)["error"]["code"],
        "NOT_FOUND"
    );

    let saved_file = folder.join("saved.md");
    fs::rename(&path, &saved_file).unwrap();
    std::os::unix::fs::symlink(&saved_file, &path).unwrap();
    assert_eq!(
        report(show(&active, &workspace_id, &fixture.password), 2)["error"]["code"],
        "INVALID_CONFIG"
    );
    #[cfg(target_os = "linux")]
    {
        let destination = "failed-export.md";
        let json = export(&active, &workspace_id, &fixture.password, destination, 2);
        assert_eq!(json["error"]["code"], "INVALID_CONFIG");
        assert!(!fixture.root.join(destination).exists());
    }
    fs::remove_file(&path).unwrap();
    fs::rename(&saved_file, &path).unwrap();
    let saved_folder = folder.with_file_name("saved");
    fs::rename(&folder, &saved_folder).unwrap();
    std::os::unix::fs::symlink(&saved_folder, &folder).unwrap();
    assert_eq!(
        report(show(&active, &workspace_id, &fixture.password), 2)["error"]["code"],
        "INVALID_CONFIG"
    );
    assert_eq!(fs::read(&path).unwrap(), content.as_bytes());
    assert_eq!(storage_state(&mut admin), before);
    assert!(!fixture.root.join("home/.local/state").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn conversation_list_pages_the_selected_queen_metadata_and_preserves_state() {
    use std::collections::BTreeMap;
    use std::path::Path;

    fn tree(path: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut result = BTreeMap::new();
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                result.insert(entry.path(), None);
                result.extend(tree(&entry.path()));
            } else {
                result.insert(entry.path(), Some(fs::read(entry.path()).unwrap()));
            }
        }
        result
    }

    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let workspace = id(1);
    let active = id(15);
    let retired = id(16);
    admin
        .exec_drop(
            "INSERT INTO bots (id, slug, display_name, revision, retired_at)
         VALUES (?, 'retired', 'Synthetic Queen', ?, UTC_TIMESTAMP(6))",
            (&retired, id(3)),
        )
        .unwrap();
    commit(&mut admin);
    admin
        .query_drop("UPDATE bots SET slug = 'working-only' WHERE slug = 'fixture'")
        .unwrap();
    let storage = storage_state(&mut admin);

    let root = fixture.root.join("copy");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("hive-workspace.yml"), descriptor(&workspace)).unwrap();
    let other_root = fixture.root.join("other-copy");
    fs::create_dir(&other_root).unwrap();
    fs::write(
        other_root.join("hive-workspace.yml"),
        descriptor(&workspace),
    )
    .unwrap();
    let port = fixture.port.to_string();
    let list = |copy: &str, bot: &str, tail: &[&str], status, code: Option<&str>| {
        let mut args = vec![
            "conversation",
            "list",
            "--workspace",
            copy,
            "--workspace-id",
            &workspace,
            "--database",
            DATABASE,
            "--port",
            &port,
            "--bot",
            bot,
            "--contract",
            "2",
            "--json",
        ];
        args.extend_from_slice(tail);
        let output = fixture.run_hive(&args, &fixture.password);
        let json: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(status), "{json}");
        assert!(output.stderr.is_empty());
        assert_eq!(json["schema_version"], 2);
        assert_eq!(json["command"], "conversation list");
        assert_eq!(json["ok"], code.is_none());
        let stdout = String::from_utf8(output.stdout).unwrap();
        for marker in [
            PRIVATE_BODY_MARKER,
            "private-header-value",
            "private-staging-target",
        ] {
            assert!(!stdout.contains(marker));
        }
        if let Some(code) = code {
            assert_eq!(json["error"]["code"], code);
            assert!(json.get("data").is_none());
            assert!(!stdout.contains("Synthetic title"));
        } else {
            assert_eq!(json["context"]["db_commit"], storage.0);
            assert!(
                json["data"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|item| item.get("body").is_none())
            );
        }
        json
    };
    let empty = list("copy", &retired, &[], 0, None);
    assert_eq!(empty["data"]["items"], serde_json::json!([]));
    assert_eq!(empty["data"]["next_cursor"], Value::Null);
    assert_eq!(empty["data"]["truncated"], false);

    let bots = root.join("conversations/bots");
    let folder = bots.join(&active);
    fs::create_dir_all(&folder).unwrap();
    let retired_folder = bots.join(&retired);
    fs::create_dir(&retired_folder).unwrap();
    let retired_id = id(99);
    fs::write(
        retired_folder.join(format!("{retired_id}.md")),
        published(&workspace, &retired, "retired", &retired_id),
    )
    .unwrap();
    let other_folder = other_root.join("conversations/bots").join(&active);
    fs::create_dir_all(&other_folder).unwrap();
    for n in 30..=55 {
        let conversation = id(n);
        let full = published(&workspace, &active, "fixture", &conversation);
        let mut bytes = full.as_bytes()[..full.len() - BODY.len()].to_vec();
        bytes.extend_from_slice(&[0xff, 0]);
        for dir in [&folder, &other_folder] {
            fs::write(dir.join(format!("{conversation}.md")), &bytes).unwrap();
        }
    }
    fs::write(folder.join(".tmp-staging.md"), "private-staging-target").unwrap();
    let files = tree(&root);
    let other_files = tree(&other_root);
    let first = list("copy", &active, &[], 0, None);
    let first_items = first["data"]["items"].as_array().unwrap();
    assert_eq!(first_items.len(), 25);
    for (item, n) in first_items.iter().zip((31..=55).rev()) {
        assert_eq!(item["conversation_id"], id(n));
        assert_eq!(item["bot_slug"], "fixture");
    }
    assert_eq!(first["data"]["truncated"], true);
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    let next = list("copy", &active, &["--cursor", cursor], 0, None);
    assert_eq!(next["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(next["data"]["items"][0]["conversation_id"], id(30));
    assert_eq!(
        next["data"]["observed_revision"],
        first["data"]["observed_revision"]
    );
    assert_eq!(next["data"]["next_cursor"], Value::Null);
    assert_eq!(next["data"]["truncated"], false);
    let retired_page = list("copy", &retired, &[], 0, None);
    assert_eq!(
        retired_page["data"]["items"][0]["conversation_id"],
        retired_id
    );
    assert_eq!(retired_page["data"]["items"].as_array().unwrap().len(), 1);
    list(
        "other-copy",
        &active,
        &["--cursor", cursor],
        5,
        Some("STALE_CURSOR"),
    );
    list(
        "copy",
        &retired,
        &["--cursor", cursor],
        5,
        Some("STALE_CURSOR"),
    );
    list(
        "copy",
        &active,
        &["--limit", "100", "--cursor", cursor],
        5,
        Some("STALE_CURSOR"),
    );
    list(
        "copy",
        &active,
        &["--cursor", "private-header-value"],
        5,
        Some("STALE_CURSOR"),
    );
    assert_eq!(tree(&root), files);
    assert_eq!(tree(&other_root), other_files);

    let added = id(56);
    let added_path = folder.join(format!("{added}.md"));
    fs::write(
        &added_path,
        published(&workspace, &active, "fixture", &added),
    )
    .unwrap();
    list(
        "copy",
        &active,
        &["--cursor", cursor],
        5,
        Some("STALE_CURSOR"),
    );
    fs::remove_file(&added_path).unwrap();
    let last_path = folder.join(format!("{}.md", id(30)));
    let previous = fs::read(&last_path).unwrap();
    let edited = published(&workspace, &active, "fixture", &id(30))
        .replace("Synthetic title", "Changed synthetic title");
    fs::write(&last_path, &edited).unwrap();
    list(
        "copy",
        &active,
        &["--cursor", cursor],
        5,
        Some("STALE_CURSOR"),
    );
    fs::write(
        &last_path,
        edited.replace("title:", "private-header-value:"),
    )
    .unwrap();
    list(
        "copy",
        &active,
        &["--limit", "1"],
        4,
        Some("INVALID_RECORD"),
    );
    fs::write(&last_path, previous).unwrap();

    std::os::unix::fs::symlink(&last_path, &added_path).unwrap();
    list("copy", &active, &[], 2, Some("INVALID_CONFIG"));
    assert!(
        fs::symlink_metadata(&added_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&added_path).unwrap();
    let hidden = folder.with_file_name("hidden-directory");
    fs::rename(&folder, &hidden).unwrap();
    std::os::unix::fs::symlink(&hidden, &folder).unwrap();
    list("copy", &active, &[], 2, Some("INVALID_CONFIG"));
    fs::remove_file(&folder).unwrap();
    fs::rename(&hidden, &folder).unwrap();
    for n in 0..4096 {
        fs::write(retired_folder.join(format!(".tmp-{n}")), "").unwrap();
    }
    let with_staging = tree(&root);
    list("copy", &retired, &[], 2, Some("CONTEXT_LIMIT"));
    assert_eq!(tree(&root), with_staging);
    for n in 0..4096 {
        fs::remove_file(retired_folder.join(format!(".tmp-{n}"))).unwrap();
    }
    assert_eq!(tree(&root), files);
    assert_eq!(storage_state(&mut admin), storage);
    assert!(!fixture.root.join("home/.local/state").exists());
}

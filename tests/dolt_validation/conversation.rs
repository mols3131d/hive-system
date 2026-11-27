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
fn conversation_show_reads_only_the_explicit_verified_copy() {
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
    let show = |bot: &str, expected_workspace: &str, password: &str| {
        fixture.run_hive(
            &[
                "conversation",
                "show",
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
            ],
            password,
        )
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
        assert_eq!(fs::read(&path).unwrap(), content.as_bytes());
    }
    let folder = copy.join("conversations/bots").join(&active);
    let path = folder.join(format!("{conversation}.md"));
    let content = fs::read_to_string(&path).unwrap();
    let other_workspace = id(98);
    let different_id = id(99);
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

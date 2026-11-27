use super::*;
use serde_json::json;

const ACTOR: &str = "fixture-queen-registrar";
const NAME: &str = "Synthetic Queen";
const SOUL: &str = "synthetic private Soul body";

#[test]
#[ignore = "requires pinned Dolt and Git, local TCP and Linux pidfds"]
fn bot_register_checks_soul_and_confirms_replays_and_recovers() {
    let listener = TcpListener::bind((HOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let workspace_id = id(1);
    let retired = id(3);
    let fixture = Fixture::initialize(&format!(
        "{}\nINSERT INTO bots VALUES ('{retired}', 'retired', 'Retired', '{retired}', '2026-01-01');",
        empty_seed_sql(CASE_INSENSITIVE),
    ));
    let workspace = fixture.root.join("workspace");
    let state_home = fixture.root.join("state");
    let state = state_home.join("hive/v2/workspaces").join(&workspace_id);
    register_fixture(&fixture, &workspace, &state);
    let assets = workspace.join("assets");
    fs::create_dir(&assets).unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .env_clear()
            .env("PATH", &path)
            .env("HOME", fixture.root.join("home"))
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
        assert!(out.status.success(), "{:?}", out.stderr);
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let write_soul = |slug: &str, bytes: &[u8]| {
        let dir = assets.join("bots").join(slug);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SOUL.md"), bytes).unwrap();
    };
    git(&["init", "-q"]);
    git(&["commit", "--allow-empty", "-qm", "Initial fixture assets"]);
    let old = git(&["rev-parse", "HEAD"]);
    let alpha = format!(
        "---\nworkspace_id: {workspace_id}\nslug: alpha\ndisplay_name: {NAME}\nrole: queen\n---\n{SOUL}\n"
    );
    write_soul("alpha", alpha.as_bytes());
    write_soul("beta", SOUL.as_bytes());
    write_soul("retired", SOUL.as_bytes());
    write_soul("malformed", b"---\nrole: queen\n");
    write_soul("mismatch", b"---\nrole: drone\n---\nSynthetic\n");
    write_soul("bad-utf8", &[0xff]);
    write_soul("oversize", &vec![b'a'; 128 * 1024 + 1]);
    fs::create_dir_all(assets.join("bots/linked")).unwrap();
    std::os::unix::fs::symlink("../alpha/SOUL.md", assets.join("bots/linked/SOUL.md")).unwrap();
    fs::write(assets.join("bots/alpha/README.md"), [0xff]).unwrap();
    fs::write(
        assets.join("bots/alpha/AGENTS.md"),
        b"synthetic management text",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "Reviewed fixture assets"]);
    let asset = git(&["rev-parse", "HEAD"]);
    let index = fs::read(assets.join(".git/index")).unwrap();

    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .env_clear()
            .env("PATH", &path)
            .env("HOME", fixture.root.join("home"))
            .env("TMPDIR", &fixture.root)
            .env("XDG_STATE_HOME", &state_home)
            .current_dir(&fixture.root)
            .args(["--contract", "2", "--json", "--workspace"])
            .arg(&workspace)
            .args(args)
            .output()
            .unwrap()
    };
    let run = |args: &[&str], exit| {
        let out = invoke(args);
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(out.status.code(), Some(exit), "{report}");
        let response = report.to_string() + &String::from_utf8_lossy(&out.stderr);
        for private in [
            SOUL,
            "synthetic management text",
            "fixture-registration-approval",
        ] {
            assert!(!response.contains(private));
        }
        report
    };
    drop(listener);
    let dolt = fixture.dolt.to_str().unwrap();
    let start = ["workspace", "start", "--dolt-bin", dolt, "--port", &port];
    run(&start, 0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let evidence_path = fixture.root.join("register-evidence.json");
        let register = |request: &str, slug: &str, revision: &str, case: &str, exit| {
            let fp = hash(
                json!([1, "bot.register", workspace_id, ACTOR, slug, NAME, revision])
                    .to_string()
                    .as_bytes(),
            );
            let now = fixture_now();
            let mut evidence = json!({
                "version": 1, "operation": "bot.register", "workspace_id": workspace_id,
                "request_id": request, "fingerprint": fp, "actor_ref": ACTOR,
                "asset_revision": revision, "authorization_ref": "fixture-registration-approval",
                "evidence_id": "fixture-evidence", "revision": "r1",
                "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
                "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
                "withdrawn": false,
            });
            match case {
                "fingerprint" => evidence["fingerprint"] = json!("0".repeat(64)),
                "actor" => evidence["actor_ref"] = json!("different-actor"),
                "expired" => evidence["expires_at"] = json!(fixture_timestamp(now)),
                "asset" => evidence["asset_revision"] = json!(old),
                _ => {}
            }
            fs::write(
                &evidence_path,
                json!({"decision": "allow", "evidence": evidence}).to_string(),
            )
            .unwrap();
            let mut args = vec![
                "bot",
                "register",
                "--workspace-id",
                &workspace_id,
                "--database",
                "db",
                "--port",
                &port,
                "--slug",
                slug,
                "--name",
                NAME,
                "--asset-revision",
                revision,
                "--actor",
                ACTOR,
                "--request-id",
                request,
                "--evidence-file",
                evidence_path.to_str().unwrap(),
            ];
            if case == "role" {
                args.extend(["--role", "drone"]);
            }
            run(&args, exit)
        };

        let mut reader = connect_owned(&state, &port, "reader");
        let before = storage_state(&mut reader);
        let marker = state.join("pending-request.json");
        let first = id(50);
        let second = id(51);
        let failed = id(52);
        for case in ["fingerprint", "actor", "expired", "asset"] {
            assert_eq!(
                register(&failed, "alpha", &asset, case, 7)["error"]["code"],
                "AUTHORITY_UNVERIFIED"
            );
        }
        assert_eq!(
            register(&failed, "Alpha", &asset, "", 2)["error"]["code"],
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            register(&failed, "alpha", &asset, "role", 2)["error"]["code"],
            "INVALID_ARGUMENT"
        );
        for (slug, exit, expected) in [
            ("missing", 3, "NOT_FOUND"),
            ("malformed", 4, "INVALID_RECORD"),
            ("mismatch", 4, "INVALID_RECORD"),
            ("bad-utf8", 4, "INVALID_RECORD"),
            ("oversize", 2, "INVALID_CONFIG"),
            ("linked", 2, "INVALID_CONFIG"),
            ("retired", 5, "ALREADY_EXISTS"),
        ] {
            assert_eq!(
                register(&failed, slug, &asset, "", exit)["error"]["code"],
                expected
            );
        }
        assert_eq!(
            register(&failed, "alpha", &old, "", 5)["error"]["code"],
            "STALE_REVISION"
        );
        write_soul("alpha", b"Dirty fixture");
        assert_eq!(
            register(&failed, "alpha", &asset, "", 5)["error"]["code"],
            "DIRTY_CONFLICT"
        );
        write_soul("alpha", alpha.as_bytes());
        assert_eq!(storage_state(&mut reader), before);
        assert!(!marker.exists());

        let first = (&first, register(&first, "alpha", &asset, "", 0));
        let second = (&second, register(&second, "beta", &asset, "", 0));
        for (slug, (_, report)) in [("alpha", &first), ("beta", &second)] {
            assert_eq!(report["command"], "bot register");
            assert_eq!(report["data"]["outcome"], "confirmed");
            let result = &report["data"]["result"];
            let bot = &result["bot"];
            assert_eq!(result["asset_revision"], asset);
            assert_eq!(bot["slug"], slug);
            assert_eq!(bot["role"], "queen");
            assert_eq!(bot["display_name"], NAME);
            assert_eq!(bot["status"], "active");
            assert_eq!(bot["id"], result["bot_id"]);
            assert_eq!(bot["revision"], result["bot_revision"]);
            for field in ["id", "revision"] {
                let value = bot[field].as_str().unwrap();
                assert_eq!(value.len(), 36);
                assert_eq!(&value[14..15], "7");
            }
        }
        assert_ne!(
            first.1["data"]["result"]["bot_id"],
            second.1["data"]["result"]["bot_id"]
        );
        let head = storage_state(&mut reader);
        assert_eq!(head.0, second.1["data"]["confirmed_commit"]);
        assert_eq!(
            reader
                .query_first::<u64, _>("SELECT COUNT(*) FROM bots")
                .unwrap(),
            Some(3)
        );
        assert_eq!(
            reader
                .query_first::<u64, _>("SELECT COUNT(*) FROM operation_receipts")
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            register(&failed, "alpha", &asset, "", 5)["error"]["code"],
            "ALREADY_EXISTS"
        );
        assert_eq!(
            register(first.0, "alpha", &old, "", 5)["error"]["code"],
            "REQUEST_CONFLICT"
        );

        // The selected Git tree is still byte-for-byte unmodified by Hive.
        assert_eq!(git(&["rev-parse", "HEAD"]), asset);
        assert_eq!(fs::read(assets.join(".git/index")).unwrap(), index);
        assert!(git(&["--no-optional-locks", "status", "--porcelain"]).is_empty());

        let mut writer = connect_owned(&state, &port, "writer");
        let grants: Vec<String> = writer.query("SELECT PRIVILEGE_TYPE FROM information_schema.TABLE_PRIVILEGES WHERE TABLE_SCHEMA='db' AND TABLE_NAME='bots'").unwrap();
        assert_eq!(grants, ["INSERT"]);
        let session = writer
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        drop(writer);
        let receipt: (String, String, String, String, String, String, String, String) = reader.exec_first(
            "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                    DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
             FROM operation_receipts WHERE request_id = ?",
            (second.0,),
        ).unwrap().unwrap();
        for payload in [&receipt.6, &receipt.7] {
            assert!(!payload.contains(SOUL));
            let json: Value = serde_json::from_str(payload).unwrap();
            assert_eq!(json["asset_revision"], asset);
        }
        drop(reader);

        // Historical verification works even when the Git repository is unavailable.
        fs::rename(&assets, workspace.join("assets-unavailable")).unwrap();
        let replay = register(first.0, "alpha", &asset, "", 0);
        assert_eq!(replay["data"]["outcome"], "replayed");
        assert_eq!(replay["data"]["result"], first.1["data"]["result"]);
        assert_eq!(replay["context"]["db_commit"], head.0);
        assert_eq!(
            replay["data"]["confirmed_commit"],
            first.1["data"]["confirmed_commit"]
        );
        assert_eq!(
            register(first.0, "alpha", &asset, "expired", 7)["error"]["code"],
            "AUTHORITY_UNVERIFIED"
        );

        let mut intent = json!({
            "version": 4, "kind": "bot.register", "workspace_id": workspace_id,
            "launch_nonce": launch_nonce(&state), "session_id": session, "request_id": second.0,
            "base_commit": receipt.4, "fingerprint": receipt.2,
            "bot_id": second.1["data"]["result"]["bot_id"],
            "receipt_digest": hash(json!(receipt).to_string().as_bytes()),
        });
        let record = |intent: &Value| {
            fs::write(&marker, intent.to_string()).unwrap();
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        };
        record(&intent);
        assert_eq!(
            register(second.0, "beta", &asset, "", 0)["data"]["outcome"],
            "replayed"
        );
        assert!(!marker.exists());

        let stop_and_recover = |outcome| {
            assert_eq!(
                run(&["workspace", "stop"], 0)["data"]["pending_request"],
                "recorded"
            );
            assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
            assert_eq!(
                run(&["workspace", "recover", "--dolt-bin", dolt], 0)["data"]["outcome"],
                outcome
            );
            assert!(!marker.exists() && !state.join("stopped-request.json").exists());
            run(&start, 0);
            assert_eq!(
                storage_state(&mut connect_owned(&state, &port, "reader")),
                head
            );
        };
        record(&intent);
        stop_and_recover("confirmed");

        let mut prior = connect_owned(&state, &port, "writer");
        intent["launch_nonce"] = json!(launch_nonce(&state));
        intent["session_id"] = json!(
            prior
                .query_first::<u64, _>("SELECT CONNECTION_ID()")
                .unwrap()
                .unwrap()
        );
        intent["request_id"] = json!(failed);
        intent["base_commit"] = json!(head.0);
        intent["bot_id"] = json!(id(53));
        prior.query_drop("START TRANSACTION").unwrap();
        prior
            .exec_drop(
                "INSERT INTO bots VALUES (?, 'rolled-back', 'Fixture', ?, NULL)",
                (id(53), id(54)),
            )
            .unwrap();
        record(&intent);
        assert_eq!(
            register(first.0, "alpha", &asset, "", 8)["error"]["code"],
            "COMMIT_UNCERTAIN"
        );
        drop(prior);
        stop_and_recover("rolled_back");
        assert_eq!(
            connect_owned(&state, &port, "reader")
                .query_first::<u64, _>("SELECT COUNT(*) FROM bots")
                .unwrap(),
            Some(3)
        );
    }));
    if state.join("server.json").exists() {
        let stop = invoke(&["workspace", "stop"]);
        if !stop.status.success() {
            let path = fixture.root.display().to_string();
            std::mem::forget(fixture);
            panic!("Verified fixture stop failed; preserved the disposable store at {path}");
        }
    }
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

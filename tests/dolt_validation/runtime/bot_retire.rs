use super::*;
use serde_json::json;

#[test]
#[ignore = "requires pinned Dolt and Git, local TCP, Linux pidfds and ext4 outside Git"]
fn bot_retire_preserves_both_conversations_and_the_queen_asset() {
    let fixture = Fixture::initialize(&seed_sql(CASE_INSENSITIVE));
    let workspace_id = id(1);
    let bot = id(15);
    let revision = id(4);
    let workspace = fixture.root.join("workspace");
    let state_home = fixture.root.join("state");
    let state = state_home.join("hive/v2/workspaces").join(&workspace_id);
    register_fixture(&fixture, &workspace, &state);
    let assets = workspace.join("assets");
    let soul = assets.join("bots/fixture/SOUL.md");
    let soul_bytes = b"# Synthetic Queen\r\nPortable purpose.\r\n";
    fs::create_dir_all(soul.parent().unwrap()).unwrap();
    fs::write(&soul, soul_bytes).unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
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
        assert!(output.status.success(), "{:?}", output.stderr);
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "Synthetic Queen asset"]);
    let asset_head = git(&["rev-parse", "HEAD"]);
    let asset_index = fs::read(assets.join(".git/index")).unwrap();

    let listener = TcpListener::bind((HOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    let invoke = |args: &[&str], read: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hive"));
        command
            .env_clear()
            .env("PATH", &path)
            .env("HOME", fixture.root.join("home"))
            .env("TMPDIR", &fixture.root)
            .env("XDG_STATE_HOME", &state_home)
            .current_dir(&fixture.root)
            .args(["--contract", "2", "--json", "--workspace"])
            .arg(&workspace)
            .args(args);
        if read {
            let nonce = launch_nonce(&state);
            command
                .env(
                    "HIVE_DOLT_USER",
                    format!("reader_{}", &nonce.replace('-', "")[..24]),
                )
                .env(
                    "HIVE_DOLT_PASSWORD",
                    fs::read_to_string(state.join(format!("run-{nonce}/reader-password"))).unwrap(),
                );
        }
        command.output().unwrap()
    };
    let run = |args: &[&str], read, exit| {
        let output = invoke(args, read);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert!(output.stderr.is_empty(), "{report}");
        report
    };
    let dolt = fixture.dolt.to_str().unwrap();
    let start = ["workspace", "start", "--dolt-bin", dolt, "--port", &port];
    let stop = ["workspace", "stop"];
    let target = [
        "--workspace-id",
        &workspace_id,
        "--database",
        "db",
        "--port",
        &port,
    ];
    let read = |args: &[&str]| run(&[args, &target].concat(), true, 0);
    let head = || storage_state(&mut connect_owned(&state, &port, "reader"));
    let offline_sql = |sql: &str| fixture.local_at(&workspace.join("db"), &["sql"], Some(sql));

    let request = id(50);
    let other_request = id(51);
    let evidence_file = fixture.root.join("retire-evidence.json");
    let now = fixture_now();
    let retire = |request: &str, expected: &str, commit: &str, case: &str, exit| {
        let actor = "fixture-actor";
        let source = "fixture-execution-check";
        let observed = fixture_timestamp(now - time::Duration::minutes(2));
        let mut evidence = json!({
            "version": 1, "operation": "bot.retire", "workspace_id": workspace_id,
            "request_id": request, "actor_ref": actor,
            "expected_bot_revision": expected,
            "execution": {"bot_id": bot, "db_commit": commit, "reconciled": true,
                "source_ref": source, "observed_at": observed},
            "authorization_ref": "fixture-approval", "evidence_id": "fixture-evidence",
            "revision": "1", "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
            "reviewer_ref": "fixture-reviewer",
            "reviewed_at": fixture_timestamp(now - time::Duration::minutes(1)),
            "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
            "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
            "withdrawn": false,
        });
        match case {
            "missing" => {
                evidence.as_object_mut().unwrap().remove("execution");
            }
            "false" => evidence["execution"]["reconciled"] = json!(false),
            "other-bot" => evidence["execution"]["bot_id"] = json!(id(16)),
            "empty-source" => evidence["execution"]["source_ref"] = json!(""),
            "future" | "old" => {
                let epoch = if case == "future" {
                    now + time::Duration::minutes(1)
                } else {
                    now - time::Duration::hours(2)
                };
                evidence["execution"]["observed_at"] = json!(fixture_timestamp(epoch));
            }
            "expired" => evidence["expires_at"] = json!(fixture_timestamp(now)),
            _ => {}
        }
        let proof = &evidence["execution"];
        let fp = if case == "fingerprint" {
            "0".repeat(64)
        } else {
            hash(
                json!([
                    1,
                    "bot.retire",
                    workspace_id,
                    actor,
                    bot,
                    expected,
                    commit,
                    proof["reconciled"],
                    proof["source_ref"],
                    proof["observed_at"]
                ])
                .to_string()
                .as_bytes(),
            )
        };
        evidence["fingerprint"] = json!(fp);
        fs::write(
            &evidence_file,
            json!({"decision":"allow", "evidence": evidence}).to_string(),
        )
        .unwrap();
        run(
            &[
                &[
                    "bot",
                    "retire",
                    &bot,
                    "--if-match",
                    expected,
                    "--actor",
                    actor,
                    "--request-id",
                    request,
                    "--evidence-file",
                    evidence_file.to_str().unwrap(),
                ][..],
                &target,
            ]
            .concat(),
            false,
            exit,
        )
    };

    let checks = std::panic::catch_unwind(|| {
        run(&start, false, 0);
        let active = read(&["bot", "show", &bot, "--asset-commit", &asset_head]);
        assert_eq!(active["data"]["bot"]["id"], bot);
        assert_eq!(active["data"]["bot"]["revision"], revision);
        assert_eq!(active["data"]["bot"]["status"], "active");

        // The committed holder is opaque even though it does not resemble the Queen UUID.
        let claimed = head();
        assert_eq!(
            retire(&request, &revision, &claimed.0, "", 5)["error"]["code"],
            "ALREADY_CLAIMED"
        );
        assert_eq!(head(), claimed);
        assert!(!state.join("pending-request.json").exists());
        run(&stop, false, 0);
        offline_sql("DELETE FROM issue_claims; UPDATE issues SET status = 'todo' WHERE status = 'in-progress';
                     CALL DOLT_COMMIT('-am', 'Quiesce synthetic fixture');");
        offline_sql(&format!(
            "INSERT INTO issue_claims VALUES ('{}', '{}', '{bot}', UTC_TIMESTAMP(6));",
            id(11),
            id(14)
        ));
        run(&start, false, 0);
        let dirty = head();
        assert_eq!(
            retire(&request, &revision, &dirty.0, "", 5)["error"]["code"],
            "DIRTY_CONFLICT"
        );
        assert_eq!(head(), dirty);
        assert!(!state.join("pending-request.json").exists());
        run(&stop, false, 0);
        offline_sql("CALL DOLT_RESET('--hard');");
        run(&start, false, 0);

        let before = head();
        for case in [
            "fingerprint",
            "missing",
            "false",
            "other-bot",
            "empty-source",
            "old",
            "future",
            "expired",
        ] {
            assert_eq!(
                retire(&request, &revision, &before.0, case, 7)["error"]["code"],
                "AUTHORITY_UNVERIFIED",
                "{case}"
            );
        }
        for (expected, commit) in [(&revision, &claimed.0), (&id(19), &before.0)] {
            assert_eq!(
                retire(&request, expected, commit, "", 5)["error"]["code"],
                "STALE_REVISION"
            );
        }
        assert_eq!(head(), before);
        assert!(!state.join("pending-request.json").exists());

        let bodies = [
            "# Before α\r\nExact bytes.",
            "# Before β\nDifferent snapshot.\n",
        ];
        let input = fixture.root.join("fixture.md");
        let import = |body: &str, exit| {
            fs::write(&input, body).unwrap();
            run(
                &[
                    &[
                        "conversation",
                        "import",
                        "--bot",
                        &bot,
                        "--session-ref",
                        "fixture-session",
                        "--file",
                        "fixture.md",
                    ][..],
                    &target,
                ]
                .concat(),
                false,
                exit,
            )
        };
        let snapshots = bodies.map(|body| {
            import(body, 0)["data"]["conversation_id"]
                .as_str()
                .unwrap()
                .to_owned()
        });
        assert_ne!(snapshots[0], snapshots[1]);

        let retired = retire(&request, &revision, &before.0, "", 0);
        assert_eq!(retired["data"]["outcome"], "confirmed");
        assert_eq!(retired["data"]["base_commit"], before.0);
        let result = &retired["data"]["result"];
        assert_eq!(result["bot"]["id"], bot);
        assert_eq!(result["bot"]["status"], "retired");
        assert_ne!(result["bot_revision"], revision);
        assert_eq!(
            import("fresh synthetic input", 3)["error"]["code"],
            "NOT_FOUND"
        );
        assert_eq!(
            retire(&other_request, &revision, &head().0, "", 3)["error"]["code"],
            "NOT_FOUND"
        );
        let replay = retire(&request, &revision, &before.0, "", 0);
        assert_eq!(replay["data"]["outcome"], "replayed");
        assert_eq!(replay["data"]["result"], *result);
        assert_eq!(
            replay["data"]["confirmed_commit"],
            retired["data"]["confirmed_commit"]
        );
        assert_eq!(
            retire(&request, &id(19), &before.0, "", 5)["error"]["code"],
            "REQUEST_CONFLICT"
        );
        assert_eq!(
            retire(&request, &revision, &before.0, "expired", 7)["error"]["code"],
            "AUTHORITY_UNVERIFIED"
        );

        let listed = read(&["conversation", "list", "--bot", &bot]);
        let items = listed["data"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        for (index, (conversation, body)) in snapshots.iter().zip(bodies).enumerate() {
            assert!(
                items
                    .iter()
                    .any(|item| item["conversation_id"] == *conversation
                        && item["provenance"]["session_ref"] == "fixture-session")
            );
            let shown = read(&["conversation", "show", "--bot", &bot, "--id", conversation]);
            assert_eq!(
                shown["data"]["body"].as_str().unwrap().as_bytes(),
                body.as_bytes()
            );
            let destination = format!("export-{index}.md");
            read(&[
                "conversation",
                "export",
                "--bot",
                &bot,
                "--id",
                conversation,
                "--destination",
                &destination,
            ]);
            assert_eq!(
                fs::read(fixture.root.join(destination)).unwrap(),
                body.as_bytes()
            );
        }
        let inspected = read(&["bot", "show", &bot, "--asset-commit", &asset_head]);
        assert_eq!(inspected["data"]["bot"], result["bot"]);
        assert_eq!(git(&["rev-parse", "HEAD"]), asset_head);
        assert_eq!(fs::read(assets.join(".git/index")).unwrap(), asset_index);
        assert_eq!(fs::read(&soul).unwrap(), soul_bytes);
        assert!(git(&["--no-optional-locks", "status", "--porcelain"]).is_empty());

        // Recreate the persisted marker for a lost acknowledgement, then
        // exercise the real stopped recovery path against the committed effect.
        let reader = &mut connect_owned(&state, &port, "reader");
        let receipt: (String, String, String, String, String, String, String, String) = reader.exec_first(
            "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                    DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
             FROM operation_receipts WHERE request_id = ?", (&request,)
        ).unwrap().unwrap();
        assert_eq!(
            reader
                .query_first::<u64, _>("SELECT COUNT(*) FROM operation_receipts")
                .unwrap(),
            Some(1)
        );
        let session = reader
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        let marker = state.join("pending-request.json");
        let mut intent = json!({
            "version": 4, "kind": "bot.retire", "workspace_id": workspace_id,
            "launch_nonce": launch_nonce(&state), "session_id": session, "request_id": request,
            "base_commit": receipt.4, "fingerprint": receipt.2, "bot_id": bot,
            "receipt_digest": hash(json!(receipt).to_string().as_bytes()),
        });
        let confirmed = head();
        let recover = |intent: &Value, expected| {
            fs::write(&marker, intent.to_string()).unwrap();
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(run(&stop, false, 0)["data"]["pending_request"], "recorded");
            assert_eq!(
                run(&start, false, 7)["error"]["code"],
                "AUTHORITY_UNVERIFIED"
            );
            assert_eq!(
                run(&["workspace", "recover", "--dolt-bin", dolt], false, 0)["data"]["outcome"],
                expected
            );
            assert!(!marker.exists() && !state.join("stopped-request.json").exists());
            run(&start, false, 0);
            assert_eq!(head(), confirmed);
        };
        recover(&intent, "confirmed");
        let mut writer = connect_owned(&state, &port, "writer");
        let session = writer
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        writer.query_drop("START TRANSACTION").unwrap();
        writer
            .exec_drop("UPDATE bots SET revision = ? WHERE id = ?", (id(80), &bot))
            .unwrap();
        intent["launch_nonce"] = json!(launch_nonce(&state));
        intent["session_id"] = json!(session);
        intent["request_id"] = json!(other_request);
        intent["base_commit"] = json!(confirmed.0);
        recover(&intent, "rolled_back");
    });
    if state.join("server.json").exists() {
        let output = invoke(&stop, false);
        if !output.status.success() {
            let root = fixture.root.clone();
            std::mem::forget(fixture);
            panic!("Cleanup of {} was uncertain: {output:?}", root.display());
        }
    }
    checks.unwrap_or_else(|error| std::panic::resume_unwind(error));
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

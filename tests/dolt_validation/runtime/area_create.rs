use super::*;
use serde_json::json;

const ACTOR: &str = "fixture-area-author";
const NAME: &str = "Synthetic Área";

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP and Linux pidfds"]
fn area_create_guards_root_and_child_and_recovers() {
    let listener = TcpListener::bind((HOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let workspace_id = id(1);
    let archived = id(2);
    let below_archived = id(3);
    let old_revision = "fixture-r1";
    let fixture = Fixture::initialize(&format!(
        "{}\nINSERT INTO areas (id, parent_id, slug, name, revision, archived_at) VALUES
         ('{archived}', NULL, 'archived', 'Archived', '{old_revision}', UTC_TIMESTAMP(6)),
         ('{below_archived}', '{archived}', 'active-under-archived', 'Active', '{old_revision}', NULL);",
        empty_seed_sql(CASE_INSENSITIVE),
    ));
    let workspace = fixture.root.join("workspace");
    let state_home = fixture.root.join("state");
    let state = state_home.join("hive/v2/workspaces").join(&workspace_id);
    register_fixture(&fixture, &workspace, &state);
    drop(listener);

    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .env_clear()
            .env("HOME", fixture.root.join("home"))
            .env("XDG_STATE_HOME", &state_home)
            .current_dir(&fixture.root)
            .args(["--contract", "2", "--json", "--workspace"])
            .arg(&workspace)
            .args(args)
            .output()
            .unwrap()
    };
    let run = |args: &[&str], exit| {
        let output = invoke(args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "Invalid CLI response: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert!(!report.to_string().contains("fixture-area-authorization"));
        report
    };
    let dolt = fixture.dolt.to_str().unwrap();
    let start = ["workspace", "start", "--dolt-bin", dolt, "--port", &port];
    run(&start, 0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let evidence_path = fixture.root.join("area-evidence.json");
        let create =
            |request: &str, scope: Option<(&str, &str)>, slug: &str, explicit_root: bool, exit| {
                let (parent, expected) = scope.unzip();
                let fingerprint = hash(
                    &json!([
                        1,
                        "area.create",
                        workspace_id,
                        parent,
                        ACTOR,
                        expected,
                        slug,
                        NAME
                    ])
                    .to_string(),
                );
                let now = fixture_now();
                let mut decision = json!({
                    "decision": "allow",
                    "evidence": {
                        "version": 1, "operation": "area.create", "workspace_id": workspace_id,
                        "request_id": request, "fingerprint": fingerprint, "actor_ref": ACTOR,
                        "parent_id": parent, "expected_parent_revision": expected,
                        "authorization_ref": "fixture-area-authorization",
                        "evidence_id": "fixture-area-evidence", "revision": "r1",
                        "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                        "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
                        "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
                        "withdrawn": false,
                    }
                });
                if !explicit_root {
                    decision["evidence"]
                        .as_object_mut()
                        .unwrap()
                        .remove("parent_id");
                }
                fs::write(&evidence_path, decision.to_string()).unwrap();
                let mut args = vec![
                    "area",
                    "create",
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
                    "--actor",
                    ACTOR,
                    "--request-id",
                    request,
                    "--evidence-file",
                    evidence_path.to_str().unwrap(),
                ];
                let if_match;
                if let Some((parent, revision)) = scope {
                    if_match = format!("{parent}={revision}");
                    args.extend(["--parent", parent, "--if-match", &if_match]);
                }
                let report = run(&args, exit);
                assert_eq!(report["command"], "area create");
                report
            };

        let mut reader = connect_owned(&state, &port, "reader");
        let before = storage_state(&mut reader);
        assert_eq!(
            create(&id(50), None, "shared", false, 7)["error"]["code"],
            "AUTHORITY_UNVERIFIED"
        );
        assert_eq!(storage_state(&mut reader), before);

        let root_request = id(50);
        let root = create(&root_request, None, "shared", true, 0);
        let root_id = root["data"]["result"]["area_id"].as_str().unwrap();
        let root_revision = root["data"]["result"]["area_revision"].as_str().unwrap();
        assert_eq!(root["data"]["outcome"], "confirmed");
        assert_eq!(root["data"]["base_commit"], before.0);
        assert_eq!(root["data"]["result"]["parent_id"], Value::Null);
        assert_eq!(root["data"]["result"]["parent_revision"], Value::Null);
        assert_eq!(root["context"]["scope"]["area_id"], root_id);
        let root_commit = storage_state(&mut reader);
        assert_eq!(root_commit.0, root["data"]["confirmed_commit"]);

        let child_request = id(51);
        let scope = Some((root_id, root_revision));
        // The same slug can be used at the root and under a distinct parent.
        let child = create(&child_request, scope, "shared", true, 0);
        let child_id = child["data"]["result"]["area_id"].as_str().unwrap();
        let child_revision = child["data"]["result"]["area_revision"].as_str().unwrap();
        let parent_revision = child["data"]["result"]["parent_revision"].as_str().unwrap();
        assert_ne!(root_id, child_id);
        assert_ne!(root_revision, parent_revision);
        assert_eq!(child["data"]["outcome"], "confirmed");
        assert_eq!(child["data"]["base_commit"], root_commit.0);
        assert_eq!(child["data"]["result"]["parent_id"], root_id);
        assert_eq!(
            child["context"]["scope"],
            json!({"area_id": child_id, "parent_id": root_id})
        );
        let committed = storage_state(&mut reader);
        assert_eq!(committed.0, child["data"]["confirmed_commit"]);

        let select = |reader: &mut Conn, area| {
            reader.exec_first::<(Option<String>, String, String, String, bool), _, _>(
                "SELECT parent_id, slug, name, revision, (description IS NULL AND archived_at IS NULL)
                 FROM areas WHERE id = ?", (area,),
            ).unwrap().unwrap()
        };
        assert_eq!(
            select(&mut reader, root_id),
            (
                None,
                "shared".into(),
                NAME.into(),
                parent_revision.into(),
                true
            )
        );
        assert_eq!(
            select(&mut reader, child_id),
            (
                Some(root_id.to_owned()),
                "shared".into(),
                NAME.into(),
                child_revision.into(),
                true
            )
        );
        assert_eq!(
            reader
                .query_first::<u64, _>("SELECT COUNT(*) FROM areas")
                .unwrap(),
            Some(4)
        );
        assert_eq!(
            reader
                .query_first::<u64, _>("SELECT COUNT(*) FROM operation_receipts")
                .unwrap(),
            Some(2)
        );

        for (report, code) in [
            (create(&id(52), None, "shared", true, 5), "ALREADY_EXISTS"),
            (create(&id(52), None, "archived", true, 5), "ALREADY_EXISTS"),
            (
                create(&id(52), Some((root_id, parent_revision)), "shared", true, 5),
                "ALREADY_EXISTS",
            ),
            (create(&id(52), scope, "other", true, 5), "STALE_REVISION"),
            (
                create(&id(52), Some((&archived, old_revision)), "other", true, 4),
                "INVALID_TRANSITION",
            ),
            (
                create(
                    &id(52),
                    Some((&below_archived, old_revision)),
                    "other",
                    true,
                    4,
                ),
                "INVALID_TRANSITION",
            ),
            (
                create(&id(52), Some((&id(99), old_revision)), "other", true, 3),
                "NOT_FOUND",
            ),
            (
                create(
                    &id(52),
                    Some(("01900000-0000-4000-8000-000000000001", old_revision)),
                    "other",
                    true,
                    2,
                ),
                "INVALID_ARGUMENT",
            ),
            (
                create(&id(52), None, "Uppercase", true, 2),
                "INVALID_ARGUMENT",
            ),
            (
                create(&child_request, scope, "changed", true, 5),
                "REQUEST_CONFLICT",
            ),
        ] {
            assert_eq!(report["error"]["code"], code);
            assert!(report.get("data").is_none());
        }
        assert_eq!(storage_state(&mut reader), committed);

        let assert_replay = |report: Value, original: &Value, head: &str| {
            assert_eq!(report["data"]["outcome"], "replayed");
            assert_eq!(report["context"]["db_commit"], head);
            for field in ["base_commit", "confirmed_commit", "result"] {
                assert_eq!(report["data"][field], original["data"][field]);
            }
        };
        assert_replay(
            create(&root_request, None, "shared", true, 0),
            &root,
            &committed.0,
        );
        assert_replay(
            create(&child_request, scope, "shared", true, 0),
            &child,
            &committed.0,
        );
        assert_eq!(storage_state(&mut reader), committed);

        let receipt: (String, String, String, String, String, String, String, String) = reader.exec_first(
            "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                    DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
             FROM operation_receipts WHERE request_id = ?",
            (&child_request,),
        ).unwrap().unwrap();
        let session = reader
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        drop(reader);
        let marker = state.join("pending-request.json");
        assert!(!marker.exists());
        // Simulate interruption after commit and before marker removal.
        let intent = json!({
            "version": 4, "kind": "area.create", "workspace_id": workspace_id,
            "launch_nonce": launch_nonce(&state), "session_id": session, "request_id": child_request,
            "base_commit": receipt.4, "fingerprint": receipt.2, "area_id": child_id,
            "parent_id": root_id, "receipt_digest": hash(&json!(receipt).to_string()),
        });
        fs::write(&marker, intent.to_string()).unwrap();
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        let stopped = run(&["workspace", "stop"], 0);
        assert_eq!(stopped["data"]["process_exit"], "pidfd_verified");
        assert_eq!(stopped["data"]["pending_request"], "recorded");
        assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
        let recovered = run(&["workspace", "recover", "--dolt-bin", dolt], 0);
        assert_eq!(recovered["data"]["outcome"], "confirmed");
        assert!(!marker.exists() && !state.join("stopped-request.json").exists());
        run(&start, 0);
        assert_eq!(
            storage_state(&mut connect_owned(&state, &port, "reader")),
            committed
        );

        // Seed two independently invalid introductions with matching current decisions.
        // The synthetic writer connection can stage exactly the rows under test.
        for (request, area, revision, slug, wrong_evidence) in [
            (id(60), id(61), id(62), "unverified-evidence", true),
            (id(63), id(64), id(65), "unverified-effect", false),
        ] {
            create(&root_request, None, slug, true, 5);
            let mut historical: Value =
                serde_json::from_slice(&fs::read(&evidence_path).unwrap()).unwrap();
            historical["evidence"]["request_id"] = json!(request);
            historical["evidence"]["withdrawn"] = json!(wrong_evidence);
            let historical = &historical["evidence"];
            let mut writer = connect_owned(&state, &port, "writer");
            let base = storage_state(&mut writer).0;
            writer
                .query_drop("SET SESSION dolt_transaction_commit=1")
                .unwrap();
            writer.query_drop("START TRANSACTION").unwrap();
            writer
                .exec_drop(
                    "INSERT INTO areas (id, slug, name, revision) VALUES (?, ?, ?, ?)",
                    (&area, slug, NAME, &revision),
                )
                .unwrap();
            if !wrong_evidence {
                writer
                    .exec_drop(
                        "UPDATE areas SET name = 'Unexpected edit' WHERE id = ?",
                        (&archived,),
                    )
                    .unwrap();
            }
            writer
                .exec_drop(
                    "INSERT INTO operation_receipts (request_id, kind, fingerprint, actor_ref,
                  authorization_ref, workflow_evidence_json, base_commit, result_json, created_at)
                 VALUES (?, 'area.create', ?, ?, ?, ?, ?, ?, UTC_TIMESTAMP(6))",
                    (
                        &request,
                        historical["fingerprint"].as_str().unwrap(),
                        ACTOR,
                        historical["authorization_ref"].as_str().unwrap(),
                        historical.to_string(),
                        &base,
                        json!({
                            "version": 1, "area_id": area, "area_revision": revision,
                            "parent_id": null, "parent_revision": null,
                        })
                        .to_string(),
                    ),
                )
                .unwrap();
            writer.query_drop("COMMIT").unwrap();
            let forged = storage_state(&mut writer);
            assert_ne!(forged.0, base);
            let (exit, code) = if wrong_evidence {
                (7, "AUTHORITY_UNVERIFIED")
            } else {
                (8, "COMMIT_UNCERTAIN")
            };
            let refused = create(&request, None, slug, true, exit);
            assert_eq!(refused["error"]["code"], code);
            assert!(refused.get("data").is_none());
            assert_eq!(storage_state(&mut writer), forged);
        }
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

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

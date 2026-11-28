use super::*;
use serde_json::json;

const ACTOR: &str = "fixture-artifact-author";
const TITLE: &str = "Fixture résumé";
const BODY: &[u8] = "# Synthetic artifact body\r\nλ\n".as_bytes();

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP and Linux pidfds"]
fn artifact_create_confirms_replays_and_recovers() {
    let listener = TcpListener::bind((HOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let workspace_id = id(1);
    let area = id(2);
    let archived = id(3);
    let revision = "area-r1";
    let fixture = Fixture::initialize(&format!(
        "{}\nINSERT INTO areas (id, slug, name, revision, archived_at) VALUES
        ('{area}', 'active', 'Active', '{revision}', NULL),
        ('{archived}', 'archived', 'Archived', '{revision}', UTC_TIMESTAMP(6));",
        empty_seed_sql(CASE_INSENSITIVE),
    ));
    let workspace = fixture.root.join("workspace");
    let state_home = fixture.root.join("state");
    let state = state_home.join("hive/v2/workspaces").join(&workspace_id);
    register_fixture(&fixture, &workspace, &state);
    drop(listener);
    let invoke_at = |root: &Path, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .env_clear()
            .env("HOME", fixture.root.join("home"))
            .env("XDG_STATE_HOME", &state_home)
            .current_dir(&fixture.root)
            .args(["--contract", "2", "--json", "--workspace"])
            .arg(root)
            .args(args)
            .output()
            .unwrap()
    };
    let invoke = |args: &[&str]| invoke_at(&workspace, args);
    let run = |args: &[&str], exit| {
        let output = invoke(args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert!(!report.to_string().contains("Synthetic artifact body"));
        assert!(
            !report
                .to_string()
                .contains("fixture-artifact-authorization")
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Synthetic artifact body"));
        report
    };
    let dolt = fixture.dolt.to_str().unwrap();
    let start = ["workspace", "start", "--dolt-bin", dolt, "--port", &port];
    run(&start, 0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let body_path = fixture.root.join("artifact.md");
        let evidence_path = fixture.root.join("artifact-evidence.json");
        let create =
            |request: &str, scope: Option<(&str, &str)>, body: &[u8], withdraw, exit| {
                let (selected_area, expected) = scope.unzip();
                let fingerprint = hash(
                    json!([
                        1,
                        "artifact.create",
                        workspace_id,
                        selected_area,
                        ACTOR,
                        expected,
                        "report",
                        TITLE,
                        hash(body),
                    ])
                    .to_string()
                    .as_bytes(),
                );
                let now = fixture_now();
                fs::write(&evidence_path, json!({
                "decision": "allow",
                "evidence": {
                    "version": 1, "operation": "artifact.create", "workspace_id": workspace_id,
                    "request_id": request, "fingerprint": fingerprint, "actor_ref": ACTOR,
                    "area_id": selected_area, "expected_area_revision": expected,
                    "authorization_ref": "fixture-artifact-authorization",
                    "evidence_id": "fixture-artifact-evidence", "revision": "r1",
                    "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                    "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
                    "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
                    "withdrawn": withdraw,
                }
            }).to_string()).unwrap();
                fs::write(&body_path, body).unwrap();
                let mut args = vec![
                    "artifact",
                    "create",
                    "--workspace-id",
                    &workspace_id,
                    "--database",
                    "db",
                    "--port",
                    &port,
                    "--kind",
                    "report",
                    "--title",
                    TITLE,
                    "--actor",
                    ACTOR,
                    "--request-id",
                    request,
                    "--file",
                    body_path.to_str().unwrap(),
                    "--evidence-file",
                    evidence_path.to_str().unwrap(),
                ];
                let if_match;
                if let Some((id, revision)) = scope {
                    if_match = format!("{id}={revision}");
                    args.extend(["--area", id, "--if-match", &if_match]);
                }
                let report = run(&args, exit);
                assert_eq!(report["command"], "artifact create");
                report
            };

        let mut reader = connect_owned(&state, &port, "reader");
        let before = storage_state(&mut reader);
        let standalone_request = id(50);
        let standalone = create(&standalone_request, None, BODY, false, 0);
        assert_eq!(standalone["data"]["outcome"], "confirmed");
        assert_eq!(standalone["data"]["base_commit"], before.0);
        assert_eq!(standalone["data"]["result"]["area_id"], Value::Null);
        assert_eq!(standalone["data"]["result"]["area_revision"], Value::Null);
        let standalone_id = standalone["data"]["result"]["artifact_id"]
            .as_str()
            .unwrap();
        let before_linked = storage_state(&mut reader);
        assert_eq!(before_linked.0, standalone["data"]["confirmed_commit"]);

        let request = id(51);
        let scope = Some((area.as_str(), revision));
        let linked = create(&request, scope, BODY, false, 0);
        assert_eq!(linked["data"]["outcome"], "confirmed");
        assert_eq!(linked["data"]["base_commit"], before_linked.0);
        let artifact = linked["data"]["result"]["artifact_id"].as_str().unwrap();
        let area_revision = linked["data"]["result"]["area_revision"].as_str().unwrap();
        assert_ne!(artifact, standalone_id);
        assert_ne!(area_revision, revision);
        let committed = storage_state(&mut reader);
        assert_eq!(committed.0, linked["data"]["confirmed_commit"]);

        let rows: Vec<(String, String, String, Vec<u8>, String, String)> = reader
            .query("SELECT id, kind, title, body, author_ref, revision FROM artifacts ORDER BY id")
            .unwrap();
        for (id, kind, title, body, actor, revision) in &rows {
            let report = if id == artifact {
                &linked
            } else {
                assert_eq!(id, standalone_id);
                &standalone
            };
            assert_eq!(
                (
                    kind.as_str(),
                    title.as_str(),
                    body.as_slice(),
                    actor.as_str()
                ),
                ("report", TITLE, BODY, ACTOR)
            );
            assert_eq!(
                revision.as_str(),
                report["data"]["result"]["artifact_revision"]
            );
        }
        assert_eq!(rows.len(), 2);
        assert_eq!(
            reader
                .query::<(String, String), _>("SELECT artifact_id, area_id FROM artifact_areas")
                .unwrap(),
            [(artifact.to_owned(), area.clone())]
        );
        assert_eq!(
            reader
                .exec_first::<String, _, _>("SELECT revision FROM areas WHERE id = ?", (&area,))
                .unwrap()
                .as_deref(),
            Some(area_revision)
        );
        assert_eq!(
            reader
                .query_first::<u64, _>("SELECT COUNT(*) FROM operation_receipts")
                .unwrap(),
            Some(2)
        );

        let assert_replay = |actual: Value, original: &Value, head: &str| {
            assert_eq!(actual["data"]["outcome"], "replayed");
            assert_eq!(actual["context"]["db_commit"], head);
            for field in ["base_commit", "confirmed_commit", "result"] {
                assert_eq!(actual["data"][field], original["data"][field]);
            }
        };
        assert_replay(
            create(&request, scope, BODY, false, 0),
            &linked,
            &committed.0,
        );
        assert_replay(
            create(&standalone_request, None, BODY, false, 0),
            &standalone,
            &committed.0,
        );
        for (report, code) in [
            (
                create(&request, scope, b"Changed body", false, 5),
                "REQUEST_CONFLICT",
            ),
            (create(&request, None, BODY, false, 5), "REQUEST_CONFLICT"),
            (
                create(&request, scope, BODY, true, 7),
                "AUTHORITY_UNVERIFIED",
            ),
            (create(&id(52), scope, BODY, false, 5), "STALE_REVISION"),
            (
                create(&id(52), Some((&archived, revision)), BODY, false, 4),
                "INVALID_TRANSITION",
            ),
            (create(&id(52), None, &[0xff], false, 2), "INVALID_ARGUMENT"),
            (
                create(&id(52), None, &vec![b'a'; 65537], false, 2),
                "CONTEXT_LIMIT",
            ),
        ] {
            assert_eq!(report["error"]["code"], code);
            assert!(report.get("data").is_none());
        }
        assert_eq!(storage_state(&mut reader), committed);
        let marker = state.join("pending-request.json");
        assert!(!marker.exists());

        let later_request = id(52);
        let later = create(&later_request, Some((&area, area_revision)), BODY, false, 0);
        let head = storage_state(&mut reader);
        assert_ne!(head.0, committed.0);
        assert_replay(create(&request, scope, BODY, false, 0), &linked, &head.0);
        assert_eq!(storage_state(&mut reader), head);

        let receipt: (String, String, String, String, String, String, String, String) = reader.exec_first(
            "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                    DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
             FROM operation_receipts WHERE request_id = ?",
            (&later_request,),
        ).unwrap().unwrap();
        assert!(
            !json!(receipt)
                .to_string()
                .contains("Synthetic artifact body")
        );
        let session = reader
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        drop(reader);
        let mut intent = json!({
            "version": 3, "kind": "artifact.create", "workspace_id": workspace_id,
            "launch_nonce": launch_nonce(&state), "session_id": session, "request_id": later_request,
            "base_commit": receipt.4, "fingerprint": receipt.2,
            "area_id": area, "artifact_id": later["data"]["result"]["artifact_id"],
            "receipt_digest": hash(json!(receipt).to_string().as_bytes()),
        });
        let record = |intent: &Value| {
            fs::write(&marker, intent.to_string()).unwrap();
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        };
        let stopped = |outcome| {
            let report = run(&["workspace", "stop"], 0);
            assert_eq!(report["data"]["process_exit"], "pidfd_verified");
            assert_eq!(report["data"]["pending_request"], "recorded");
            assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
            let report = run(&["workspace", "recover", "--dolt-bin", dolt], 0);
            assert_eq!(report["data"]["outcome"], outcome);
            assert!(!marker.exists() && !state.join("stopped-request.json").exists());
            run(&start, 0);
            assert_eq!(
                storage_state(&mut connect_owned(&state, &port, "reader")),
                head
            );
        };
        record(&intent);
        stopped("confirmed");

        let mut prior = connect_owned(&state, &port, "writer");
        let session = prior
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        let rolled_back = id(54);
        intent["launch_nonce"] = json!(launch_nonce(&state));
        intent["session_id"] = json!(session);
        intent["request_id"] = json!(id(53));
        intent["base_commit"] = json!(head.0);
        intent["artifact_id"] = json!(rolled_back);
        intent.as_object_mut().unwrap().remove("area_id");
        prior.query_drop("START TRANSACTION").unwrap();
        prior.exec_drop(
            "INSERT INTO artifacts SELECT ?, kind, title, body, author_ref, source_ref, revision,
                    created_at, substantive_updated_at, archived_at, archive_reason
             FROM artifacts WHERE id = ?",
            (&rolled_back, standalone_id),
        ).unwrap();
        assert_eq!(prior.affected_rows(), 1);
        record(&intent);
        let blocked = create(&id(55), None, BODY, false, 8);
        assert_eq!(blocked["error"]["code"], "COMMIT_UNCERTAIN");
        assert_eq!(fs::read_to_string(&marker).unwrap(), intent.to_string());
        drop(prior);
        stopped("rolled_back");

        let mut reader = connect_owned(&state, &port, "reader");
        let before_revision = later["data"]["result"]["area_revision"].as_str().unwrap();
        let delegation_ref = fixture.root.join("delegation-reference.json");
        let current_source = fixture.root.join("current-delegation.json");
        let locator = json!({
            "version": 1, "authorization_ref": "fixture-artifact-authorization",
            "evidence_id": "fixture-delegation", "revision": "grant-r1", "source_ref": "fixture-source",
        });
        let saved_bytes = locator.to_string();
        fs::write(&delegation_ref, &saved_bytes).unwrap();
        let mut current = locator;
        let now = fixture_now();
        current.as_object_mut().unwrap().extend([
            ("operation".into(), json!("artifact.create")),
            ("workspace_id".into(), json!(workspace_id)),
            ("actor_ref".into(), json!(ACTOR)),
            ("area_id".into(), json!(area)),
            ("issuer_ref".into(), json!("fixture-issuer")),
            (
                "valid_from".into(),
                json!(fixture_timestamp(now - time::Duration::hours(1))),
            ),
            (
                "expires_at".into(),
                json!(fixture_timestamp(now + time::Duration::hours(1))),
            ),
            ("withdrawn".into(), json!(false)),
        ]);
        let publish = |value: &Value| {
            let replacement = current_source.with_extension("tmp");
            fs::write(&replacement, value.to_string()).unwrap();
            fs::rename(replacement, &current_source).unwrap();
        };
        let delegated_request = id(56);
        let fresh_request = id(57);
        let original_match = format!("{area}={before_revision}");
        let delegated_args = |request, expected| {
            [
                "artifact",
                "create",
                "--workspace-id",
                &workspace_id,
                "--database",
                "db",
                "--port",
                &port,
                "--kind",
                "report",
                "--title",
                TITLE,
                "--actor",
                ACTOR,
                "--request-id",
                request,
                "--file",
                body_path.to_str().unwrap(),
                "--evidence-file",
                delegation_ref.to_str().unwrap(),
                "--delegation-source",
                current_source.to_str().unwrap(),
                "--area",
                &area,
                "--if-match",
                expected,
            ]
        };
        fs::write(&body_path, BODY).unwrap();
        publish(&current);
        let original = delegated_args(&delegated_request, &original_match);
        let confirmed = run(&original, 0);
        assert_eq!(confirmed["data"]["outcome"], "confirmed");
        assert_eq!(confirmed["data"]["base_commit"], head.0);
        let committed = storage_state(&mut reader);
        assert_replay(run(&original, 0), &confirmed, &committed.0);
        fs::write(&body_path, b"Changed body").unwrap();
        assert_eq!(run(&original, 5)["error"]["code"], "REQUEST_CONFLICT");
        fs::write(&body_path, BODY).unwrap();
        let retained: String = reader.exec_first(
            "SELECT CAST(workflow_evidence_json AS CHAR) FROM operation_receipts WHERE request_id = ?",
            (&delegated_request,),
        ).unwrap().unwrap();
        assert!(!retained.contains(current_source.to_str().unwrap()));
        let evidence: Value = serde_json::from_str(&retained).unwrap();
        assert_eq!(evidence["version"], 2);
        assert_eq!(evidence["revision"], "grant-r1");
        assert_eq!(evidence["request_id"], delegated_request);

        let withdrawn = {
            let mut value = current.clone();
            value["withdrawn"] = json!(true);
            value
        };
        publish(&withdrawn);
        assert_eq!(run(&original, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");

        let current_match = format!(
            "{area}={}",
            confirmed["data"]["result"]["area_revision"]
                .as_str()
                .unwrap()
        );
        // The payload has a fresh request ID and the current Area revision. The
        // saved grant locator remains byte-for-byte identical to the successful write.
        let delegated = delegated_args(&fresh_request, &current_match);
        let unchanged = |reader: &mut Conn| {
            assert_eq!(fs::read_to_string(&delegation_ref).unwrap(), saved_bytes);
            assert_eq!(storage_state(reader), committed);
            assert_eq!(
                reader
                    .exec_first::<u64, _, _>(
                        "SELECT COUNT(*) FROM operation_receipts WHERE request_id = ?",
                        (&fresh_request,),
                    )
                    .unwrap(),
                Some(0)
            );
            assert!(!marker.exists());
        };
        let deny = |reader: &mut Conn| {
            let response = run(&delegated, 7);
            assert_eq!(response["error"]["code"], "AUTHORITY_UNVERIFIED");
            assert!(response.get("data").is_none());
            assert!(
                !response
                    .to_string()
                    .contains(current_source.to_str().unwrap())
            );
            unchanged(reader);
        };
        deny(&mut reader);
        for (field, value) in [
            ("revision", json!("grant-r2")),
            (
                "expires_at",
                json!(fixture_timestamp(now - time::Duration::minutes(1))),
            ),
            ("area_id", json!(archived)),
            ("workspace_id", json!(id(58))),
            ("actor_ref", json!("fixture-other-actor")),
            ("operation", json!("artifact.update")),
        ] {
            let mut wrong = current.clone();
            wrong[field] = value;
            publish(&wrong);
            deny(&mut reader);
        }
        fs::remove_file(&current_source).unwrap();
        deny(&mut reader);
        let without_source: Vec<_> = delegated
            .into_iter()
            .filter(|arg| !["--delegation-source", current_source.to_str().unwrap()].contains(arg))
            .collect();
        assert_eq!(
            run(&without_source, 7)["error"]["code"],
            "AUTHORITY_UNVERIFIED"
        );
        unchanged(&mut reader);

        // A separate current reviewed decision can inspect the retained outcome.
        assert_replay(
            create(
                &delegated_request,
                Some((&area, before_revision)),
                BODY,
                false,
                0,
            ),
            &confirmed,
            &committed.0,
        );
        unchanged(&mut reader);

        publish(&current);
        let inactive = fixture.root.join("inactive-copy");
        fs::create_dir(&inactive).unwrap();
        fs::copy(
            workspace.join("hive-workspace.yml"),
            inactive.join("hive-workspace.yml"),
        )
        .unwrap();
        let output = invoke_at(&inactive, &delegated);
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(7), "{response}");
        assert_eq!(response["error"]["code"], "AUTHORITY_UNVERIFIED");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("canonical root"),
            "{response}"
        );
        assert_eq!(fs::read_dir(&inactive).unwrap().count(), 1);
        unchanged(&mut reader);
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

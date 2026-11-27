use super::*;
use serde_json::json;

const ACTOR: &str = "fixture-editor";
const AUTHORIZATION: &str = "fixture-artifact-update-decision";
const ORIGINAL: &str = "Synthetic prior Markdown";
const BODY: &str = "# Synthetic replacement Markdown\r\nλ\n";
const REVISION: &str = "artifact-r1";

struct Edit<'a> {
    request: &'a str,
    artifact: &'a str,
    area: Option<&'a str>,
    expected: &'a str,
    body: &'a [u8],
}

impl Edit<'_> {
    fn decision(&self, workspace: &str) -> Value {
        let now = fixture_now();
        let fingerprint = hash(
            json!([
                1,
                "artifact.update",
                workspace,
                self.area,
                self.artifact,
                ACTOR,
                self.expected,
                hash(self.body)
            ])
            .to_string()
            .as_bytes(),
        );
        json!({
            "decision": "allow",
            "evidence": {
                "version": 1, "operation": "artifact.update", "workspace_id": workspace,
                "request_id": self.request, "fingerprint": fingerprint, "actor_ref": ACTOR,
                "area_id": self.area, "artifact_id": self.artifact,
                "expected_artifact_revision": self.expected,
                "authorization_ref": AUTHORIZATION, "evidence_id": "fixture-evidence",
                "revision": "r1", "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
                "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
                "withdrawn": false,
            }
        })
    }
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP and Linux pidfds"]
fn artifact_update_confirms_replays_and_recovers() {
    let listener = TcpListener::bind((HOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let workspace_id = id(1);
    let area = id(2);
    let other = id(3);
    let archived_parent = id(4);
    let archived_child = id(5);
    let standalone = id(10);
    let linked = id(11);
    let multi = id(12);
    let archived = id(13);
    let invalid = id(14);
    let frozen = id(15);
    let project_only = id(16);
    let project = id(17);
    let fixture = Fixture::initialize(&format!(
        "{}
         INSERT INTO areas (id, parent_id, slug, name, archived_at, revision) VALUES
            ('{area}', NULL, 'active', 'Active', NULL, 'area-r1'),
            ('{other}', NULL, 'other', 'Other', NULL, 'area-r1'),
            ('{archived_parent}', NULL, 'archived', 'Archived', '2026-01-01', 'area-r1'),
            ('{archived_child}', '{archived_parent}', 'child', 'Child', NULL, 'area-r1');
         INSERT INTO artifacts (id, kind, title, body, author_ref, source_ref, revision,
                                created_at, substantive_updated_at, archived_at, archive_reason) VALUES
            ('{standalone}', 'report', 'Fixture report', '{ORIGINAL}', 'fixture-author', NULL,
             '{REVISION}', '2026-01-01', '2026-01-01', NULL, NULL),
            ('{linked}', 'report', 'Fixture report', '{ORIGINAL}', 'fixture-author', 'fixture-origin',
             '{REVISION}', '2026-01-01', '2026-01-01', NULL, NULL),
            ('{multi}', 'report', 'Fixture report', '{ORIGINAL}', 'fixture-author', NULL,
             '{REVISION}', '2026-01-01', '2026-01-01', NULL, NULL),
            ('{archived}', 'report', 'Fixture report', '{ORIGINAL}', 'fixture-author', NULL,
             '{REVISION}', '2026-01-01', '2026-01-01', '2026-01-01', 'fixture archive'),
            ('{invalid}', 'invalid-kind', 'Fixture report', '{ORIGINAL}', 'fixture-author', NULL,
             '{REVISION}', '2026-01-01', '2026-01-01', NULL, NULL),
            ('{frozen}', 'report', 'Fixture report', '{ORIGINAL}', 'fixture-author', NULL,
             '{REVISION}', '2026-01-01', '2026-01-01', NULL, NULL),
            ('{project_only}', 'report', 'Fixture report', '{ORIGINAL}', 'fixture-author', NULL,
             '{REVISION}', '2026-01-01', '2026-01-01', NULL, NULL);
         INSERT INTO projects (id, name, status, revision) VALUES ('{project}', 'Fixture', 'active', 'r1');
         INSERT INTO artifact_projects VALUES ('{project_only}', '{project}');
         INSERT INTO artifact_areas VALUES ('{linked}', '{area}'), ('{multi}', '{area}'),
                                           ('{multi}', '{other}'), ('{frozen}', '{archived_child}');",
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
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{report}");
        for private in [ORIGINAL, "Synthetic replacement Markdown", AUTHORIZATION] {
            assert!(!report.to_string().contains(private));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(private));
        }
        report
    };
    let dolt = fixture.dolt.to_str().unwrap();
    let start = ["workspace", "start", "--dolt-bin", dolt, "--port", &port];
    run(&start, 0);
    let test = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let body_path = fixture.root.join("artifact-edit.md");
        let evidence_path = fixture.root.join("artifact-edit-decision.json");
        let update = |edit: &Edit<'_>, decision: Value, exit| {
            fs::write(&body_path, edit.body).unwrap();
            fs::write(&evidence_path, decision.to_string()).unwrap();
            let if_match = format!("{}={}", edit.artifact, edit.expected);
            let mut args = vec![
                "artifact",
                "update",
                edit.artifact,
                "--workspace-id",
                &workspace_id,
                "--database",
                "db",
                "--port",
                &port,
                "--actor",
                ACTOR,
                "--request-id",
                edit.request,
                "--if-match",
                &if_match,
                "--file",
                body_path.to_str().unwrap(),
                "--evidence-file",
                evidence_path.to_str().unwrap(),
            ];
            if let Some(area) = edit.area {
                args.extend(["--area", area]);
            }
            let report = run(&args, exit);
            assert_eq!(report["command"], "artifact update");
            report
        };
        let apply = |edit: &Edit<'_>, exit| update(edit, edit.decision(&workspace_id), exit);

        let mut reader = connect_owned(&state, &port, "reader");
        let initial = storage_state(&mut reader);
        let mut writer = connect_owned(&state, &port, "writer");
        assert!(
            reader
                .query_drop("UPDATE artifacts SET body = '' WHERE 1 = 0")
                .is_err()
        );
        assert!(
            writer
                .query_drop("DELETE FROM artifacts WHERE 1 = 0")
                .is_err()
        );
        assert!(
            writer
                .query_drop("UPDATE artifact_areas SET area_id = area_id WHERE 1 = 0")
                .is_err()
        );
        assert!(
            writer
                .query_drop("UPDATE operation_receipts SET kind = kind WHERE 1 = 0")
                .is_err()
        );
        drop(writer);

        let request = id(20);
        let edit = Edit {
            request: &request,
            artifact: &linked,
            area: Some(&area),
            expected: REVISION,
            body: BODY.as_bytes(),
        };
        let mut denied = edit.decision(&workspace_id);
        denied["evidence"]["withdrawn"] = json!(true);
        assert_eq!(
            update(&edit, denied, 7)["error"]["code"],
            "AUTHORITY_UNVERIFIED"
        );
        let mut wrong_target = edit.decision(&workspace_id);
        wrong_target["evidence"]["artifact_id"] = json!(standalone);
        assert_eq!(
            update(&edit, wrong_target, 7)["error"]["code"],
            "AUTHORITY_UNVERIFIED"
        );
        for (artifact, area, revision, body, status, code) in [
            (
                &linked,
                None,
                REVISION,
                BODY.as_bytes(),
                7,
                "AUTHORITY_UNVERIFIED",
            ),
            (
                &linked,
                Some(other.as_str()),
                REVISION,
                BODY.as_bytes(),
                7,
                "AUTHORITY_UNVERIFIED",
            ),
            (
                &multi,
                Some(area.as_str()),
                REVISION,
                BODY.as_bytes(),
                7,
                "AUTHORITY_UNVERIFIED",
            ),
            (
                &project_only,
                None,
                REVISION,
                BODY.as_bytes(),
                7,
                "AUTHORITY_UNVERIFIED",
            ),
            (
                &frozen,
                Some(archived_child.as_str()),
                REVISION,
                BODY.as_bytes(),
                4,
                "INVALID_TRANSITION",
            ),
            (
                &archived,
                None,
                REVISION,
                BODY.as_bytes(),
                4,
                "INVALID_TRANSITION",
            ),
            (
                &invalid,
                None,
                REVISION,
                BODY.as_bytes(),
                4,
                "INVALID_RECORD",
            ),
            (
                &linked,
                Some(area.as_str()),
                REVISION,
                ORIGINAL.as_bytes(),
                4,
                "INVALID_TRANSITION",
            ),
            (
                &linked,
                Some(area.as_str()),
                "stale-r0",
                BODY.as_bytes(),
                5,
                "STALE_REVISION",
            ),
            (
                &linked,
                Some(area.as_str()),
                REVISION,
                &[0xff],
                2,
                "INVALID_ARGUMENT",
            ),
        ] {
            let candidate = Edit {
                artifact,
                area,
                expected: revision,
                body,
                ..edit
            };
            assert_eq!(apply(&candidate, status)["error"]["code"], code);
        }
        let marker = state.join("pending-request.json");
        assert_eq!(storage_state(&mut reader), initial);
        assert!(!marker.exists());

        let standalone_request = id(21);
        let standalone_edit = Edit {
            request: &standalone_request,
            artifact: &standalone,
            area: None,
            body: b"",
            ..edit
        };
        let standalone_result = apply(&standalone_edit, 0);
        assert_eq!(standalone_result["data"]["result"]["area_id"], Value::Null);
        let empty: Vec<u8> = reader
            .exec_first(
                "SELECT CAST(body AS BINARY) FROM artifacts WHERE id = ?",
                (&standalone,),
            )
            .unwrap()
            .unwrap();
        assert!(empty.is_empty());

        let confirmed = apply(&edit, 0);
        assert_eq!(confirmed["data"]["outcome"], "confirmed");
        assert_eq!(confirmed["data"]["kind"], "artifact.update");
        assert_eq!(
            confirmed["data"]["base_commit"],
            standalone_result["data"]["confirmed_commit"]
        );
        let result = &confirmed["data"]["result"];
        let revision = result["artifact_revision"].as_str().unwrap();
        assert_ne!(revision, REVISION);
        assert_eq!(result["artifact_id"], linked);
        assert_eq!(result["area_id"], area);
        let proof: (Vec<u8>, String, bool, bool) = reader.exec_first(
            "SELECT CAST(a.body AS BINARY), a.revision,
                    a.kind = 'report' AND a.title = 'Fixture report' AND a.author_ref = 'fixture-author'
                    AND a.source_ref = 'fixture-origin' AND a.created_at = '2026-01-01'
                    AND a.archived_at IS NULL AND a.archive_reason IS NULL,
                    a.substantive_updated_at = r.created_at
             FROM artifacts a CROSS JOIN operation_receipts r WHERE a.id = ? AND r.request_id = ?",
            (&linked, &request),
        ).unwrap().unwrap();
        assert_eq!(
            proof,
            (BODY.as_bytes().to_vec(), revision.to_owned(), true, true)
        );
        assert_eq!(
            reader
                .exec_first::<String, _, _>("SELECT revision FROM areas WHERE id = ?", (&area,))
                .unwrap()
                .as_deref(),
            Some("area-r1")
        );
        assert_eq!(
            reader
                .exec_first::<String, _, _>(
                    "SELECT area_id FROM artifact_areas WHERE artifact_id = ?",
                    (&linked,)
                )
                .unwrap()
                .as_deref(),
            Some(area.as_str())
        );

        let later_request = id(22);
        let later_edit = Edit {
            request: &later_request,
            expected: revision,
            body: b"# Later fixture",
            ..edit
        };
        let later = apply(&later_edit, 0);
        let head = storage_state(&mut reader);
        assert_eq!(head.0, later["data"]["confirmed_commit"]);
        let replay = apply(&edit, 0);
        assert_eq!(replay["data"]["outcome"], "replayed");
        assert_eq!(replay["context"]["db_commit"], head.0);
        for field in ["base_commit", "confirmed_commit", "result"] {
            assert_eq!(replay["data"][field], confirmed["data"][field]);
        }
        let changed_request = Edit {
            body: b"different request body",
            ..edit
        };
        assert_eq!(
            apply(&changed_request, 5)["error"]["code"],
            "REQUEST_CONFLICT"
        );
        assert_eq!(storage_state(&mut reader), head);

        let receipt: (String, String, String, String, String, String, String, String) = reader.exec_first(
            "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                    DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
             FROM operation_receipts WHERE request_id = ?", (&later_request,),
        ).unwrap().unwrap();
        let receipt_json = json!(receipt).to_string();
        assert!(!receipt_json.contains("# Later fixture"));
        let session = reader
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        drop(reader);
        let mut intent = json!({
            "version": 3, "kind": "artifact.update", "workspace_id": workspace_id,
            "launch_nonce": launch_nonce(&state), "session_id": session, "request_id": later_request,
            "base_commit": receipt.4, "fingerprint": receipt.2,
            "area_id": area, "artifact_id": linked, "receipt_digest": hash(receipt_json.as_bytes()),
        });
        let record = |intent: &Value| {
            fs::write(&marker, intent.to_string()).unwrap();
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        };
        record(&intent);
        let stopped = run(&["workspace", "stop"], 0);
        assert_eq!(stopped["data"]["process_exit"], "pidfd_verified");
        assert_eq!(stopped["data"]["pending_request"], "recorded");
        let recovered = run(&["workspace", "recover", "--dolt-bin", dolt], 0);
        assert_eq!(recovered["data"]["outcome"], "confirmed");
        assert!(!marker.exists() && !state.join("stopped-request.json").exists());
        run(&start, 0);

        let mut prior = connect_owned(&state, &port, "writer");
        let session = prior
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        intent["launch_nonce"] = json!(launch_nonce(&state));
        intent["session_id"] = json!(session);
        intent["request_id"] = json!(id(23));
        intent["base_commit"] = json!(head.0);
        prior.query_drop("START TRANSACTION").unwrap();
        prior
            .exec_drop(
                "UPDATE artifacts SET body = ? WHERE id = ?",
                ("Fixture uncommitted Markdown", &linked),
            )
            .unwrap();
        record(&intent);
        assert_eq!(apply(&later_edit, 8)["error"]["code"], "COMMIT_UNCERTAIN");
        assert!(marker.exists());
        drop(prior);
        assert_eq!(apply(&later_edit, 0)["data"]["outcome"], "replayed");
        assert!(!marker.exists());
        assert_eq!(
            storage_state(&mut connect_owned(&state, &port, "reader")),
            head
        );

        // The later link makes the declared scope valid at HEAD. The update
        // receipt still cannot prove that scope at its own base.
        let wrong_base_request = id(24);
        let wrong_base_edit = Edit {
            request: &wrong_base_request,
            area: Some(&area),
            body: BODY.as_bytes(),
            expected: standalone_result["data"]["result"]["artifact_revision"]
                .as_str()
                .unwrap(),
            ..standalone_edit
        };
        let mut writer = connect_owned(&state, &port, "writer");
        seed_receipt_outside_declared_scope(&mut writer, &workspace_id, &wrong_base_edit, &head.0);
        let later_scope = storage_state(&mut writer);
        let refused = apply(&wrong_base_edit, 8);
        assert_eq!(refused["error"]["code"], "COMMIT_UNCERTAIN");
        assert_eq!(storage_state(&mut writer), later_scope);
    }));
    if state.join("server.json").exists() {
        let stop = invoke(&["workspace", "stop"]);
        if !stop.status.success() {
            let path = fixture.root.display().to_string();
            std::mem::forget(fixture);
            panic!("Verified fixture stop failed; preserved the disposable store at {path}");
        }
    }
    if let Err(error) = test {
        std::panic::resume_unwind(error);
    }
}

fn seed_receipt_outside_declared_scope(
    writer: &mut Conn,
    workspace: &str,
    edit: &Edit<'_>,
    base: &str,
) {
    let decision = edit.decision(workspace);
    let evidence = decision["evidence"].to_string();
    let fingerprint = decision["evidence"]["fingerprint"].as_str().unwrap();
    let revision = id(25);
    let result = json!({
        "version": 1, "artifact_id": edit.artifact, "artifact_revision": revision, "area_id": edit.area,
    }).to_string();
    let timestamp = fixture_timestamp(fixture_now());
    let sql_time = timestamp[..19].replace('T', " ");
    writer
        .query_drop("SET SESSION dolt_transaction_commit=1")
        .unwrap();
    writer.query_drop("START TRANSACTION").unwrap();
    writer
        .exec_drop(
            "UPDATE artifacts SET body = ?, revision = ?, substantive_updated_at = ? WHERE id = ?",
            (edit.body, &revision, &sql_time, edit.artifact),
        )
        .unwrap();
    writer
        .exec_drop(
            "INSERT INTO operation_receipts VALUES (?, 'artifact.update', ?, ?, ?, ?, ?, ?, ?)",
            (
                edit.request,
                fingerprint,
                ACTOR,
                AUTHORIZATION,
                evidence,
                base,
                result,
                sql_time,
            ),
        )
        .unwrap();
    writer.query_drop("COMMIT").unwrap();
    writer
        .exec_drop(
            "INSERT INTO artifact_areas VALUES (?, ?)",
            (edit.artifact, edit.area.unwrap()),
        )
        .unwrap();
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

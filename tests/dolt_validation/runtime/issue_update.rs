use super::*;
use serde_json::json;

const BODY: &str = "Synthetic revised body café\r\nλ\n";
const ACCEPTANCE: &str = "Synthetic acceptance café\r\n";
const HOLD: &str = "Synthetic scheduling constraint";
const TITLE: &str = "dependent";
const ACTOR: &str = "fixture-update-planner";

pub(super) fn exercise(
    state: &Path,
    root: &Path,
    port: &str,
    dolt: &Path,
    run: &impl Fn(&[&str], i32) -> Value,
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
) {
    let issue = id(13);
    let area = id(3);
    let expected = id(4);
    let workspace = id(1);
    let actor = ACTOR;
    let request = id(190);
    let marker = state.join("pending-request.json");
    let evidence = root.join("update-decision.json");
    let body = root.join("update-body.md");
    let acceptance = root.join("update-acceptance.md");
    fs::write(&body, BODY).unwrap();
    fs::write(&acceptance, ACCEPTANCE).unwrap();
    let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let update = |request: &str,
                  area: &str,
                  issue: &str,
                  revision: &str,
                  edits: &[&str],
                  values: Value,
                  exit: i32| {
        let fingerprint = digest(
            json!([
                1,
                "issue.update",
                workspace,
                area,
                issue,
                actor,
                revision,
                values
            ])
            .to_string()
            .as_bytes(),
        );
        let now = fixture_now();
        let decision = json!({
            "decision": "allow",
            "evidence": {
                "version": 1, "operation": "issue.update", "workspace_id": workspace,
                "request_id": request, "fingerprint": fingerprint, "actor_ref": actor,
                "area_id": area, "issue_id": issue, "expected_issue_revision": revision,
                "authorization_ref": "fixture-update-authorization", "evidence_id": "fixture-update-evidence",
                "revision": "r1", "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
                "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
                "withdrawn": false,
            }
        });
        fs::write(&evidence, decision.to_string()).unwrap();
        let if_match = format!("{issue}={revision}");
        let mut args = vec![
            "issue",
            "update",
            issue,
            "--workspace-id",
            &workspace,
            "--database",
            "db",
            "--port",
            port,
            "--area",
            area,
            "--actor",
            actor,
            "--if-match",
            &if_match,
            "--request-id",
            request,
            "--evidence-file",
            evidence.to_str().unwrap(),
        ];
        args.extend(edits);
        let report = run(&args, exit);
        assert_eq!(report["command"], "issue update");
        for private in [
            "Synthetic revised body",
            "Synthetic acceptance",
            HOLD,
            "fixture-update-authorization",
        ] {
            assert!(!report.to_string().contains(private));
        }
        report
    };
    let all_edits = [
        "--title",
        TITLE,
        "--file",
        body.to_str().unwrap(),
        "--acceptance-file",
        acceptance.to_str().unwrap(),
        "--priority",
        "0",
        "--hold",
        HOLD,
    ];
    let all_values = json!([
        TITLE,
        digest(BODY.as_bytes()),
        digest(ACCEPTANCE.as_bytes()),
        0,
        [digest(HOLD.as_bytes())]
    ]);
    let title_edit = ["--title", "Synthetic new title"];
    let title_values = json!(["Synthetic new title", null, null, null, null]);
    let mut reader = connect_owned(state, port, "reader");
    let before = storage_state(&mut reader);
    assert_eq!(
        update(
            &id(191),
            &id(2),
            &issue,
            &expected,
            &title_edit,
            title_values.clone(),
            7
        )["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    assert_eq!(
        update(
            &id(191),
            &area,
            &issue,
            &expected,
            &title_edit,
            json!([null, null, null, 0, null]),
            7
        )["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    assert_eq!(storage_state(&mut reader), before);
    assert!(!marker.exists());

    let confirmed = update(
        &request,
        &area,
        &issue,
        &expected,
        &all_edits,
        all_values.clone(),
        0,
    );
    let result = &confirmed["data"]["result"];
    let revision = result["issue_revision"].as_str().unwrap();
    assert_ne!(revision, expected);
    assert_eq!(confirmed["data"]["kind"], "issue.update");
    assert_eq!(confirmed["data"]["outcome"], "confirmed");
    assert_eq!(confirmed["data"]["base_commit"], before.0);
    assert_eq!(result["issue_id"], issue);
    assert_eq!(result["area_id"], area);
    assert!(result.get("fields").is_none());
    let stored: (String, Vec<u8>, Vec<u8>, u8, String, String, String, String) = reader
        .exec_first(
            "SELECT title, CAST(body AS BINARY), CAST(acceptance_criteria AS BINARY), priority,
                    hold_reason, status, area_id, revision FROM issues WHERE id = ?",
            (&issue,),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        stored,
        (
            TITLE.into(),
            BODY.as_bytes().into(),
            ACCEPTANCE.as_bytes().into(),
            0,
            HOLD.into(),
            "todo".into(),
            area.clone(),
            revision.into()
        )
    );
    let prerequisites: Vec<String> = reader
        .exec(
            "SELECT prerequisite_id FROM issue_dependencies WHERE issue_id = ?",
            (&issue,),
        )
        .unwrap();
    assert_eq!(prerequisites, [id(12)]);
    let after = storage_state(&mut reader);
    assert_eq!(confirmed["data"]["confirmed_commit"], after.0);
    assert!(!marker.exists());

    let receipt = stored_receipt(&mut reader, &request);
    let mut observer = connect_owned(state, port, "writer");
    let mut prior = connect_owned(state, port, "writer");
    prior.query_drop("START TRANSACTION").unwrap();
    let session: u64 = prior
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    let intent = json!({
        "version": 2, "kind": "issue.update", "workspace_id": workspace,
        "launch_nonce": launch_nonce(state), "session_id": session, "request_id": request,
        "base_commit": before.0, "fingerprint": receipt.2, "area_id": area,
        "issue_id": issue, "receipt_digest": digest(json!(receipt).to_string().as_bytes()),
    });
    record_intent(state, &intent);
    assert_eq!(
        update(
            &id(191),
            &area,
            &issue,
            revision,
            &title_edit,
            title_values.clone(),
            8
        )["error"]["code"],
        "COMMIT_UNCERTAIN"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), intent.to_string());
    prior.query_drop("ROLLBACK").unwrap();
    drop(prior);
    wait_for_session_exit(&mut observer, session);
    let recovered = update(
        &request,
        &area,
        &issue,
        &expected,
        &all_edits,
        all_values.clone(),
        0,
    );
    assert_eq!(recovered["data"]["outcome"], "replayed");
    assert_eq!(recovered["data"]["result"], *result);
    assert!(!marker.exists());

    assert_eq!(
        update(
            &id(191),
            &area,
            &issue,
            &expected,
            &title_edit,
            title_values.clone(),
            5
        )["error"]["code"],
        "STALE_REVISION"
    );
    assert_eq!(
        update(
            &id(191),
            &area,
            &issue,
            revision,
            &all_edits,
            all_values.clone(),
            2
        )["error"]["code"],
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        update(
            &request,
            &area,
            &issue,
            &expected,
            &title_edit,
            title_values.clone(),
            5
        )["error"]["code"],
        "REQUEST_CONFLICT"
    );
    assert_eq!(storage_state(&mut reader), after);
    drop(observer);
    drop(reader);
    stopped_recovery(state, port, dolt, run, &intent);
    let mut reader = connect_owned(state, port, "reader");
    assert_eq!(storage_state(&mut reader), after);

    let cleared = update(
        &id(191),
        &area,
        &issue,
        revision,
        &["--clear-hold"],
        json!([null, null, null, null, [null]]),
        0,
    );
    let hold: bool = reader
        .exec_first(
            "SELECT hold_reason IS NULL FROM issues WHERE id = ?",
            (&issue,),
        )
        .unwrap()
        .unwrap();
    assert!(hold);
    let cleared_commit = cleared["data"]["confirmed_commit"].as_str().unwrap();
    let cleared_revision = cleared["data"]["result"]["issue_revision"]
        .as_str()
        .unwrap();
    let archive_ancestor = format!(
        "UPDATE areas SET archived_at = UTC_TIMESTAMP(6) WHERE id = '{}';",
        id(2)
    );
    drop(reader);
    restart(Some(OfflineFixtureEdit::CommitSql(&archive_ancestor)), None);
    let mut reader = connect_owned(state, port, "reader");
    let archived = storage_state(&mut reader);
    assert_ne!(archived.0, cleared_commit);
    assert_eq!(
        update(
            &id(198),
            &area,
            &issue,
            cleared_revision,
            &title_edit,
            title_values.clone(),
            4
        )["error"]["code"],
        "INVALID_TRANSITION"
    );
    let replayed = update(
        &request, &area, &issue, &expected, &all_edits, all_values, 0,
    );
    assert_eq!(replayed["data"]["result"], *result);
    assert_eq!(replayed["data"]["confirmed_commit"], after.0);
    assert_eq!(replayed["context"]["db_commit"], archived.0);
    assert_eq!(storage_state(&mut reader), archived);
    assert!(!marker.exists());

    let commit = &archived.0;
    let invocation = id(192);
    let now = fixture_now();
    let proof = json!({
        "version": 1, "evidence_id": "fixture-inspection", "revision": "r1",
        "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
        "workspace_id": workspace, "actor_ref": actor, "bot_id": null,
        "area_id": area, "issue_id": issue, "operations": ["inspect_operation_receipt"],
        "record": {"kind": "operation_receipt", "id": request, "revision": commit},
        "invocation_id": invocation, "db_commit": commit,
        "valid_from": fixture_timestamp(now - time::Duration::seconds(10)),
        "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
        "withdrawn": false,
    });
    fs::write(&evidence, proof.to_string()).unwrap();
    let inspected = run(
        &[
            "operation",
            "show",
            &request,
            "--database",
            "db",
            "--port",
            port,
            "--workspace-id",
            &workspace,
            "--area",
            &area,
            "--issue",
            &issue,
            "--actor",
            actor,
            "--invocation-id",
            &invocation,
            "--evidence-file",
            evidence.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(inspected["data"]["result"], *result);
    assert_eq!(inspected["data"]["confirmed_commit"], after.0);
    assert!(inspected["data"]["current_claim"].is_null());
    assert!(!inspected.to_string().contains("Synthetic revised body"));

    drop(reader);
    restart(Some(OfflineFixtureEdit::RestoreHead(cleared_commit)), None);
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "UPDATE issues SET status='review' WHERE id = '{}';",
            id(10)
        ))),
        None,
    );
    let mut reader = connect_owned(state, port, "reader");
    let protected = storage_state(&mut reader);
    let running_revision: String = reader
        .exec_first("SELECT revision FROM issues WHERE id = ?", (id(11),))
        .unwrap()
        .unwrap();
    for (target_area, target_issue, target_revision) in [
        (id(3), id(11), running_revision.clone()),
        (id(2), id(10), id(4)),
        (id(2), id(12), id(4)),
    ] {
        assert_eq!(
            update(
                &id(193),
                &target_area,
                &target_issue,
                &target_revision,
                &title_edit,
                title_values.clone(),
                4
            )["error"]["code"],
            "INVALID_TRANSITION"
        );
    }
    assert_eq!(storage_state(&mut reader), protected);
    let prioritised = update(
        &id(193),
        &area,
        &id(11),
        &running_revision,
        &["--priority", "2"],
        json!([null, null, null, 2, null]),
        0,
    );
    assert_eq!(prioritised["data"]["outcome"], "confirmed");
    let claim: (String, String) = reader
        .exec_first(
            "SELECT claim_id, holder_ref FROM issue_claims WHERE issue_id = ?",
            (id(11),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(claim, (id(14), "fixture-holder".into()));

    // A matching receipt and target edit cannot attest to a commit that changed another Issue.
    let forged_base = storage_state(&mut reader).0;
    let forged = id(194);
    let forged_revision = id(195);
    let priority_values = json!([null, null, null, 1, null]);
    let fingerprint = digest(
        json!([
            1,
            "issue.update",
            workspace,
            area,
            issue,
            actor,
            cleared_revision,
            priority_values,
        ])
        .to_string()
        .as_bytes(),
    );
    let mut forged_evidence: Value = serde_json::from_str(&receipt.6).unwrap();
    forged_evidence["request_id"] = json!(forged);
    forged_evidence["fingerprint"] = json!(fingerprint);
    forged_evidence["expected_issue_revision"] = json!(cleared_revision);
    let forged_result = json!({
        "version": 1, "area_id": area, "issue_id": issue,
        "issue_revision": forged_revision, "fields": ["priority"],
    });
    let timestamp = receipt.5.trim_end_matches('Z').replace('T', " ");
    let edit_with_receipt = |base: &str| {
        format!(
        "UPDATE issues SET priority = 1, revision = '{forged_revision}', updated_at = '{timestamp}' WHERE id = '{issue}';
         INSERT INTO operation_receipts (request_id, kind, fingerprint, actor_ref, authorization_ref,
             base_commit, workflow_evidence_json, result_json, created_at)
         VALUES ('{forged}', 'issue.update', '{fingerprint}', '{actor}', '{}',
             '{base}', '{forged_evidence}', '{forged_result}', '{timestamp}');",
        receipt.3,
    )
    };
    drop(reader);
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "{} UPDATE issues SET priority = 2 WHERE id = '{}';",
            edit_with_receipt(&forged_base),
            id(10),
        ))),
        None,
    );
    let mut reader = connect_owned(state, port, "reader");
    let forged_state = storage_state(&mut reader);
    assert_eq!(
        update(
            &forged,
            &area,
            &issue,
            cleared_revision,
            &["--priority", "1"],
            priority_values.clone(),
            8
        )["error"]["code"],
        "COMMIT_UNCERTAIN",
    );
    assert_eq!(storage_state(&mut reader), forged_state);
    assert!(!marker.exists());

    // The verified stop binds the forged marker itself. The extra Issue edit
    // still makes the underlying committed effect unverifiable.
    let forged_receipt = stored_receipt(&mut reader, &forged);
    let session: u64 = reader
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    drop(reader);
    let tampered = json!({
        "version": 2, "kind": "issue.update", "workspace_id": workspace,
        "launch_nonce": launch_nonce(state), "session_id": session, "request_id": forged,
        "base_commit": forged_base, "fingerprint": forged_receipt.2, "area_id": area,
        "issue_id": issue, "receipt_digest": digest(json!(forged_receipt).to_string().as_bytes()),
    });
    record_intent(state, &tampered);
    let result = run(&["workspace", "stop"], 0);
    assert_eq!(result["data"]["process_exit"], "pidfd_verified");
    assert_eq!(result["data"]["pending_request"], "recorded");
    let proof = state.join("stopped-request.json");
    let stop_proof = fs::read(&proof).unwrap();
    let dolt_bin = dolt.to_str().unwrap();
    let result = run(&["workspace", "recover", "--dolt-bin", dolt_bin], 8);
    assert_eq!(result["error"]["code"], "COMMIT_UNCERTAIN");
    assert_recovery_redacted(&result, &tampered);
    assert_eq!(fs::read_to_string(&marker).unwrap(), tampered.to_string());
    assert_eq!(fs::read(&proof).unwrap(), stop_proof);
    let start = ["workspace", "start", "--dolt-bin", dolt_bin, "--port", port];
    assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");

    // Restore the disposable branch for the next fixture.
    fs::remove_file(&marker).unwrap();
    run(&start, 0);
    assert_eq!(
        storage_state(&mut connect_owned(state, port, "reader")),
        forged_state
    );
    restart(Some(OfflineFixtureEdit::RestoreHead(&forged_base)), None);

    // This fixture commit changes only the target Issue and adds its matching
    // receipt, but the target's ancestor was archived at the edit's base.
    restart(Some(OfflineFixtureEdit::CommitSql(&archive_ancestor)), None);
    let archived_base = storage_state(&mut connect_owned(state, port, "reader")).0;
    restart(
        Some(OfflineFixtureEdit::CommitSql(&edit_with_receipt(
            &archived_base,
        ))),
        None,
    );
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "UPDATE areas SET archived_at = NULL WHERE id = '{}';",
            id(2)
        ))),
        None,
    );
    let mut reader = connect_owned(state, port, "reader");
    let after_restore = storage_state(&mut reader);
    assert_eq!(
        update(
            &forged,
            &area,
            &issue,
            cleared_revision,
            &["--priority", "1"],
            priority_values,
            8
        )["error"]["code"],
        "COMMIT_UNCERTAIN"
    );
    assert_eq!(storage_state(&mut reader), after_restore);
    assert!(!marker.exists());
    drop(reader);
    restart(Some(OfflineFixtureEdit::RestoreHead(&forged_base)), None);
    assert_eq!(
        storage_state(&mut connect_owned(state, port, "reader")).0,
        forged_base
    );
}

type Receipt = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn stored_receipt(connection: &mut Conn, request: &str) -> Receipt {
    connection.exec_first(
        "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
           FROM operation_receipts WHERE request_id = ?",
        (request,),
    ).unwrap().unwrap()
}

fn record_intent(state: &Path, intent: &Value) {
    let marker = state.join("pending-request.json");
    fs::write(&marker, intent.to_string()).unwrap();
    fs::set_permissions(&marker, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
}

fn assert_recovery_redacted(report: &Value, intent: &Value) {
    let serialized = report.to_string();
    for private in [
        "Synthetic revised body",
        "Synthetic acceptance",
        HOLD,
        ACTOR,
        "fixture-update-authorization",
        "receipt_digest",
        intent["receipt_digest"].as_str().unwrap(),
        intent["fingerprint"].as_str().unwrap(),
    ] {
        assert!(!serialized.contains(private));
    }
}

fn stopped_recovery(
    state: &Path,
    port: &str,
    dolt: &Path,
    run: &impl Fn(&[&str], i32) -> Value,
    committed: &Value,
) {
    let marker = state.join("pending-request.json");
    let proof = state.join("stopped-request.json");
    let dolt_bin = dolt.to_str().unwrap();
    let recover = ["workspace", "recover", "--dolt-bin", dolt_bin];
    let start = ["workspace", "start", "--dolt-bin", dolt_bin, "--port", port];
    let stop = || {
        let result = run(&["workspace", "stop"], 0);
        assert_eq!(result["data"]["process_exit"], "pidfd_verified");
        assert_eq!(result["data"]["pending_request"], "recorded");
        assert!(proof.exists());
    };
    let resolve = |outcome, intent: &Value| {
        let result = run(&recover, 0);
        assert_eq!(result["data"]["state"], "stopped");
        assert_eq!(result["data"]["process_exit"], "pidfd_verified");
        assert_eq!(result["data"]["outcome"], outcome);
        assert_eq!(result["data"]["pending_request"], "unrecorded");
        assert_recovery_redacted(&result, intent);
        assert!(!marker.exists() && !proof.exists() && !state.join("server.json").exists());
    };

    record_intent(state, committed);
    stop();
    assert_eq!(fs::read_to_string(&marker).unwrap(), committed.to_string());
    resolve("confirmed", committed);
    run(&start, 0);

    let issue = committed["issue_id"].as_str().unwrap();
    let prior_request = committed["request_id"].as_str().unwrap();
    let mut writer = connect_owned(state, port, "writer");
    let original = storage_state(&mut writer);
    let (session, revision, priority): (u64, String, u8) = writer
        .exec_first(
            "SELECT CONNECTION_ID(), revision, priority FROM issues WHERE id = ?",
            (issue,),
        )
        .unwrap()
        .unwrap();
    assert_eq!(priority, 0);
    let request = id(196);
    let new_revision = id(197);
    let prior_receipt = stored_receipt(&mut writer, prior_request);
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(
            json!([
                1,
                "issue.update",
                committed["workspace_id"],
                committed["area_id"],
                issue,
                ACTOR,
                revision,
                [null, null, null, 1, null],
            ])
            .to_string()
        )
    );
    let mut evidence: Value = serde_json::from_str(&prior_receipt.6).unwrap();
    evidence["request_id"] = json!(request);
    evidence["fingerprint"] = json!(fingerprint);
    evidence["expected_issue_revision"] = json!(revision);
    let result = json!({
        "version": 1, "area_id": committed["area_id"], "issue_id": issue,
        "issue_revision": new_revision, "fields": ["priority"],
    });
    let timestamp = prior_receipt.5.trim_end_matches('Z').replace('T', " ");

    writer.query_drop("START TRANSACTION").unwrap();
    writer.exec_drop(
        "UPDATE issues SET priority = 1, revision = ?, updated_at = ? WHERE id = ? AND revision = ?",
        (&new_revision, &timestamp, issue, &revision),
    ).unwrap();
    assert_eq!(writer.affected_rows(), 1);
    writer.exec_drop(
        "INSERT INTO operation_receipts (request_id, kind, fingerprint, actor_ref, authorization_ref,
             base_commit, workflow_evidence_json, result_json, created_at)
         VALUES (?, 'issue.update', ?, ?, ?, ?, ?, ?, ?)",
        (&request, &fingerprint, ACTOR, &prior_receipt.3, &original.0, evidence.to_string(), result.to_string(), &timestamp),
    ).unwrap();
    assert_eq!(writer.affected_rows(), 1);

    let staged_receipt = stored_receipt(&mut writer, &request);
    let mut rollback = committed.clone();
    rollback["launch_nonce"] = json!(launch_nonce(state));
    rollback["session_id"] = json!(session);
    rollback["request_id"] = json!(request);
    rollback["base_commit"] = json!(original.0);
    rollback["fingerprint"] = json!(fingerprint);
    rollback["receipt_digest"] = json!(format!(
        "{:x}",
        Sha256::digest(json!(staged_receipt).to_string())
    ));
    record_intent(state, &rollback);
    stop();
    drop(writer);
    resolve("rolled_back", &rollback);
    run(&start, 0);

    let mut reader = connect_owned(state, port, "reader");
    assert_eq!(storage_state(&mut reader), original);
    let observed: (String, u8, u64) = reader.exec_first(
        "SELECT revision, priority, (SELECT COUNT(*) FROM operation_receipts WHERE request_id = ?)
         FROM issues WHERE id = ?", (&request, issue),
    ).unwrap().unwrap();
    assert_eq!(observed, (revision, priority, 0));
}

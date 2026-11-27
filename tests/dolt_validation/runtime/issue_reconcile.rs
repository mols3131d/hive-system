use super::*;
use serde_json::json;

const ACTOR: &str = "fixture-reconciler";
const REASON: &str = "  Synthetic decision λ\r\n";

pub(super) fn exercise(
    state: &Path,
    root: &Path,
    port: &str,
    dolt: &Path,
    run: &impl Fn(&[&str], i32) -> Value,
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
) {
    let workspace = id(1);
    let area = id(3);
    let cancelled = id(221);
    let prerequisite = id(222);
    let parent = id(223);
    let running = id(224);
    let reviewing = id(225);
    let accepted = id(226);
    let child = id(227);
    let review_parent = id(228);
    let revision = id(4);
    let marker = state.join("pending-request.json");
    let connect = |role| connect_owned(state, port, role);
    let initial = storage_state(&mut connect("reader")).0;
    let edit = |sql: &str| restart(Some(OfflineFixtureEdit::CommitSql(sql)), None);
    edit(&format!(
        "INSERT INTO issues (id, number, area_id, parent_id, title, type, status, revision,
             acceptance_criteria, hold_reason, created_at, updated_at, closed_at, closed_by_ref, resolution_note) VALUES
         ('{cancelled}', 221, '{area}', NULL, 'Abandoned', 'task', 'backlog', '{revision}', 'Criteria', 'Held',
             UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), NULL, NULL, NULL),
         ('{parent}', 223, '{area}', NULL, 'Parent', 'epic', 'done', '{revision}', 'Criteria', NULL,
             UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), 'fixture-reviewer', 'Accepted'),
         ('{prerequisite}', 222, '{area}', '{parent}', 'Prerequisite', 'task', 'done', '{revision}', 'Criteria', NULL,
             UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), 'fixture-reviewer', 'Accepted'),
         ('{running}', 224, '{area}', NULL, 'Running', 'task', 'in-progress', '{revision}', 'Criteria', NULL,
             UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), NULL, NULL, NULL),
         ('{reviewing}', 225, '{area}', NULL, 'Reviewing', 'task', 'review', '{revision}', 'Criteria', NULL,
             UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), NULL, NULL, NULL),
         ('{accepted}', 226, '{area}', NULL, 'Accepted', 'task', 'done', '{revision}', 'Criteria', NULL,
             UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), 'fixture-reviewer', 'Accepted'),
         ('{review_parent}', 228, '{area}', NULL, 'Review parent', 'feature', 'review', '{revision}', 'Criteria', NULL,
             UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), NULL, NULL, NULL),
         ('{child}', 227, '{area}', '{review_parent}', 'Child', 'task', 'todo', '{revision}', 'Criteria', NULL,
             UTC_TIMESTAMP(6), UTC_TIMESTAMP(6), NULL, NULL, NULL);
         INSERT INTO issue_dependencies VALUES ('{running}', '{prerequisite}'),
             ('{reviewing}', '{prerequisite}'), ('{accepted}', '{prerequisite}');
         INSERT INTO issue_claims VALUES ('{running}', '{}', 'fixture-execution', UTC_TIMESTAMP(6));",
        id(229)
    ));

    let digest = |bytes: &str| format!("{:x}", Sha256::digest(bytes));
    let decision_path = root.join("reconcile-decision.json");
    let decision = |action: &str, issue: &str, rev: &str, request: &str, reason: &str| {
        let kind = format!("issue.{action}");
        let fingerprint = digest(
            &json!([1, kind, workspace, area, issue, ACTOR, rev, digest(reason)]).to_string(),
        );
        json!({"decision": "allow", "evidence": {
            "version": 1, "operation": kind, "workspace_id": workspace, "area_id": area,
            "issue_id": issue, "actor_ref": ACTOR, "request_id": request,
            "expected_issue_revision": rev, "fingerprint": fingerprint,
            "authorization_ref": "fixture-decision", "evidence_id": "fixture-evidence",
            "revision": "r1", "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
            "valid_from": "2000-01-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
            "withdrawn": false
        }})
    };
    let command = |action: &str, issue: &str, rev: &str, request: &str, reason: &str, exit| {
        fs::write(
            &decision_path,
            decision(action, issue, rev, request, reason).to_string(),
        )
        .unwrap();
        let report = run(
            &[
                "issue",
                action,
                issue,
                "--area",
                &area,
                "--actor",
                ACTOR,
                "--if-match",
                &format!("{issue}={rev}"),
                "--reason",
                reason,
                "--request-id",
                request,
                "--workspace-id",
                &workspace,
                "--database",
                "db",
                "--port",
                port,
                "--evidence-file",
                decision_path.to_str().unwrap(),
            ],
            exit,
        );
        assert_eq!(report["command"], format!("issue {action}"));
        assert_eq!(report["schema_version"], 2);
        assert!(!report.to_string().contains("Synthetic decision"));
        report
    };
    let refuse = |action: &str, issue: &str, rev: &str, request: &str, reason: &str, exit, code| {
        let mut reader = connect("reader");
        let before = storage_state(&mut reader);
        let report = command(action, issue, rev, request, reason, exit);
        assert_eq!(report["error"]["code"], code, "{report}");
        assert_eq!(storage_state(&mut reader), before);
        assert!(!marker.exists());
    };
    refuse(
        "cancel",
        &cancelled,
        &revision,
        &id(230),
        "　",
        2,
        "INVALID_ARGUMENT",
    );
    refuse(
        "cancel",
        &cancelled,
        &revision,
        &id(230),
        &"λ".repeat(2049),
        2,
        "INVALID_ARGUMENT",
    );
    refuse(
        "cancel",
        &child,
        &revision,
        &id(231),
        REASON,
        4,
        "INVALID_TRANSITION",
    );
    refuse(
        "cancel",
        &running,
        &revision,
        &id(232),
        REASON,
        5,
        "CLAIM_CONFLICT",
    );
    refuse(
        "reopen",
        &prerequisite,
        &revision,
        &id(233),
        REASON,
        4,
        "INVALID_TRANSITION",
    );
    assert_eq!(
        command("reopen", &parent, &revision, &id(234), REASON, 0)["data"]["outcome"],
        "confirmed"
    );
    refuse(
        "reopen",
        &prerequisite,
        &revision,
        &id(233),
        REASON,
        4,
        "INVALID_TRANSITION",
    );
    command("reopen", &accepted, &revision, &id(235), REASON, 0);
    command("cancel", &reviewing, &revision, &id(236), REASON, 0);
    // The remaining dependent still owns a Claim. Model a separately verified release
    // with an offline synthetic edit because this change does not implement release.
    refuse(
        "reopen",
        &prerequisite,
        &revision,
        &id(233),
        REASON,
        4,
        "INVALID_TRANSITION",
    );
    let released = id(237);
    edit(&format!(
        "DELETE FROM issue_claims WHERE issue_id = '{running}';
         UPDATE issues SET status = 'todo', revision = '{released}' WHERE id = '{running}';"
    ));
    command("cancel", &running, &released, &id(232), REASON, 0);
    assert_eq!(
        command("reopen", &prerequisite, &revision, &id(233), REASON, 0)["data"]["result"]["status"],
        "todo"
    );

    let request = id(238);
    let mut reader = connect("reader");
    let before = storage_state(&mut reader).0;
    let closed = command("cancel", &cancelled, &revision, &request, REASON, 0);
    let result = &closed["data"]["result"];
    assert_eq!(result["status"], "cancelled");
    assert!(result.get("claim_id").is_none());
    let cancel_revision = result["issue_revision"].as_str().unwrap();
    let transition = result["transition_id"].as_str().unwrap();
    let receipt = stored_receipt(&mut reader, &request);
    assert_eq!(receipt.4, before);
    let old_contents: String = reader.exec_first(
        format!("SELECT JSON_ARRAY(number, area_id, parent_id, type, title, body, acceptance_criteria,
                        priority, hold_reason, created_at) FROM issues AS OF '{before}' WHERE id = ?"),
        (&cancelled,),
    ).unwrap().unwrap();
    let stored: (String, String, String, String, String, String, String) = reader
        .exec_first(
            "SELECT i.status, DATE_FORMAT(i.closed_at, '%Y-%m-%dT%H:%i:%s.%fZ'), i.closed_by_ref,
                i.resolution_note, u.body, u.transition_json, i.revision
         FROM issues i JOIN issue_updates u ON u.issue_id = i.id WHERE i.id = ? AND u.id = ?",
            (&cancelled, transition),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        stored,
        (
            "cancelled".into(),
            receipt.5.clone(),
            ACTOR.into(),
            REASON.into(),
            REASON.into(),
            receipt.7.clone(),
            cancel_revision.into()
        )
    );
    let typed: Value = serde_json::from_str(&receipt.7).unwrap();
    assert_eq!(typed["previous_status"], "backlog");
    assert_eq!(typed["previous_issue_revision"], revision);
    assert_eq!(typed["issue_revision"], cancel_revision);
    assert_eq!(
        typed["effect"],
        json!({"operation": "issue.cancel", "reason_sha256": digest(REASON)})
    );
    assert_eq!(typed["evidence"]["authorization_ref"], receipt.3);

    let prior = connect("writer");
    let session: u64 = prior.connection_id().into();
    drop(prior);
    let intent = json!({
        "version": 4, "kind": "issue.cancel", "workspace_id": workspace,
        "launch_nonce": launch_nonce(state), "session_id": session, "request_id": request,
        "base_commit": before, "fingerprint": receipt.2, "area_id": area, "issue_id": cancelled,
        "receipt_digest": digest(&json!(receipt).to_string()),
    });
    let stop_recover = |intent: &Value, expected: &str| {
        record_intent(state, intent);
        run(&["workspace", "stop"], 0);
        assert_eq!(
            run(
                &["workspace", "recover", "--dolt-bin", dolt.to_str().unwrap()],
                0
            )["data"]["outcome"],
            expected
        );
        assert!(!marker.exists());
        run(
            &[
                "workspace",
                "start",
                "--dolt-bin",
                dolt.to_str().unwrap(),
                "--port",
                port,
            ],
            0,
        );
    };

    drop(reader);
    stop_recover(&intent, "confirmed");
    refuse(
        "reopen",
        &cancelled,
        &revision,
        &id(239),
        REASON,
        5,
        "STALE_REVISION",
    );

    let reopen_request = id(239);
    let mut writer = connect("writer");
    let mut rollback = intent;
    rollback["kind"] = json!("issue.reopen");
    rollback["launch_nonce"] = json!(launch_nonce(state));
    rollback["session_id"] = json!(writer.connection_id());
    rollback["request_id"] = json!(reopen_request);
    rollback["base_commit"] = closed["data"]["confirmed_commit"].clone();
    rollback["fingerprint"] = decision(
        "reopen",
        &cancelled,
        cancel_revision,
        &reopen_request,
        REASON,
    )["evidence"]["fingerprint"]
        .clone();
    writer.query_drop("START TRANSACTION").unwrap();
    writer.exec_drop(
        "UPDATE issues SET status = 'todo', closed_at = NULL, closed_by_ref = NULL, resolution_note = NULL
          WHERE id = ?", (&cancelled,)
    ).unwrap();
    drop(writer);
    stop_recover(&rollback, "rolled_back");
    let mut reader = connect("reader");
    let before_retry = storage_state(&mut reader).0;
    assert_eq!(before_retry, closed["data"]["confirmed_commit"]);

    let reopened = command(
        "reopen",
        &cancelled,
        cancel_revision,
        &reopen_request,
        REASON,
        0,
    );
    let reopened_result = &reopened["data"]["result"];
    let fields: (String, String, bool, String, u64) = reader.exec_first(
        "SELECT status, revision, closed_at IS NULL AND closed_by_ref IS NULL AND resolution_note IS NULL,
                JSON_ARRAY(number, area_id, parent_id, type, title, body, acceptance_criteria, priority, hold_reason, created_at),
                (SELECT COUNT(*) FROM issue_claims WHERE issue_id = ?)
                FROM issues WHERE id = ?",
        (&cancelled, &cancelled)
    ).unwrap().unwrap();
    assert_eq!(
        fields,
        (
            "todo".into(),
            reopened_result["issue_revision"].as_str().unwrap().into(),
            true,
            old_contents,
            0
        )
    );
    let replay = command("cancel", &cancelled, &revision, &request, REASON, 0);
    assert_eq!(replay["data"]["outcome"], "replayed");
    assert_eq!(replay["data"]["result"], *result);
    refuse(
        "cancel",
        &cancelled,
        &revision,
        &request,
        "Different reason",
        5,
        "REQUEST_CONFLICT",
    );

    let head = storage_state(&mut reader).0;
    let invocation = id(240);
    let proof = root.join("reconcile-operation.json");
    fs::write(
        &proof,
        json!({
            "version": 1, "evidence_id": "fixture-inspection", "revision": "r1",
            "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
            "workspace_id": workspace, "actor_ref": ACTOR, "area_id": area, "issue_id": cancelled,
            "operations": ["inspect_operation_receipt"],
            "record": {"kind": "operation_receipt", "id": request, "revision": head},
            "invocation_id": invocation, "db_commit": head, "valid_from": "2000-01-01T00:00:00Z",
            "expires_at": "2099-01-01T00:00:00Z", "withdrawn": false
        })
        .to_string(),
    )
    .unwrap();
    let inspected = run(
        &[
            "operation",
            "show",
            &request,
            "--area",
            &area,
            "--issue",
            &cancelled,
            "--actor",
            ACTOR,
            "--invocation-id",
            &invocation,
            "--workspace-id",
            &workspace,
            "--database",
            "db",
            "--port",
            port,
            "--evidence-file",
            proof.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(inspected["data"]["result"], *result);
    assert_eq!(inspected["data"]["current_claim"], Value::Null);
    assert_eq!(storage_state(&mut reader).0, head);
    drop(reader);

    // A matching typed update and receipt cannot establish a different closure outcome.
    restart(Some(OfflineFixtureEdit::RestoreHead(&before)), None);
    let timestamp = receipt.5.trim_end_matches('Z').replace('T', " ");
    let reason_hex: String = REASON.bytes().map(|byte| format!("{byte:02x}")).collect();
    edit(&format!(
        "UPDATE issues SET status = 'cancelled', revision = '{cancel_revision}', updated_at = '{timestamp}',
         closed_at = '{timestamp}', closed_by_ref = '{ACTOR}', resolution_note = 'Different closure' WHERE id = '{cancelled}';
         INSERT INTO issue_updates VALUES ('{transition}', '{cancelled}', 'transition',
            CONVERT(UNHEX('{reason_hex}') USING utf8mb4), '{ACTOR}', '{timestamp}', '{}');
         INSERT INTO operation_receipts VALUES ('{request}', 'issue.cancel', '{}', '{ACTOR}', '{}',
            '{}', '{before}', '{}', '{timestamp}');",
        receipt.7, receipt.2, receipt.3, receipt.6, receipt.7,
    ));
    refuse(
        "cancel",
        &cancelled,
        &revision,
        &request,
        REASON,
        8,
        "COMMIT_UNCERTAIN",
    );
    restart(Some(OfflineFixtureEdit::RestoreHead(&initial)), None);
}

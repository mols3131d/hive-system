use super::*;
use serde_json::json;

const ACTOR: &str = "fixture-planner";
const HOLDER: &str = "fixture-execution";

pub(super) fn exercise(
    state: &Path,
    root: &Path,
    port: &str,
    dolt: &Path,
    invoke: &(impl Fn(&[&str]) -> io::Result<std::process::Output> + Sync),
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
) {
    let workspace = id(1);
    let area = id(3);
    let task = id(201);
    let epic = id(202);
    let blank = id(203);
    let prerequisite = id(204);
    let revision = id(4);
    let marker = state.join("pending-request.json");
    let edit = |sql: &str| restart(Some(OfflineFixtureEdit::CommitSql(sql)), None);
    let mut reader = connect_owned(state, port, "reader");
    let initial = storage_state(&mut reader).0;
    drop(reader);
    let baseline = std::env::var_os("HIVE_ISSUE_TRANSITION_BASELINE_BIN");
    restart(Some(OfflineFixtureEdit::CommitSql(&format!(
        "INSERT INTO issues (id, number, area_id, title, type, status, revision, acceptance_criteria, hold_reason, created_at, updated_at) VALUES
         ('{task}', 201, '{area}', 'Task', 'task', 'backlog', '{revision}', 'Synthetic criteria', 'Deferred', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
         ('{epic}', 202, '{area}', 'Epic', 'epic', 'backlog', '{revision}', 'Synthetic criteria', NULL, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
         ('{blank}', 203, '{area}', 'Blank', 'bug', 'backlog', '{revision}', '　', NULL, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
         ('{prerequisite}', 204, '{area}', 'Dependency', 'task', 'todo', '{revision}', 'Synthetic criteria', NULL, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6));
         INSERT INTO issue_dependencies VALUES ('{task}', '{prerequisite}');"
    ))), baseline.as_deref());

    let digest = |bytes: &str| format!("{:x}", Sha256::digest(bytes));
    let command = |action: &str, issue: &str, rev: &str, request: &str, holder: Option<&str>| {
        let kind = format!("issue.{action}");
        let now = fixture_now();
        let evidence = json!({
            "decision": "allow",
            "evidence": {
                "version": 1, "operation": kind, "workspace_id": workspace,
                "area_id": area, "issue_id": issue, "actor_ref": ACTOR,
                "request_id": request, "expected_issue_revision": rev, "holder_ref": holder,
                "fingerprint": digest(&json!([1, kind, workspace, area, issue, ACTOR, rev, holder]).to_string()),
                "authorization_ref": "fixture-transition-decision",
                "evidence_id": "fixture-transition-evidence", "revision": "r1",
                "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                "valid_from": fixture_timestamp(now - time::Duration::minutes(1)),
                "expires_at": fixture_timestamp(now + time::Duration::hours(1)), "withdrawn": false,
            }
        });
        let path = root.join(format!("decision-{request}.json"));
        let mut args = [
            "issue",
            action,
            issue,
            "--area",
            &area,
            "--actor",
            ACTOR,
            "--if-match",
            &format!("{issue}={rev}"),
            "--request-id",
            request,
            "--workspace-id",
            &workspace,
            "--database",
            "db",
            "--port",
            port,
            "--evidence-file",
            path.to_str().unwrap(),
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(holder) = holder {
            args.extend(["--holder".to_owned(), holder.to_owned()]);
        }
        fs::write(&path, evidence.to_string()).unwrap();
        (args, path, evidence)
    };
    let run = |args: &[&str], exit| {
        let (status, report) = decode(invoke(args).unwrap());
        assert_eq!(status, exit, "{args:?}: {report}");
        report
    };
    let run_command =
        |args: &[String], exit| run(&args.iter().map(String::as_str).collect::<Vec<_>>(), exit);

    let mut reader = connect_owned(state, port, "reader");
    let before = storage_state(&mut reader);
    let (args, _, _) = command("select", &blank, &revision, &id(205), None);
    assert_eq!(run_command(&args, 4)["error"]["code"], "INVALID_TRANSITION");
    assert_eq!(storage_state(&mut reader), before);
    assert!(!marker.exists());

    let (args, _, _) = command("select", &epic, &revision, &id(206), None);
    let selected_epic = run_command(&args, 0);
    let epic_rev = selected_epic["data"]["result"]["issue_revision"]
        .as_str()
        .unwrap();
    let (args, _, _) = command("claim", &epic, epic_rev, &id(207), Some(HOLDER));
    if baseline.is_some() {
        let report = run_command(&args, 7);
        assert_eq!(report["error"]["code"], "RUNTIME_UNAVAILABLE");
        assert!(
            report["error"]["message"]
                .as_str()
                .unwrap()
                .contains("workspace start")
        );
        drop(reader);
        restart(None, None);
        reader = connect_owned(state, port, "reader");
    }
    assert_eq!(run_command(&args, 4)["error"]["code"], "INVALID_TRANSITION");

    let (args, path, decision) = command("select", &task, &revision, &id(208), None);
    let mut expired = decision.clone();
    expired["evidence"]["expires_at"] = expired["evidence"]["valid_from"].clone();
    fs::write(&path, expired.to_string()).unwrap();
    let before = storage_state(&mut reader);
    assert_eq!(
        run_command(&args, 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    assert_eq!(storage_state(&mut reader), before);

    fs::write(&path, decision.to_string()).unwrap();
    let selected = run_command(&args, 0);
    assert_eq!(selected["data"]["result"]["status"], "todo");
    assert!(selected["data"]["result"].get("claim_id").is_none());
    let todo = selected["data"]["result"]["issue_revision"]
        .as_str()
        .unwrap();
    let request_a = id(209);
    let request_b = id(210);
    let (a, path_a, evidence_a) = command("claim", &task, todo, &request_a, Some(HOLDER));
    let (b, path_b, evidence_b) = command(
        "claim",
        &task,
        todo,
        &request_b,
        Some("fixture-execution-other"),
    );
    let held = storage_state(&mut reader);
    let blocked = run_command(&a, 4);
    assert_eq!(blocked["error"]["code"], "BLOCKED");
    assert_eq!(
        blocked["error"]["details"]["blockers"],
        json!(["HOLD", "PREREQUISITE_NOT_DONE"])
    );
    assert_eq!(storage_state(&mut reader), held);
    assert!(!marker.exists());

    drop(reader);
    edit(&format!(
        "UPDATE issues SET hold_reason = NULL WHERE id = '{task}';
         UPDATE issues SET status = 'done', closed_at = UTC_TIMESTAMP(6), closed_by_ref = 'fixture-reviewer',
            resolution_note = '　' WHERE id = '{prerequisite}';"
    ));
    let mut reader = connect_owned(state, port, "reader");
    let corrupt = storage_state(&mut reader);
    assert_eq!(run_command(&a, 4)["error"]["code"], "INVALID_RECORD");
    assert_eq!(storage_state(&mut reader), corrupt);
    drop(reader);
    edit(&format!(
        "UPDATE issues SET resolution_note = 'Accepted' WHERE id = '{prerequisite}';"
    ));

    let (stale, _, _) = command("claim", &task, &revision, &id(212), Some(HOLDER));
    let mut reader = connect_owned(state, port, "reader");
    let ready = storage_state(&mut reader);
    assert_eq!(run_command(&stale, 5)["error"]["code"], "STALE_REVISION");
    assert_eq!(storage_state(&mut reader), ready);
    assert!(!marker.exists());

    let mut wrong_holder = evidence_a.clone();
    wrong_holder["evidence"]["holder_ref"] = json!(ACTOR);
    fs::write(&path_a, wrong_holder.to_string()).unwrap();
    assert_eq!(run_command(&a, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(storage_state(&mut reader), ready);
    fs::write(&path_a, evidence_a.to_string()).unwrap();

    let barrier = std::sync::Barrier::new(2);
    let race = |args: &[String]| {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        barrier.wait();
        decode(invoke(&args).unwrap())
    };
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| race(&a));
        let right = race(&b);
        (left.join().unwrap(), right)
    });
    let (winner, loser, args, path, decision, request, holder) = if left.0 == 0 {
        (left, right, &a, &path_a, evidence_a, &request_a, HOLDER)
    } else {
        (
            right,
            left,
            &b,
            &path_b,
            evidence_b,
            &request_b,
            "fixture-execution-other",
        )
    };
    assert_eq!(winner.0, 0, "{winner:?}, {loser:?}");
    assert_eq!(loser.0, 5, "{loser:?}");
    assert!(matches!(
        loser.1["error"]["code"].as_str(),
        Some("LOCK_BUSY" | "CLAIM_CONFLICT")
    ));
    let result = &winner.1["data"]["result"];
    assert_eq!(winner.1["data"]["outcome"], "confirmed");
    assert_eq!(winner.1["data"]["base_commit"], ready.0);
    let after = storage_state(&mut reader);
    assert_eq!(winner.1["data"]["confirmed_commit"], after.0);
    let stored: (String, String, String, String, String, String) = reader
        .exec_first(
            "SELECT i.status, i.revision, c.claim_id, c.holder_ref, u.author_ref, u.transition_json
         FROM issues i JOIN issue_claims c ON c.issue_id = i.id
         JOIN issue_updates u ON u.issue_id = i.id WHERE i.id = ? AND u.id = ?",
            (&task, result["transition_id"].as_str().unwrap()),
        )
        .unwrap()
        .unwrap();
    assert_eq!(stored.0, "in-progress");
    assert_eq!(stored.1, result["issue_revision"]);
    assert_eq!(stored.2, result["claim_id"]);
    assert_eq!(stored.3, holder);
    assert_eq!(stored.4, ACTOR);
    let typed: Value = serde_json::from_str(&stored.5).unwrap();
    assert_eq!(typed["previous_issue_revision"], todo);
    assert_eq!(typed["previous_status"], "todo");
    assert_eq!(typed["effect"]["holder_ref"], holder);
    let receipts: u64 = reader
        .exec_first(
            "SELECT COUNT(*) FROM operation_receipts WHERE request_id IN (?, ?)",
            (&request_a, &request_b),
        )
        .unwrap()
        .unwrap();
    assert_eq!(receipts, 1);

    let receipt = stored_receipt(&mut reader, request);
    fs::write(path, &receipt.6).unwrap();
    assert_eq!(
        run_command(args, 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    let mut handed_off = decision;
    handed_off["evidence"]["revision"] = json!("r2");
    fs::write(path, handed_off.to_string()).unwrap();

    let mut observer = connect_owned(state, port, "writer");
    let mut prior = connect_owned(state, port, "writer");
    let session: u64 = prior
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    let intent = json!({
        "version": 4, "kind": "issue.claim", "workspace_id": workspace,
        "launch_nonce": launch_nonce(state), "session_id": session, "request_id": request,
        "base_commit": ready.0, "fingerprint": receipt.2, "area_id": area, "issue_id": task,
        "receipt_digest": digest(&json!(receipt).to_string()),
    });
    record_intent(state, &intent);
    assert_eq!(run_command(args, 8)["error"]["code"], "COMMIT_UNCERTAIN");
    drop(prior);
    wait_for_session_exit(&mut observer, session);
    let replay = run_command(args, 0);
    assert_eq!(replay["data"]["outcome"], "replayed");
    assert_eq!(replay["data"]["result"], *result);
    assert_eq!(storage_state(&mut reader), after);
    assert!(!marker.exists());

    drop(observer);
    drop(reader);
    record_intent(state, &intent);
    run(&["workspace", "stop"], 0);
    let recovered = run(
        &["workspace", "recover", "--dolt-bin", dolt.to_str().unwrap()],
        0,
    );
    assert_eq!(recovered["data"]["outcome"], "confirmed");
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

    let claim_again = |revision: &str, request: String| {
        let (args, _, _) = command("claim", &task, revision, &request, Some(holder));
        run_command(&args, 0)["data"]["result"].clone()
    };
    exercise_stop(
        state,
        root,
        port,
        dolt,
        &task,
        result,
        &run,
        restart,
        claim_again,
    );

    let mut reader = connect_owned(state, port, "reader");
    let settled = storage_state(&mut reader);
    let replay = run_command(args, 0);
    assert_eq!(replay["data"]["outcome"], "replayed");
    assert_eq!(replay["data"]["result"], *result);
    assert_eq!(replay["context"]["db_commit"], settled.0);
    assert_eq!(storage_state(&mut reader), settled);
    drop(reader);
    restart(Some(OfflineFixtureEdit::RestoreHead(&initial)), None);
}

#[allow(clippy::too_many_arguments)]
fn exercise_stop(
    state: &Path,
    root: &Path,
    port: &str,
    dolt: &Path,
    issue: &str,
    first_claim: &Value,
    run: &impl Fn(&[&str], i32) -> Value,
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
    claim_again: impl Fn(&str, String) -> Value,
) {
    let workspace = id(1);
    let area = id(3);
    let holder = first_claim["holder_ref"].as_str().unwrap();
    let reason = "Synthetic stopped execution";
    let result_body = "Synthetic submission café\r\n";
    let result_file = root.join("submission.md");
    fs::write(&result_file, result_body).unwrap();
    let decision_file = root.join("stop-decision.json");
    let digest = |body: &str| format!("{:x}", Sha256::digest(body));
    let observation_time = || {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let second = time::OffsetDateTime::from_unix_timestamp(now.as_secs() as i64).unwrap();
        let timestamp = fixture_timestamp(second);
        format!("{}.{:06}Z", &timestamp[..19], now.subsec_micros())
    };
    let stopped = |claim, observed| {
        json!({
            "version": 1, "kind": "execution_stopped", "evidence_ref": "fixture-stop",
            "workspace_id": workspace, "issue_id": issue, "claim_id": claim, "holder_ref": holder,
            "source_ref": "fixture-host-observation", "observed_at": observed,
        })
    };
    let run_stop = |action: &str, rev: &str, request: &str, stop: Value, reviewed: bool, exit| {
        let now = fixture_now();
        let actor = if action == "recover" {
            "fixture-owner"
        } else {
            holder
        };
        let operation = format!("issue.{action}");
        let result_ref = (action == "submit").then_some("fixture-result");
        let body = if result_ref.is_some() {
            result_body
        } else {
            reason
        };
        let fingerprint = digest(
            &json!([
                1,
                operation,
                workspace,
                area,
                issue,
                actor,
                rev,
                stop,
                digest(body),
                result_ref,
            ])
            .to_string(),
        );
        let mut evidence = json!({
            "version": 1, "operation": operation, "workspace_id": workspace,
            "area_id": area, "issue_id": issue, "actor_ref": actor,
            "request_id": request, "expected_issue_revision": rev, "holder_ref": holder,
            "fingerprint": fingerprint, "stop": stop,
            "authorization_ref": "fixture-request-decision",
            "evidence_id": "fixture-decision-evidence", "revision": "r1",
            "source_ref": "fixture-source", "issuer_ref": "fixture-owner",
            "valid_from": fixture_timestamp(now - time::Duration::minutes(1)),
            "expires_at": fixture_timestamp(now + time::Duration::hours(1)), "withdrawn": false,
        });
        if reviewed {
            evidence["recovery_review"] = json!({
                "role": "owner", "reviewer_ref": "fixture-owner", "reviewed_at": observation_time(),
            });
        }
        fs::write(
            &decision_file,
            json!({"decision": "allow", "evidence": evidence}).to_string(),
        )
        .unwrap();
        let expected = format!("{issue}={rev}");
        let mut args = vec![
            "issue",
            action,
            issue,
            "--area",
            &area,
            "--actor",
            actor,
            "--if-match",
            &expected,
            "--request-id",
            request,
            "--workspace-id",
            &workspace,
            "--database",
            "db",
            "--port",
            port,
            "--evidence-file",
            decision_file.to_str().unwrap(),
            "--holder",
            holder,
            "--claim",
            stop["claim_id"].as_str().unwrap(),
            "--stop-evidence",
            "fixture-stop",
        ];
        if let Some(reference) = result_ref {
            args.extend([
                "--evidence",
                reference,
                "--file",
                result_file.to_str().unwrap(),
            ]);
        } else {
            args.extend(["--reason", reason]);
        }
        run(&args, exit)
    };

    let revision = |claim: &Value| claim["issue_revision"].as_str().unwrap().to_owned();
    let current_stop = |claim: &Value| {
        stopped(
            claim["claim_id"].as_str().unwrap().to_owned(),
            observation_time(),
        )
    };
    let first_revision = revision(first_claim);
    let release_request = id(220);
    let first_stop = current_stop(first_claim);
    let release = |exit| {
        run_stop(
            "release",
            &first_revision,
            &release_request,
            first_stop.clone(),
            false,
            exit,
        )
    };
    if let Some(base) = std::env::var_os("HIVE_ISSUE_STOP_BASELINE_BIN") {
        restart(None, Some(&base));
        let old = release(7);
        assert_eq!(old["error"]["code"], "RUNTIME_UNAVAILABLE");
        assert!(
            old["error"]["message"]
                .as_str()
                .unwrap()
                .contains("workspace start")
        );
        assert!(!state.join("pending-request.json").exists());
        restart(None, None);
    }
    let mut reader = connect_owned(state, port, "reader");
    let before = storage_state(&mut reader);
    for bad in [
        stopped(
            first_claim["claim_id"].as_str().unwrap().to_owned(),
            "2020-01-01T00:00:00Z".to_owned(),
        ),
        stopped(
            first_claim["claim_id"].as_str().unwrap().to_owned(),
            fixture_timestamp(fixture_now() + time::Duration::hours(1)),
        ),
    ] {
        assert_eq!(
            run_stop("release", &first_revision, &release_request, bad, false, 7)["error"]["code"],
            "AUTHORITY_UNVERIFIED"
        );
    }
    let mut event = first_stop.clone();
    event["kind"] = json!("session_end");
    assert_eq!(
        run_stop(
            "release",
            &first_revision,
            &release_request,
            event,
            false,
            7
        )["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    let mut unresolved = first_stop.clone();
    unresolved["evidence_ref"] = json!("fixture-other-stop");
    assert_eq!(
        run_stop(
            "release",
            &first_revision,
            &release_request,
            unresolved,
            false,
            7
        )["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    assert_eq!(storage_state(&mut reader), before);

    let released = release(0);
    let result = &released["data"]["result"];
    assert_eq!(result["status"], "todo");
    assert_eq!(result["former_claim_id"], first_claim["claim_id"]);
    assert_eq!(result["former_holder_ref"], holder);
    let second = claim_again(&revision(result), id(221));
    assert_ne!(second["claim_id"], first_claim["claim_id"]);
    assert_eq!(release(0)["data"]["outcome"], "replayed");
    let second_revision = revision(&second);
    let recover_request = id(222);
    let second_stop = current_stop(&second);
    let recover = |exit| {
        run_stop(
            "recover",
            &second_revision,
            &recover_request,
            second_stop.clone(),
            true,
            exit,
        )
    };
    assert_eq!(
        run_stop(
            "recover",
            &second_revision,
            &recover_request,
            first_stop,
            true,
            5
        )["error"]["code"],
        "CLAIM_CONFLICT"
    );
    assert_eq!(
        run_stop(
            "recover",
            &first_revision,
            &recover_request,
            second_stop.clone(),
            true,
            5
        )["error"]["code"],
        "STALE_REVISION"
    );
    assert_eq!(
        run_stop(
            "recover",
            &second_revision,
            &recover_request,
            second_stop.clone(),
            false,
            7
        )["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );

    let mut writer = connect_owned(state, port, "writer");
    let session: u64 = writer
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    let base = storage_state(&mut reader);
    let mut intent = json!({
        "version": 4, "kind": "issue.recover", "workspace_id": workspace,
        "launch_nonce": launch_nonce(state), "session_id": session, "request_id": recover_request,
        "base_commit": base.0, "area_id": area, "issue_id": issue,
        "fingerprint": digest("fixture rollback"), "receipt_digest": digest("fixture absent receipt"),
    });
    writer.query_drop("START TRANSACTION").unwrap();
    writer
        .exec_drop("DELETE FROM issue_claims WHERE issue_id = ?", (issue,))
        .unwrap();
    record_intent(state, &intent);
    assert_eq!(recover(8)["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(storage_state(&mut reader), base);
    drop(writer);
    let mut observer = connect_owned(state, port, "writer");
    wait_for_session_exit(&mut observer, session);
    drop(observer);
    let recovered = recover(0);
    assert_eq!(recovered["data"]["result"]["status"], "todo");
    assert_eq!(
        recovered["data"]["result"]["former_claim_id"],
        second["claim_id"]
    );
    let recovery_replay = recover(0);
    assert_eq!(recovery_replay["data"]["outcome"], "replayed");
    assert_eq!(
        recovery_replay["data"]["result"],
        recovered["data"]["result"]
    );
    let third = claim_again(&revision(&recovered["data"]["result"]), id(223));
    let third_revision = revision(&third);
    let submit_request = id(224);
    let third_stop = current_stop(&third);
    let submit = |exit| {
        run_stop(
            "submit",
            &third_revision,
            &submit_request,
            third_stop.clone(),
            false,
            exit,
        )
    };
    drop(reader);
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "UPDATE issues SET status = 'cancelled' WHERE id = '{}';",
            id(204),
        ))),
        None,
    );
    let blocked = submit(4);
    assert_eq!(
        blocked["error"]["details"]["blockers"],
        json!(["PREREQUISITE_NOT_DONE"])
    );
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "UPDATE issues SET status = 'done' WHERE id = '{}';",
            id(204),
        ))),
        None,
    );
    let mut reader = connect_owned(state, port, "reader");
    let submitted = submit(0);
    let result = &submitted["data"]["result"];
    assert_eq!(result["status"], "review");
    assert_eq!(result["previous_issue_revision"], third_revision);
    assert_eq!(result["previous_status"], "in-progress");
    assert_eq!(result["former_claim_id"], third["claim_id"]);
    assert_eq!(result["result_evidence_ref"], "fixture-result");
    assert_eq!(result["result_sha256"], digest(result_body));
    let row: (String, String) = reader
        .exec_first(
            "SELECT body, transition_json FROM issue_updates WHERE id = ?",
            (result["transition_id"].as_str().unwrap(),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(row.0, result_body);
    let typed: Value = serde_json::from_str(&row.1).unwrap();
    assert_eq!(typed["request_id"], submit_request);
    assert_eq!(typed["effect"]["stopped"]["stop"], third_stop);
    assert!(!row.1.contains("Synthetic submission"));
    let count: u64 = reader
        .exec_first(
            "SELECT COUNT(*) FROM issue_claims WHERE issue_id = ?",
            (issue,),
        )
        .unwrap()
        .unwrap();
    assert_eq!(count, 0);

    let receipt = stored_receipt(&mut reader, &submit_request);
    let mut writer = connect_owned(state, port, "writer");
    let session: u64 = writer
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    intent["kind"] = json!("issue.submit");
    intent["launch_nonce"] = json!(launch_nonce(state));
    intent["session_id"] = json!(session);
    intent["request_id"] = json!(submit_request);
    intent["base_commit"] = json!(receipt.4);
    intent["fingerprint"] = json!(receipt.2);
    intent["receipt_digest"] = json!(digest(&json!(receipt).to_string()));
    drop(writer);
    wait_for_session_exit(&mut connect_owned(state, port, "writer"), session);
    record_intent(state, &intent);
    let replay = submit(0);
    assert_eq!(replay["data"]["outcome"], "replayed");
    assert_eq!(replay["data"]["result"], *result);
    assert!(!state.join("pending-request.json").exists());

    drop(reader);
    record_intent(state, &intent);
    run(&["workspace", "stop"], 0);
    let verified = run(
        &["workspace", "recover", "--dolt-bin", dolt.to_str().unwrap()],
        0,
    );
    assert_eq!(verified["data"]["outcome"], "confirmed");
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

    // Model a subsequent nonsemantic revision without changing the original submission.
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "UPDATE issues SET priority = 1, revision = '{}' WHERE id = '{issue}';",
            id(225),
        ))),
        None,
    );
    let replay = submit(0);
    assert_eq!(replay["data"]["outcome"], "replayed");
    assert_eq!(
        replay["data"]["confirmed_commit"],
        submitted["data"]["confirmed_commit"]
    );
    assert_eq!(replay["data"]["result"], *result);
    let head = replay["context"]["db_commit"].as_str().unwrap();
    let invocation = id(226);
    let now = fixture_now();
    let inspection = json!({
        "version": 1, "evidence_id": "fixture-inspection", "revision": "r1",
        "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
        "workspace_id": workspace, "actor_ref": holder,
        "area_id": area, "issue_id": issue, "operations": ["inspect_operation_receipt"],
        "record": {"kind": "operation_receipt", "id": submit_request, "revision": head},
        "invocation_id": invocation, "db_commit": head,
        "valid_from": fixture_timestamp(now - time::Duration::seconds(10)),
        "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
        "withdrawn": false,
    });
    fs::write(&decision_file, inspection.to_string()).unwrap();
    let inspected = run(
        &[
            "operation",
            "show",
            &submit_request,
            "--database",
            "db",
            "--port",
            port,
            "--workspace-id",
            &workspace,
            "--area",
            &area,
            "--issue",
            issue,
            "--actor",
            holder,
            "--invocation-id",
            &invocation,
            "--evidence-file",
            decision_file.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(inspected["data"]["result"], *result);
    assert!(inspected["data"]["current_claim"].is_null());
}

fn decode(output: std::process::Output) -> (i32, Value) {
    let report = serde_json::from_slice(&output.stdout).unwrap();
    (output.status.code().unwrap(), report)
}

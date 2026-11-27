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
        assert_eq!(status, exit, "{report}");
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

    // Offline fixture edit models later settlement to test historical replay.
    edit(&format!(
        "DELETE FROM issue_claims WHERE issue_id = '{task}';
                  UPDATE issues SET status = 'todo', revision = '{}' WHERE id = '{task}';",
        id(211)
    ));
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

fn decode(output: std::process::Output) -> (i32, Value) {
    let report = serde_json::from_slice(&output.stdout).unwrap();
    (output.status.code().unwrap(), report)
}

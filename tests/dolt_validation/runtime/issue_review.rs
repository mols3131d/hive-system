use super::*;
use serde_json::json;
use time::OffsetDateTime;

const REVIEWER: &str = "fixture-independent-reviewer";
const APPROVAL: &str = "fixture-reviewed-decision";
const RESULT_REF: &str = "fixture-epic-result";
const BODY: &str = "Synthetic review café\r\n";

pub(super) struct Target<'a> {
    pub(super) root: &'a Path,
    pub(super) port: &'a str,
    pub(super) issue: &'a str,
}

pub(super) struct Decision {
    args: Vec<String>,
    path: PathBuf,
    evidence: Value,
}

impl Decision {
    pub(super) fn run(&self, invoke: &impl Fn(&[&str], i32) -> Value, exit: i32) -> Value {
        fs::write(&self.path, self.evidence.to_string()).unwrap();
        invoke(
            &self.args.iter().map(String::as_str).collect::<Vec<_>>(),
            exit,
        )
    }
}

impl Target<'_> {
    fn command(
        &self,
        action: &str,
        rev: &str,
        actor: &str,
        request: &str,
        suffix: Value,
    ) -> Decision {
        let body = self.root.join("review-outcome.md");
        fs::write(&body, BODY).unwrap();
        let path = self.root.join(format!("review-decision-{request}.json"));
        let operation = format!("issue.{action}");
        let workspace = id(1);
        let area = id(3);
        let mut fields = json!([1, operation, workspace, area, self.issue, actor, rev]);
        fields
            .as_array_mut()
            .unwrap()
            .extend(suffix.as_array().unwrap().iter().cloned());
        let now = OffsetDateTime::from_unix_timestamp_nanos(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i128,
        )
        .unwrap();
        let mut evidence = json!({
            "version": 1, "operation": operation, "workspace_id": workspace,
            "area_id": area, "issue_id": self.issue, "actor_ref": actor,
            "request_id": request, "expected_issue_revision": rev,
            "fingerprint": sha(&fields.to_string()),
            "authorization_ref": APPROVAL, "evidence_id": "fixture-reviewed-evidence",
            "revision": "r1", "source_ref": "fixture-source", "issuer_ref": actor,
            "valid_from": fixture_timestamp(now - time::Duration::minutes(1)),
            "expires_at": fixture_timestamp(now + time::Duration::hours(1)), "withdrawn": false,
        });
        let mut args = [
            "issue",
            action,
            self.issue,
            "--area",
            &area,
            "--actor",
            actor,
            "--if-match",
            &format!("{}={rev}", self.issue),
            "--request-id",
            request,
            "--workspace-id",
            &workspace,
            "--database",
            "db",
            "--port",
            self.port,
            "--evidence-file",
            path.to_str().unwrap(),
            "--file",
            body.to_str().unwrap(),
        ]
        .map(str::to_owned)
        .to_vec();
        if action == "submit-epic" {
            args.extend(["--evidence".to_owned(), RESULT_REF.to_owned()]);
        } else {
            let submission = suffix[0].as_str().unwrap();
            let cancelled = suffix[2].as_array().unwrap();
            args.extend(["--submission", submission, "--approval", APPROVAL].map(str::to_owned));
            for child in cancelled {
                args.extend(["--ack-cancelled-child", child.as_str().unwrap()].map(str::to_owned));
            }
            evidence["review"] = json!({
                "outcome": action.replace('-', "_"),
                "reviewer_ref": actor, "reviewed_at": format!("{}.{:06}Z", &fixture_timestamp(now)[..19], now.microsecond()),
                "binding": {"submission_id": submission, "body_sha256": suffix[1], "cancelled_child_ids": cancelled},
            });
        }
        Decision {
            args,
            path,
            evidence: json!({"decision": "allow", "evidence": evidence}),
        }
    }

    pub(super) fn review(
        &self,
        action: &str,
        rev: &str,
        submission: &str,
        actor: &str,
        request: u32,
        children: &[String],
    ) -> Decision {
        self.command(
            action,
            rev,
            actor,
            &id(request),
            json!([submission, sha(BODY), children]),
        )
    }
}

fn sha(body: &str) -> String {
    format!("{:x}", Sha256::digest(body))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn exercise_epic(
    state: &Path,
    root: &Path,
    port: &str,
    dolt: &Path,
    epic: &str,
    rev: &str,
    run: &impl Fn(&[&str], i32) -> Value,
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
) {
    let area = id(3);
    let task = id(243);
    let child = id(244);
    let prerequisite = id(245);
    let edit = |sql: &str| restart(Some(OfflineFixtureEdit::CommitSql(sql)), None);
    edit(&format!("INSERT INTO issues
        (id, number, area_id, parent_id, title, type, status, revision, acceptance_criteria, created_at, updated_at)
        VALUES ('{task}', 243, '{area}', NULL, 'Non-epic', 'task', 'todo', '{rev}', 'Criteria', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
               ('{child}', 244, '{area}', '{epic}', 'Child', 'task', 'todo', '{rev}', 'Criteria', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
               ('{prerequisite}', 245, '{area}', NULL, 'Prerequisite', 'task', 'todo', '{rev}', 'Criteria', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6));
        INSERT INTO issue_dependencies VALUES ('{epic}', '{prerequisite}');"));
    let target = Target {
        root,
        port,
        issue: epic,
    };
    let submit = target.command(
        "submit-epic",
        rev,
        "fixture-epic-author",
        &id(246),
        json!([RESULT_REF, sha(BODY)]),
    );
    let wrong_type = Target {
        issue: &task,
        ..target
    }
    .command(
        "submit-epic",
        rev,
        "fixture-epic-author",
        &id(247),
        json!([RESULT_REF, sha(BODY)]),
    );
    assert_eq!(
        wrong_type.run(run, 4)["error"]["code"],
        "INVALID_TRANSITION"
    );
    assert_eq!(
        submit.run(run, 4)["error"]["details"]["blockers"],
        json!(["PREREQUISITE_NOT_DONE"])
    );
    edit(&format!(
        "UPDATE issues SET status = 'done', closed_at = UTC_TIMESTAMP(6),
        closed_by_ref = 'fixture-reviewer', resolution_note = 'Done' WHERE id = '{prerequisite}';"
    ));
    assert_eq!(submit.run(run, 4)["error"]["code"], "INVALID_TRANSITION");
    edit(&format!("UPDATE issues SET status = 'cancelled', closed_at = UTC_TIMESTAMP(6),
        closed_by_ref = 'fixture-reviewer', resolution_note = 'Dropped scope' WHERE id = '{child}';"));

    edit(&format!(
        "UPDATE areas SET archived_at = UTC_TIMESTAMP(6) WHERE id = '{area}';"
    ));
    assert_eq!(
        submit.run(run, 4)["error"]["details"]["blockers"],
        json!(["ARCHIVED_AREA"])
    );
    edit(&format!("UPDATE areas SET archived_at = NULL WHERE id = '{area}';
        INSERT INTO issue_claims VALUES ('{epic}', '{}', 'fixture-invalid-epic-holder', UTC_TIMESTAMP(6));", id(254)));
    assert_eq!(submit.run(run, 5)["error"]["code"], "CLAIM_CONFLICT");
    edit(&format!(
        "DELETE FROM issue_claims WHERE issue_id = '{epic}';"
    ));

    let body_path = root.join("review-outcome.md");
    fs::write(&body_path, [0xff]).unwrap();
    assert_eq!(submit.run(run, 2)["error"]["code"], "INVALID_ARGUMENT");
    fs::write(&body_path, b"synthetic\0body").unwrap();
    assert_eq!(submit.run(run, 2)["error"]["code"], "INVALID_ARGUMENT");
    fs::write(&body_path, "Different synthetic bytes").unwrap();
    assert_eq!(submit.run(run, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    fs::write(body_path, BODY).unwrap();

    let submitted = submit.run(run, 0);
    let result = &submitted["data"]["result"];
    assert_eq!(result["status"], "review");
    assert_eq!(result["result_evidence_ref"], RESULT_REF);
    assert_eq!(result["result_sha256"], sha(BODY));
    let submission = result["transition_id"].as_str().unwrap();
    let current = result["issue_revision"].as_str().unwrap();
    let accept = |actor, request, ids: &[String]| {
        target.review("accept", current, submission, actor, request, ids)
    };

    let mut reader = connect_owned(state, port, "reader");
    let first_receipt = stored_receipt(&mut reader, &id(246));
    let before = storage_state(&mut reader);
    for (decision, exit, code) in [
        (
            accept("fixture-epic-author", 248, std::slice::from_ref(&child)),
            7,
            "AUTHORITY_UNVERIFIED",
        ),
        (accept(REVIEWER, 249, &[]), 4, "INVALID_TRANSITION"),
        (
            accept(REVIEWER, 250, std::slice::from_ref(&prerequisite)),
            4,
            "INVALID_TRANSITION",
        ),
    ] {
        assert_eq!(decision.run(run, exit)["error"]["code"], code);
        assert_eq!(storage_state(&mut reader), before);
        assert!(!state.join("pending-request.json").exists());
    }
    let mut expired = accept(REVIEWER, 251, std::slice::from_ref(&child));
    expired.evidence["evidence"]["expires_at"] = expired.evidence["evidence"]["valid_from"].clone();
    assert_eq!(expired.run(run, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    let mut mismatch = accept(REVIEWER, 252, std::slice::from_ref(&child));
    mismatch.evidence["evidence"]["review"]["outcome"] = json!("request_changes");
    assert_eq!(
        mismatch.run(run, 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    assert_eq!(storage_state(&mut reader), before);

    let accept = accept(REVIEWER, 253, std::slice::from_ref(&child));
    let mut writer = connect_owned(state, port, "writer");
    let session: u64 = writer
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    let mut intent = json!({
        "version": 4, "kind": "issue.accept", "workspace_id": id(1),
        "launch_nonce": launch_nonce(state), "session_id": session, "request_id": id(253),
        "base_commit": before.0, "area_id": area, "issue_id": epic,
        "fingerprint": sha("fixture rollback"), "receipt_digest": sha("fixture absent receipt"),
    });
    writer.query_drop("START TRANSACTION").unwrap();
    record_intent(state, &intent);
    assert_eq!(accept.run(run, 8)["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(storage_state(&mut reader), before);
    drop(writer);
    wait_for_session_exit(&mut connect_owned(state, port, "writer"), session);
    let accepted = accept.run(run, 0);
    let outcome = &accepted["data"]["result"];
    assert_eq!(outcome["status"], "done");
    assert_eq!(outcome["submission_id"], submission);
    assert_eq!(outcome["cancelled_child_ids"], json!([child]));
    let receipt = stored_receipt(&mut reader, &id(253));
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
    ) = reader
        .exec_first(
            "SELECT i.status, i.revision, i.closed_by_ref, i.resolution_note,
           DATE_FORMAT(i.closed_at, '%Y-%m-%dT%H:%i:%s.%fZ'), u.body, c.claim_id
         FROM issues i JOIN issue_updates u ON u.issue_id = i.id
         LEFT JOIN issue_claims c ON c.issue_id = i.id WHERE i.id = ? AND u.id = ?",
            (epic, outcome["transition_id"].as_str().unwrap()),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        row,
        (
            "done".to_owned(),
            outcome["issue_revision"].as_str().unwrap().to_owned(),
            REVIEWER.to_owned(),
            BODY.to_owned(),
            receipt.5.clone(),
            BODY.to_owned(),
            None
        )
    );
    let delta: Vec<(String, u64, u64, u64)> = reader.exec(
        "SELECT table_name, rows_added, rows_deleted, rows_modified FROM DOLT_DIFF_STAT(?, ?) ORDER BY table_name",
        (&receipt.4, accepted["data"]["confirmed_commit"].as_str().unwrap()),
    ).unwrap();
    assert_eq!(
        delta,
        vec![
            ("issue_updates".to_owned(), 1, 0, 0),
            ("issues".to_owned(), 0, 0, 1),
            ("operation_receipts".to_owned(), 1, 0, 0),
        ]
    );
    assert_eq!(accept.run(run, 0)["data"]["outcome"], "replayed");
    assert_eq!(submit.run(run, 0)["data"]["result"], *result);

    intent["base_commit"] = json!(receipt.4);
    intent["fingerprint"] = json!(receipt.2);
    intent["receipt_digest"] = json!(sha(&json!(receipt).to_string()));
    record_intent(state, &intent);
    drop(reader);
    run(&["workspace", "stop"], 0);
    let recovered = run(
        &["workspace", "recover", "--dolt-bin", dolt.to_str().unwrap()],
        0,
    );
    assert_eq!(recovered["data"]["outcome"], "confirmed");
    assert!(!state.join("pending-request.json").exists());
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

    // Reuse the first introduction timestamp for a second, independently
    // verifiable epic submission after request-changes.
    restart(
        Some(OfflineFixtureEdit::RestoreHead(
            submitted["data"]["confirmed_commit"].as_str().unwrap(),
        )),
        None,
    );
    let returned = target
        .review("request-changes", current, submission, REVIEWER, 255, &[])
        .run(run, 0);
    let returned_rev = returned["data"]["result"]["issue_revision"]
        .as_str()
        .unwrap();
    let returned_commit = returned["data"]["confirmed_commit"].as_str().unwrap();
    let new_request = id(256);
    let new_transition = id(257);
    let new_revision = id(258);
    let mut same_time = target.command(
        "submit-epic",
        returned_rev,
        "fixture-epic-author",
        &new_request,
        json!([RESULT_REF, sha(BODY)]),
    );
    let mut typed: Value = serde_json::from_str(&first_receipt.7).unwrap();
    typed["request_id"] = json!(new_request);
    typed["transition_id"] = json!(new_transition);
    typed["previous_issue_revision"] = json!(returned_rev);
    typed["issue_revision"] = json!(new_revision);
    let prior_evidence: Value = serde_json::from_str(&first_receipt.6).unwrap();
    let evidence = &mut same_time.evidence["evidence"];
    evidence["valid_from"] = prior_evidence["valid_from"].clone();
    let fingerprint = evidence["fingerprint"].as_str().unwrap();
    edit(&format!(
        "UPDATE issues SET status = 'review', revision = '{new_revision}',
            updated_at = (SELECT created_at FROM issue_updates WHERE id = '{submission}')
         WHERE id = '{epic}';
         INSERT INTO issue_updates (id, issue_id, kind, body, author_ref, created_at, transition_json)
         SELECT '{new_transition}', issue_id, kind, body, author_ref, created_at, '{typed}'
         FROM issue_updates WHERE id = '{submission}';
         INSERT INTO operation_receipts
            (request_id, kind, actor_ref, fingerprint, authorization_ref, workflow_evidence_json,
             base_commit, result_json, created_at)
         SELECT '{new_request}', kind, actor_ref, '{fingerprint}', authorization_ref,
            '{evidence}', '{returned_commit}', '{typed}', created_at
         FROM operation_receipts WHERE request_id = '{}';", id(246),
    ));
    assert_eq!(same_time.run(run, 0)["data"]["outcome"], "replayed");
    let old = target.review(
        "accept",
        &new_revision,
        submission,
        REVIEWER,
        259,
        std::slice::from_ref(&child),
    );
    assert_eq!(old.run(run, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    let exact = target.review(
        "accept",
        &new_revision,
        &new_transition,
        REVIEWER,
        260,
        std::slice::from_ref(&child),
    );
    assert_eq!(
        exact.run(run, 0)["data"]["result"]["submission_id"],
        new_transition
    );
}

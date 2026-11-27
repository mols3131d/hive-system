use super::*;
use serde_json::json;
use std::process::Output;
use std::sync::Barrier;

const ACTOR: &str = "fixture-parent-planner";
type Endpoint = (String, String, String);

struct Edit {
    request: String,
    issue: Endpoint,
    old_parent: Option<Endpoint>,
    parent: Option<Endpoint>,
    evidence: Value,
}

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string()))
}

impl Edit {
    fn new(
        request: u32,
        issue: Endpoint,
        old_parent: Option<Endpoint>,
        parent: Option<Endpoint>,
    ) -> Self {
        let request = id(request);
        let workspace = id(1);
        let fingerprint = digest(&json!([
            1,
            "issue.set-parent",
            workspace,
            ACTOR,
            issue,
            old_parent,
            parent
        ]));
        let now = fixture_now();
        let grant = |(area, issue, revision): &Endpoint| {
            json!({
                "endpoint": {"area_id": area, "issue_id": issue, "expected_issue_revision": revision},
                "decision": "allow", "evidence_id": "fixture-endpoint", "revision": "v1",
                "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
                "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
                "withdrawn": false,
            })
        };
        let evidence = json!({
            "decision": "allow",
            "evidence": {
                "version": 1, "operation": "issue.set-parent", "workspace_id": workspace,
                "request_id": request, "fingerprint": fingerprint, "actor_ref": ACTOR,
                "authorization_ref": "fixture-parent-authorization",
                "issue": grant(&issue), "old_parent": old_parent.as_ref().map(grant),
                "parent": parent.as_ref().map(grant),
            }
        });
        Self {
            request,
            issue,
            old_parent,
            parent,
            evidence,
        }
    }

    fn args(&self, root: &Path, port: &str) -> Vec<String> {
        let file = root.join(format!("parent-{}.json", self.request));
        fs::write(&file, self.evidence.to_string()).unwrap();
        let mut args = [
            "issue",
            "set-parent",
            &self.issue.1,
            "--workspace-id",
            &id(1),
            "--database",
            "db",
            "--port",
            port,
            "--area",
            &self.issue.0,
            "--actor",
            ACTOR,
            "--request-id",
            &self.request,
            "--evidence-file",
            file.to_str().unwrap(),
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some((area, issue, _)) = &self.old_parent {
            args.extend(["--old-parent", issue, "--old-parent-area", area].map(str::to_owned));
        }
        if let Some((area, issue, _)) = &self.parent {
            args.extend(["--parent", issue, "--parent-area", area].map(str::to_owned));
        } else {
            args.push("--clear".to_owned());
        }
        for (_, issue, revision) in [&self.issue]
            .into_iter()
            .chain(self.old_parent.iter())
            .chain(self.parent.iter())
            .rev()
        {
            args.extend(["--if-match".to_owned(), format!("{issue}={revision}")]);
        }
        args
    }

    fn marker(
        &self,
        state: &Path,
        session: u64,
        base: &str,
        receipt: &issue_update::Receipt,
    ) -> Value {
        json!({
            "version": 2, "kind": "issue.set-parent", "workspace_id": id(1),
            "launch_nonce": launch_nonce(state), "session_id": session,
            "request_id": self.request, "base_commit": base,
            "fingerprint": self.evidence["evidence"]["fingerprint"], "area_id": self.issue.0,
            "issue_id": self.issue.1, "receipt_digest": digest(&json!(receipt)),
        })
    }
}

fn endpoint(reader: &mut Conn, issue: u32) -> Endpoint {
    let issue = id(issue);
    let (area, revision) = reader
        .exec_first(
            "SELECT area_id, revision FROM issues WHERE id = ?",
            (&issue,),
        )
        .unwrap()
        .unwrap();
    (area, issue, revision)
}

fn parent_of(reader: &mut Conn, issue: &str) -> Option<String> {
    reader
        .exec_first("SELECT parent_id FROM issues WHERE id = ?", (issue,))
        .unwrap()
        .unwrap()
}

fn protected_rows(reader: &mut Conn) -> Vec<mysql::Row> {
    reader
        .query(
            "SELECT id, number, area_id, title, body, acceptance_criteria, type, status, priority,
                hold_reason, created_at, updated_at, closed_at, closed_by_ref, resolution_note
         FROM issues ORDER BY id",
        )
        .unwrap()
}

pub(super) fn exercise(
    state: &Path,
    root: &Path,
    port: &str,
    dolt: &Path,
    invoke: &(impl Fn(&[&str]) -> io::Result<Output> + Sync),
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
) {
    let run = |args: &[&str]| {
        let output = invoke(args).unwrap();
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|err| panic!("{err}: {output:?}"));
        assert!(!report.to_string().contains("fixture-parent-authorization"));
        (output.status.code().unwrap(), report)
    };
    let submit = |args: &[String]| run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let edit = |edit: &Edit, exit| {
        let (code, report) = submit(&edit.args(root, port));
        assert_eq!(report["command"], "issue set-parent");
        assert_eq!(code, exit, "{report}");
        report
    };
    let connect = |role| connect_owned(state, port, role);
    let stop = || {
        let (code, report) = run(&["workspace", "stop"]);
        assert_eq!(code, 0, "{report}");
        assert_eq!(report["data"]["process_exit"], "pidfd_verified");
    };
    let start = || {
        let (code, report) = run(&[
            "workspace",
            "start",
            "--dolt-bin",
            dolt.to_str().unwrap(),
            "--port",
            port,
        ]);
        assert_eq!(code, 0, "{report}");
    };
    let recover = |outcome| {
        let (code, report) = run(&["workspace", "recover", "--dolt-bin", dolt.to_str().unwrap()]);
        assert_eq!(code, 0, "{report}");
        assert_eq!(report["data"]["outcome"], outcome);
        assert!(!state.join("pending-request.json").exists());
    };

    let initial = storage_state(&mut connect("reader"));
    let [
        a,
        b,
        c,
        d,
        review,
        cancelled,
        archive,
        descendant,
        archived_parent,
    ] = [170, 171, 172, 173, 174, 175, 176, 177, 178].map(id);
    let area = id(2);
    let rev = id(4);
    restart(Some(OfflineFixtureEdit::CommitSql(&format!(
        "UPDATE workspace_counters SET next_value = 17 WHERE name = 'issue_number';
         INSERT INTO areas (id, slug, name, revision, archived_at)
           VALUES ('{archive}', 'archived-root', 'Archived root', '{rev}', '2026-01-01');
         INSERT INTO areas (id, parent_id, slug, name, revision)
           VALUES ('{descendant}', '{archive}', 'archived-descendant', 'Archived descendant', '{rev}');
         INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at) VALUES
           ('{a}', 10, '{area}', 'Fixture A', 'task', 'backlog', '{rev}', '2026-01-01', '2026-01-01'),
           ('{b}', 11, '{area}', 'Fixture B', 'task', 'backlog', '{rev}', '2026-01-01', '2026-01-01'),
           ('{c}', 12, '{area}', 'Fixture C', 'task', 'backlog', '{rev}', '2026-01-01', '2026-01-01'),
           ('{d}', 13, '{area}', 'Fixture D', 'task', 'backlog', '{rev}', '2026-01-01', '2026-01-01'),
           ('{review}', 14, '{area}', 'Review', 'task', 'review', '{rev}', '2026-01-01', '2026-01-01'),
           ('{cancelled}', 15, '{area}', 'Cancelled', 'task', 'todo', '{rev}', '2026-01-01', '2026-01-01'),
           ('{archived_parent}', 16, '{descendant}', 'Archived parent', 'task', 'todo', '{rev}', '2026-01-01', '2026-01-01');
         UPDATE issues SET status = 'cancelled', closed_at = '2026-01-02', closed_by_ref = 'fixture-reviewer',
           resolution_note = 'Cancelled scope' WHERE id = '{cancelled}';
         INSERT INTO issue_dependencies VALUES ('{b}', '{c}'), ('{d}', '{a}');",
    ))), None);

    let mut reader = connect("reader");
    let base = storage_state(&mut reader);
    let unchanged_issues = protected_rows(&mut reader);
    let claims = |reader: &mut Conn| -> Vec<mysql::Row> {
        reader
            .query("SELECT * FROM issue_claims ORDER BY issue_id")
            .unwrap()
    };
    let initial_claims = claims(&mut reader);
    let child = endpoint(&mut reader, 11);
    let old = endpoint(&mut reader, 10);
    let parent = endpoint(&mut reader, 13);
    let accepted = endpoint(&mut reader, 12);
    let review = endpoint(&mut reader, 174);
    let cancelled = endpoint(&mut reader, 175);
    let archived_parent = endpoint(&mut reader, 178);

    // Current owner evidence must cover each incident Issue, including the removed edge.
    let mut no_old_grant = Edit::new(180, child.clone(), Some(old.clone()), Some(parent.clone()));
    no_old_grant.evidence["evidence"]["old_parent"] = Value::Null;
    assert_eq!(
        edit(&no_old_grant, 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    for wrong in 0..3 {
        let mut endpoints = [child.clone(), old.clone(), parent.clone()];
        endpoints[wrong].0 = if endpoints[wrong].0 == id(2) {
            id(3)
        } else {
            id(2)
        };
        let [child, old, parent] = endpoints;
        assert_eq!(
            edit(&Edit::new(180, child, Some(old), Some(parent)), 7)["error"]["code"],
            "AUTHORITY_UNVERIFIED"
        );
        let mut endpoints = [
            endpoint(&mut reader, 11),
            endpoint(&mut reader, 10),
            endpoint(&mut reader, 13),
        ];
        endpoints[wrong].2 = id(190);
        let [child, old, parent] = endpoints;
        assert_eq!(
            edit(&Edit::new(180, child, Some(old), Some(parent)), 5)["error"]["code"],
            "STALE_REVISION"
        );
    }
    assert_eq!(
        edit(
            &Edit::new(180, child.clone(), None, Some(parent.clone())),
            5
        )["error"]["code"],
        "STALE_REVISION"
    );
    assert_eq!(
        edit(
            &Edit::new(
                180,
                child.clone(),
                Some(accepted.clone()),
                Some(parent.clone())
            ),
            5
        )["error"]["code"],
        "STALE_REVISION"
    );
    for plan in [
        Edit::new(180, child.clone(), Some(old.clone()), Some(old.clone())),
        Edit::new(180, old.clone(), None, Some(old.clone())),
    ] {
        assert_eq!(edit(&plan, 2)["error"]["code"], "INVALID_ARGUMENT");
    }
    for plan in [
        Edit::new(180, old.clone(), None, Some(child.clone())),
        Edit::new(
            180,
            child.clone(),
            Some(old.clone()),
            Some(accepted.clone()),
        ),
        Edit::new(180, child.clone(), Some(old.clone()), Some(review.clone())),
        Edit::new(180, cancelled, None, Some(review)),
        Edit::new(180, child.clone(), Some(old.clone()), Some(archived_parent)),
    ] {
        assert_eq!(edit(&plan, 4)["error"]["code"], "INVALID_TRANSITION");
    }
    assert_eq!(storage_state(&mut reader), base);

    let move_plan = Edit::new(180, child.clone(), Some(old.clone()), Some(parent.clone()));
    let moved = edit(&move_plan, 0);
    assert_eq!(moved["data"]["base_commit"], base.0);
    assert_eq!(parent_of(&mut reader, &child.1), Some(parent.1.clone()));
    for (number, prior, key) in [
        (11, child, "issue"),
        (10, old, "old_parent"),
        (13, parent, "parent"),
    ] {
        let current = endpoint(&mut reader, number);
        assert_ne!(current.2, prior.2);
        assert_eq!(
            moved["data"]["result"][key],
            json!({"area_id": prior.0, "issue_id": prior.1, "issue_revision": current.2})
        );
    }
    assert_eq!(endpoint(&mut reader, 12), accepted);
    let child = endpoint(&mut reader, 11);
    let parent = endpoint(&mut reader, 13);
    let former = endpoint(&mut reader, 10);
    let clear = Edit::new(181, child.clone(), Some(parent.clone()), None);
    let cleared = edit(&clear, 0);
    assert!(cleared["data"]["result"]["parent"].is_null());
    assert_eq!(parent_of(&mut reader, &child.1), None);
    assert_ne!(endpoint(&mut reader, 11).2, child.2);
    assert_ne!(endpoint(&mut reader, 13).2, parent.2);
    assert_eq!(endpoint(&mut reader, 10), former);
    assert_eq!(protected_rows(&mut reader), unchanged_issues);
    assert_eq!(claims(&mut reader), initial_claims);
    let cleared_state = storage_state(&mut reader);
    assert_eq!(
        edit(&move_plan, 0)["data"]["result"],
        moved["data"]["result"]
    );
    assert_eq!(
        edit(&move_plan, 0)["data"]["confirmed_commit"],
        moved["data"]["confirmed_commit"]
    );
    assert_eq!(
        edit(
            &Edit::new(180, endpoint(&mut reader, 11), None, Some(former.clone())),
            5
        )["error"]["code"],
        "REQUEST_CONFLICT"
    );
    assert_eq!(storage_state(&mut reader), cleared_state);

    // Recover a confirmed clear, then recover an uncommitted attach after positive server-stop evidence.
    let receipt = issue_update::stored_receipt(&mut reader, &clear.request);
    let session = reader
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    issue_update::record_intent(state, &clear.marker(state, session, &receipt.4, &receipt));
    drop(reader);
    stop();
    recover("confirmed");
    start();

    let attach = Edit::new(
        182,
        endpoint(&mut connect("reader"), 11),
        None,
        Some(former.clone()),
    );
    let mut writer = connect("writer");
    let session = writer
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    writer.query_drop("START TRANSACTION").unwrap();
    writer
        .exec_drop(
            "UPDATE issues SET parent_id = ? WHERE id = ?",
            (&former.1, &attach.issue.1),
        )
        .unwrap();
    assert_eq!(writer.affected_rows(), 1);
    issue_update::record_intent(
        state,
        &attach.marker(state, session, &cleared_state.0, &receipt),
    );
    assert_eq!(edit(&attach, 8)["error"]["code"], "COMMIT_UNCERTAIN");
    stop();
    drop(writer);
    recover("rolled_back");
    start();
    assert_eq!(storage_state(&mut connect("reader")), cleared_state);
    let attached = edit(&attach, 0);
    assert_eq!(
        parent_of(&mut connect("reader"), &attach.issue.1),
        Some(former.1.clone())
    );
    assert!(attached["data"]["result"]["old_parent"].is_null());
    assert_eq!(protected_rows(&mut connect("reader")), unchanged_issues);
    assert_eq!(claims(&mut connect("reader")), initial_claims);

    // Two otherwise valid proposals use disjoint Issue endpoints. Together they close a
    // cycle through B -> C and D -> A, so stale revisions alone cannot stop the loser.
    let mut reader = connect("reader");
    let first = Edit::new(
        183,
        endpoint(&mut reader, 171),
        None,
        Some(endpoint(&mut reader, 170)),
    );
    let second = Edit::new(
        184,
        endpoint(&mut reader, 173),
        None,
        Some(endpoint(&mut reader, 172)),
    );
    let args = [first.args(root, port), second.args(root, port)];
    let barrier = Barrier::new(2);
    let result = std::thread::scope(|scope| {
        let handles = args.each_ref().map(|args| {
            scope.spawn(|| {
                barrier.wait();
                submit(args)
            })
        });
        handles.map(|worker| worker.join().unwrap())
    });
    assert_eq!(
        result.iter().filter(|(code, _)| *code == 0).count(),
        1,
        "{result:?}"
    );
    for (plan, (code, report)) in [&first, &second].into_iter().zip(result) {
        match code {
            0 => assert_eq!(report["data"]["outcome"], "confirmed"),
            4 => assert_eq!(report["error"]["code"], "INVALID_TRANSITION"),
            5 => {
                assert_eq!(report["error"]["code"], "LOCK_BUSY");
                assert_eq!(edit(plan, 4)["error"]["code"], "INVALID_TRANSITION");
            }
            _ => panic!("{report}"),
        }
    }
    assert_ne!(parent_of(&mut reader, &b), parent_of(&mut reader, &d));
    assert_eq!(
        reader
            .exec_first::<u64, _, _>(
                "SELECT COUNT(*) FROM operation_receipts WHERE request_id IN (?, ?)",
                (&first.request, &second.request),
            )
            .unwrap(),
        Some(1)
    );
    assert!(!state.join("pending-request.json").exists());
    let accepted = endpoint(&mut reader, 12);
    let review = endpoint(&mut reader, 174);
    edit(&Edit::new(185, accepted, None, Some(review)), 0);
    let accepted = endpoint(&mut reader, 12);
    let review = endpoint(&mut reader, 174);
    let protected = storage_state(&mut reader);
    assert_eq!(
        edit(&Edit::new(186, accepted, Some(review), None), 4)["error"]["code"],
        "INVALID_TRANSITION"
    );
    assert_eq!(storage_state(&mut reader), protected);

    // A receipt and the expected endpoint revisions cannot hide an Issue field change.
    drop(reader);
    restart(Some(OfflineFixtureEdit::RestoreHead(&receipt.4)), None);
    let timestamp = receipt.5.trim_end_matches('Z').replace('T', " ");
    let result = &cleared["data"]["result"];
    restart(Some(OfflineFixtureEdit::CommitSql(&format!(
        "UPDATE issues SET parent_id = NULL, revision = '{}', title = 'Unexpected title' WHERE id = '{}';
         UPDATE issues SET revision = '{}' WHERE id = '{}';
         INSERT INTO operation_receipts VALUES ('{}', '{}', '{}', '{}', '{}', '{}', '{}', '{}', '{}');",
        result["issue"]["issue_revision"].as_str().unwrap(),
        clear.issue.1,
        result["old_parent"]["issue_revision"].as_str().unwrap(),
        clear.old_parent.as_ref().unwrap().1,
        clear.request, receipt.0, receipt.2, receipt.1, receipt.3, receipt.6, receipt.4, receipt.7, timestamp,
    ))), None);
    let forged = storage_state(&mut connect("reader"));
    assert_eq!(edit(&clear, 8)["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(storage_state(&mut connect("reader")), forged);
    restart(Some(OfflineFixtureEdit::RestoreHead(&initial.0)), None);
}

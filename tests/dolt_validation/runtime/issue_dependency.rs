use super::*;
use serde_json::json;
use std::process::Output;
use std::sync::Barrier;

const ACTOR: &str = "fixture-dependency-planner";

struct Edit {
    leaf: &'static str,
    request: String,
    endpoints: [(String, String, String); 2],
    evidence: Value,
}

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string()))
}

impl Edit {
    fn new(leaf: &'static str, request: u32, endpoints: [(String, String, String); 2]) -> Self {
        let request = id(request);
        let workspace = id(1);
        let operation = format!("issue.{leaf}");
        let fingerprint = digest(&json!([
            1,
            operation,
            workspace,
            ACTOR,
            endpoints[0],
            endpoints[1]
        ]));
        let grant = |(area, issue, revision): &(String, String, String), evidence_id| {
            let now = fixture_now();
            json!({
                "endpoint": {"area_id": area, "issue_id": issue, "expected_issue_revision": revision},
                "decision": "allow", "evidence_id": evidence_id,
                "revision": "r1", "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
                "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
                "withdrawn": false,
            })
        };
        let evidence = json!({
            "decision": "allow",
            "evidence": {
                "version": 1, "operation": operation, "workspace_id": workspace,
                "request_id": request, "fingerprint": fingerprint, "actor_ref": ACTOR,
                "authorization_ref": "fixture-dependency-authorization",
                "issue": grant(&endpoints[0], "fixture-dependent-evidence"),
                "prerequisite": grant(&endpoints[1], "fixture-prerequisite-evidence"),
            }
        });
        Self {
            leaf,
            request,
            endpoints,
            evidence,
        }
    }

    fn args(&self, root: &Path, port: &str) -> Vec<String> {
        let [
            (area, issue, revision),
            (prereq_area, prereq, prereq_revision),
        ] = &self.endpoints;
        let file = root.join(format!("dependency-{}.json", self.request));
        fs::write(&file, self.evidence.to_string()).unwrap();
        [
            "issue",
            self.leaf,
            issue,
            prereq,
            "--workspace-id",
            &id(1),
            "--database",
            "db",
            "--port",
            port,
            "--area",
            area,
            "--prerequisite-area",
            prereq_area,
            "--actor",
            ACTOR,
            "--if-match",
            &format!("{issue}={revision}"),
            "--if-match",
            &format!("{prereq}={prereq_revision}"),
            "--request-id",
            &self.request,
            "--evidence-file",
            file.to_str().unwrap(),
        ]
        .map(str::to_owned)
        .into()
    }
}

fn revision(reader: &mut Conn, issue: &str) -> String {
    reader
        .exec_first("SELECT revision FROM issues WHERE id = ?", (issue,))
        .unwrap()
        .unwrap()
}

fn marker_for(state: &Path, session: u64, edit: &Edit, receipt: &Receipt) -> Value {
    json!({
        "version": 2, "kind": receipt.0, "workspace_id": id(1),
        "launch_nonce": launch_nonce(state), "session_id": session, "request_id": edit.request,
        "base_commit": receipt.4, "fingerprint": receipt.2, "area_id": edit.endpoints[0].0,
        "issue_id": edit.endpoints[0].1, "receipt_digest": digest(&json!(receipt)),
    })
}

pub(super) fn exercise(
    state: &Path,
    root: &Path,
    port: &str,
    dolt: &Path,
    invoke: &(impl Fn(&[&str]) -> io::Result<Output> + Sync),
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
) {
    let parse = |args: &[&str]| {
        let output = invoke(args).unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            !report
                .to_string()
                .contains("fixture-dependency-authorization")
        );
        (output.status.code().unwrap(), report)
    };
    let run = |args: &[&str], exit| {
        let (code, report) = parse(args);
        assert_eq!(code, exit, "{report}");
        report
    };
    let submit = |args: &[String]| {
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        parse(&args)
    };
    let edit = |edit: &Edit, exit| {
        let (code, report) = submit(&edit.args(root, port));
        assert_eq!(report["command"], format!("issue {}", edit.leaf));
        assert_eq!(code, exit, "{report}");
        report
    };
    let connect = |role| connect_owned(state, port, role);
    let mut reader = connect("reader");
    let initial = storage_state(&mut reader);
    let issue = id(13);
    let prereq = id(12);
    let endpoints = [
        (id(3), issue.clone(), revision(&mut reader, &issue)),
        (id(2), prereq.clone(), revision(&mut reader, &prereq)),
    ];
    let mut writer = connect("writer");
    let grants: Vec<String> = writer
        .query(
            "SELECT PRIVILEGE_TYPE FROM information_schema.TABLE_PRIVILEGES
         WHERE TABLE_SCHEMA = 'db' AND TABLE_NAME = 'issue_dependencies' ORDER BY PRIVILEGE_TYPE",
        )
        .unwrap();
    assert_eq!(grants, ["DELETE", "INSERT"]);
    assert!(
        writer
            .query_drop("UPDATE issue_dependencies SET issue_id = issue_id")
            .is_err()
    );

    let mut denied = Edit::new("remove-dependency", 151, endpoints.clone());
    denied.evidence["evidence"]["prerequisite"]["withdrawn"] = json!(true);
    assert_eq!(edit(&denied, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    let mut denied = Edit::new("remove-dependency", 151, endpoints.clone());
    denied.evidence["evidence"]
        .as_object_mut()
        .unwrap()
        .remove("issue");
    assert_eq!(edit(&denied, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    let mut wrong_area = endpoints.clone();
    wrong_area[1].0 = id(3);
    assert_eq!(
        edit(&Edit::new("remove-dependency", 151, wrong_area), 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    let mut stale = endpoints.clone();
    stale[1].2 = id(153);
    assert_eq!(
        edit(&Edit::new("remove-dependency", 151, stale), 5)["error"]["code"],
        "STALE_REVISION"
    );
    let duplicate = Edit::new("add-dependency", 151, endpoints.clone());
    assert_eq!(edit(&duplicate, 4)["error"]["code"], "INVALID_TRANSITION");
    let remove = Edit::new("remove-dependency", 151, endpoints.clone());
    let mut args = remove.args(root, port);
    args[3] = args[2].clone();
    assert_eq!(submit(&args).1["error"]["code"], "INVALID_ARGUMENT");
    args = remove.args(root, port);
    args[19] = args[17].clone();
    assert_eq!(submit(&args).1["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(storage_state(&mut reader), initial);

    let removed = edit(&remove, 0);
    let result = &removed["data"]["result"];
    assert_eq!(result["issue_id"], issue);
    assert_eq!(result["prerequisite_id"], prereq);
    for (index, key) in [(0, "issue_revision"), (1, "prerequisite_revision")] {
        let (_, endpoint, old_revision) = &endpoints[index];
        let actual = revision(&mut reader, endpoint);
        assert_ne!(actual, *old_revision);
        assert_eq!(result[key], actual);
    }
    assert_eq!(removed["data"]["base_commit"], initial.0);
    let removed_state = storage_state(&mut reader);
    let removed_receipt = stored_receipt(&mut reader, &remove.request);
    let present = |reader: &mut Conn| -> u64 {
        reader.exec_first(
            "SELECT COUNT(*) FROM issue_dependencies WHERE issue_id = ? AND prerequisite_id = ?",
            (&issue, &prereq),
        ).unwrap().unwrap()
    };
    assert_eq!(present(&mut reader), 0);

    let after = [
        (id(3), issue.clone(), revision(&mut reader, &issue)),
        (id(2), prereq.clone(), revision(&mut reader, &prereq)),
    ];
    let session: u64 = writer
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    writer.query_drop("START TRANSACTION").unwrap();
    let pending = marker_for(state, session, &remove, &removed_receipt);
    record_intent(state, &pending);
    let marker = state.join("pending-request.json");
    let add = Edit::new("add-dependency", 152, after.clone());
    assert_eq!(edit(&add, 8)["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(fs::read_to_string(&marker).unwrap(), pending.to_string());
    assert_eq!(storage_state(&mut reader), removed_state);
    writer.query_drop("ROLLBACK").unwrap();
    drop(writer);
    wait_for_session_exit(&mut connect("writer"), session);
    assert_eq!(
        edit(&Edit::new("remove-dependency", 153, after.clone()), 4)["error"]["code"],
        "INVALID_TRANSITION"
    );
    assert!(!marker.exists());
    let added = edit(&add, 0);
    assert_eq!(present(&mut reader), 1);
    let added_state = storage_state(&mut reader);
    let replay = edit(&remove, 0);
    assert_eq!(replay["data"]["outcome"], "replayed");
    assert_eq!(replay["data"]["result"], *result);
    assert_eq!(replay["data"]["confirmed_commit"], removed_state.0);
    assert_eq!(
        edit(&Edit::new("add-dependency", 151, after.clone()), 5)["error"]["code"],
        "REQUEST_CONFLICT"
    );
    assert_eq!(storage_state(&mut reader), added_state);

    let receipt = stored_receipt(&mut reader, &add.request);
    let session = reader
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    record_intent(state, &marker_for(state, session, &add, &receipt));
    drop(reader);
    let stop = run(&["workspace", "stop"], 0);
    assert_eq!(stop["data"]["process_exit"], "pidfd_verified");
    let recover = run(
        &["workspace", "recover", "--dolt-bin", dolt.to_str().unwrap()],
        0,
    );
    assert_eq!(recover["data"]["outcome"], "confirmed");
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
    assert_eq!(edit(&add, 0)["data"]["result"], added["data"]["result"]);

    // An uncommitted deletion disappears on verified stop. The fresh marker
    // keeps the workspace blocked until recovery checks its recorded base.
    let mut writer = connect("writer");
    let session = writer
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    writer.query_drop("START TRANSACTION").unwrap();
    writer
        .exec_drop(
            "DELETE FROM issue_dependencies WHERE issue_id = ? AND prerequisite_id = ?",
            (&issue, &prereq),
        )
        .unwrap();
    assert_eq!(writer.affected_rows(), 1);
    let pending_remove = Edit::new(
        "remove-dependency",
        165,
        [
            (
                id(3),
                issue.clone(),
                added["data"]["result"]["issue_revision"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            ),
            (
                id(2),
                prereq.clone(),
                added["data"]["result"]["prerequisite_revision"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            ),
        ],
    );
    let mut rollback = marker_for(state, session, &pending_remove, &receipt);
    rollback["kind"] = json!("issue.remove-dependency");
    rollback["base_commit"] = json!(added_state.0);
    rollback["fingerprint"] = pending_remove.evidence["evidence"]["fingerprint"].clone();
    record_intent(state, &rollback);
    run(&["workspace", "stop"], 0);
    drop(writer);
    let recover = run(
        &["workspace", "recover", "--dolt-bin", dolt.to_str().unwrap()],
        0,
    );
    assert_eq!(recover["data"]["outcome"], "rolled_back");
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
    assert_eq!(storage_state(&mut connect("reader")), added_state);
    assert_eq!(present(&mut connect("reader")), 1);

    // Forge the same apparent effect with an additional Issue change in the
    // introducing commit. Current authorization and matching IDs are insufficient.
    let forged_result = &added["data"]["result"];
    let timestamp = receipt.5.trim_end_matches('Z').replace('T', " ");
    let exact_effect = format!(
        "INSERT INTO issue_dependencies VALUES ('{issue}', '{prereq}');
         UPDATE issues SET revision = '{}', updated_at = '{timestamp}' WHERE id = '{issue}';
         UPDATE issues SET revision = '{}', updated_at = '{timestamp}' WHERE id = '{prereq}';",
        forged_result["issue_revision"].as_str().unwrap(),
        forged_result["prerequisite_revision"].as_str().unwrap(),
    );
    let insert_receipt = |base: &str| {
        format!(
            "INSERT INTO operation_receipts VALUES ('{}', '{}', '{}', '{}', '{}', '{}', '{}', '{}', '{}');",
            add.request,
            receipt.0,
            receipt.2,
            receipt.1,
            receipt.3,
            receipt.6,
            base,
            receipt.7,
            timestamp,
        )
    };
    restart(
        Some(OfflineFixtureEdit::RestoreHead(&removed_state.0)),
        None,
    );
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "{exact_effect} {} UPDATE issues SET title = 'Unexpected edit' WHERE id = '{}';",
            insert_receipt(&removed_state.0),
            id(10),
        ))),
        None,
    );
    assert_eq!(edit(&add, 8)["error"]["code"], "COMMIT_UNCERTAIN");

    // Matching endpoint revisions and a three-table delta still cannot
    // establish eligibility when an owning ancestry was archived at the base.
    restart(
        Some(OfflineFixtureEdit::RestoreHead(&removed_state.0)),
        None,
    );
    let archive = format!(
        "UPDATE areas SET archived_at = UTC_TIMESTAMP(6) WHERE id = '{}';",
        id(2)
    );
    restart(Some(OfflineFixtureEdit::CommitSql(&archive)), None);
    assert_eq!(edit(&add, 4)["error"]["code"], "INVALID_TRANSITION");
    let archived_base = storage_state(&mut connect("reader")).0;
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "{exact_effect} {}",
            insert_receipt(&archived_base),
        ))),
        None,
    );
    let forged_receipt = stored_receipt(&mut connect("reader"), &add.request);
    let session = connect("reader")
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    let forged_marker = marker_for(state, session, &add, &forged_receipt);
    record_intent(state, &forged_marker);
    run(&["workspace", "stop"], 0);
    let recover = run(
        &["workspace", "recover", "--dolt-bin", dolt.to_str().unwrap()],
        8,
    );
    assert_eq!(recover["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        forged_marker.to_string()
    );
    fs::remove_file(&marker).unwrap();
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

    let ids = [id(154), id(155), id(156), id(157)];
    let [a, b, c, d] = ids.each_ref();
    let area = id(2);
    let rev = id(4);
    let setup = format!(
        "UPDATE workspace_counters SET next_value = 14 WHERE name = 'issue_number';
         INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at) VALUES
           ('{a}', 10, '{area}', 'A', 'task', 'backlog', '{rev}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
           ('{b}', 11, '{area}', 'B', 'task', 'backlog', '{rev}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
           ('{c}', 12, '{area}', 'C', 'task', 'backlog', '{rev}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
           ('{d}', 13, '{area}', 'D', 'task', 'backlog', '{rev}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6));
         UPDATE issues SET parent_id = '{b}' WHERE id = '{c}';
         UPDATE issues SET status = 'review' WHERE id = '{issue}';
         INSERT INTO issue_dependencies VALUES ('{b}', '{c}'), ('{d}', '{a}');",
    );
    restart(Some(OfflineFixtureEdit::RestoreHead(&initial.0)), None);
    restart(Some(OfflineFixtureEdit::CommitSql(&setup)), None);
    let endpoint = |id: &String| (area.clone(), id.clone(), rev.clone());
    let before_unmet = storage_state(&mut connect("reader"));
    for (request, (area, issue)) in [
        (159, (id(3), id(11))),
        (166, (id(3), id(13))),
        (167, (id(2), id(12))),
    ] {
        let revision = revision(&mut connect("reader"), &issue);
        let unmet = Edit::new(
            "add-dependency",
            request,
            [(area, issue, revision), endpoint(a)],
        );
        assert_eq!(edit(&unmet, 4)["error"]["code"], "INVALID_TRANSITION");
    }
    assert_eq!(storage_state(&mut connect("reader")), before_unmet);
    // Remove a dependency parallel to the parent obligation. The reverse
    // dependency would still deadlock completion, though the dependency DAG is acyclic.
    let removed = edit(
        &Edit::new("remove-dependency", 160, [endpoint(b), endpoint(c)]),
        0,
    );
    let mut b_endpoint = endpoint(b);
    let mut c_endpoint = endpoint(c);
    b_endpoint.2 = removed["data"]["result"]["issue_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    c_endpoint.2 = removed["data"]["result"]["prerequisite_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let combined = Edit::new(
        "add-dependency",
        161,
        [c_endpoint.clone(), b_endpoint.clone()],
    );
    assert_eq!(edit(&combined, 4)["error"]["code"], "INVALID_TRANSITION");
    let readded = edit(
        &Edit::new("add-dependency", 162, [b_endpoint, c_endpoint]),
        0,
    );
    let mut b_endpoint = endpoint(b);
    let mut c_endpoint = endpoint(c);
    b_endpoint.2 = readded["data"]["result"]["issue_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    c_endpoint.2 = readded["data"]["result"]["prerequisite_revision"]
        .as_str()
        .unwrap()
        .to_owned();

    let first = Edit::new("add-dependency", 163, [endpoint(a), b_endpoint]);
    let second = Edit::new("add-dependency", 164, [c_endpoint, endpoint(d)]);
    let race_args = [first.args(root, port), second.args(root, port)];
    let start = Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let workers = race_args.each_ref().map(|args| {
            scope.spawn(|| {
                start.wait();
                submit(args)
            })
        });
        workers.map(|worker| worker.join().unwrap())
    });
    assert_eq!(
        results.iter().filter(|(code, _)| *code == 0).count(),
        1,
        "{results:?}"
    );
    for (plan, (code, report)) in [&first, &second].into_iter().zip(results) {
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
    let mut reader = connect("reader");
    let edges: Vec<(String, String)> = reader.query(
        "SELECT issue_id, prerequisite_id FROM issue_dependencies ORDER BY issue_id, prerequisite_id",
    ).unwrap();
    assert_ne!(
        edges.contains(&(a.clone(), b.clone())),
        edges.contains(&(c.clone(), d.clone()))
    );
    let count: u64 = reader
        .exec_first(
            "SELECT COUNT(*) FROM operation_receipts WHERE request_id IN (?, ?)",
            (&first.request, &second.request),
        )
        .unwrap()
        .unwrap();
    assert_eq!(count, 1);
    assert!(!marker.exists());

    drop(reader);
    // A planning edit must also reject an already invalid stored completion
    // state, even when the requested edge would be acyclic on its own.
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "UPDATE issues SET status = 'review' WHERE id = '{}';",
            id(10),
        ))),
        None,
    );
    let mut reader = connect("reader");
    let corrupt_base = storage_state(&mut reader);
    let unrelated = Edit::new(
        "remove-dependency",
        168,
        [
            (area.clone(), d.to_owned(), revision(&mut reader, d)),
            (area.clone(), a.to_owned(), revision(&mut reader, a)),
        ],
    );
    assert_eq!(edit(&unrelated, 4)["error"]["code"], "INVALID_RECORD");
    assert_eq!(storage_state(&mut reader), corrupt_base);
    drop(reader);
    restart(Some(OfflineFixtureEdit::RestoreHead(&initial.0)), None);
}

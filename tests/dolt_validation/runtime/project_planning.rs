use super::*;
use serde_json::json;

const ACTOR: &str = "fixture-planner";
const NAME: &str = "Cross-Area résumé";

struct Request<'a> {
    id: String,
    args: Vec<&'a str>,
    membership: Value,
    payload: Value,
    evidence_actor: &'a str,
}

impl<'a> Request<'a> {
    fn create(id: u32, name: &'a str, workspace: &str) -> Self {
        Self {
            id: super::id(id),
            args: vec!["create", "--name", name],
            membership: Value::Null,
            payload: json!([1, "project.create", workspace, ACTOR, name]),
            evidence_actor: ACTOR,
        }
    }

    fn add(
        id: u32,
        workspace: &str,
        project: &'a str,
        issue: &'a str,
        area: &'a str,
        project_rev: &str,
        issue_rev: &str,
    ) -> Self {
        Self {
            id: super::id(id),
            args: vec!["add-issue", project, issue, "--area", area],
            membership: json!({
                "project_id": project, "issue_id": issue, "area_id": area,
                "expected_project_revision": project_rev, "expected_issue_revision": issue_rev,
            }),
            payload: json!([
                1,
                "project.add-issue",
                workspace,
                ACTOR,
                project,
                issue,
                area,
                project_rev,
                issue_rev,
            ]),
            evidence_actor: ACTOR,
        }
    }
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP and Linux pidfds"]
fn project_planning_crosses_areas_and_recovers_the_exact_membership() {
    let listener = TcpListener::bind((HOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let workspace_id = id(1);
    let workspace_seed = seed_sql(CASE_INSENSITIVE);
    let inactive_project = id(25);
    let fixture = Fixture::initialize(&format!(
        "{workspace_seed}
         INSERT INTO projects VALUES ('{inactive_project}', 'Completed fixture', NULL, 'completed', 'p1');"
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
        assert!(!report.to_string().contains("fixture-project-authorization"));
        report
    };
    let dolt = fixture.dolt.to_str().unwrap();
    let start = ["workspace", "start", "--dolt-bin", dolt, "--port", &port];
    run(&start, 0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let evidence = fixture.root.join("project-evidence.json");
        let target = [
            "--workspace-id",
            &workspace_id,
            "--database",
            "db",
            "--port",
            &port,
        ];
        let declaration = |request: &Request<'_>| {
            let now = fixture_now();
            json!({
                    "version": 1, "operation": request.payload[1], "workspace_id": workspace_id,
                    "request_id": request.id, "fingerprint": hash(request.payload.to_string().as_bytes()),
                    "actor_ref": request.evidence_actor, "membership": request.membership,
                    "authorization_ref": "fixture-project-authorization",
                    "evidence_id": "fixture-project-evidence", "revision": "r1",
                    "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
                    "valid_from": fixture_timestamp(now - time::Duration::hours(1)),
                    "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
                    "withdrawn": false,
            })
        };
        let submit = |request: &Request<'_>, exit| {
            fs::write(
                &evidence,
                json!({"decision": "allow", "evidence": declaration(request)}).to_string(),
            )
            .unwrap();
            let mut args = vec!["project"];
            args.extend_from_slice(&request.args);
            args.extend(target);
            args.extend([
                "--request-id",
                request.id.as_str(),
                "--actor",
                ACTOR,
                "--evidence-file",
                evidence.to_str().unwrap(),
            ]);
            let project_match;
            let issue_match;
            if !request.membership.is_null() {
                let field = |name: &str| request.membership[name].as_str().unwrap();
                project_match = format!(
                    "{}={}",
                    field("project_id"),
                    field("expected_project_revision")
                );
                issue_match = format!("{}={}", field("issue_id"), field("expected_issue_revision"));
                // Both selectors are keyed by ID, independently of option order.
                args.extend(["--if-match", &issue_match, "--if-match", &project_match]);
            }
            run(&args, exit)
        };

        let mut reader = connect_owned(&state, &port, "reader");
        let baseline = storage_state(&mut reader);
        let full_issue_rows = |db: &mut Conn| {
            db.query::<mysql::Row, _>(
                "SELECT id, number, area_id, parent_id, title, body, acceptance_criteria, type,
                    status, priority, hold_reason, created_at, updated_at, closed_at,
                    closed_by_ref, resolution_note FROM issues ORDER BY number",
            )
            .unwrap()
        };
        let issues_before = full_issue_rows(&mut reader);
        let create = Request::create(50, NAME, &workspace_id);
        let created = submit(&create, 0);
        assert_eq!(created["data"]["outcome"], "confirmed");
        assert_eq!(created["data"]["base_commit"], baseline.0);
        let project = created["data"]["result"]["project_id"].as_str().unwrap();
        let project_r1 = created["data"]["result"]["project_revision"]
            .as_str()
            .unwrap();
        let revision = id(4);
        let area = id(2);
        let subarea = id(3);
        let executing = id(11);
        let closed = id(12);
        let first = Request::add(
            51,
            &workspace_id,
            project,
            &executing,
            &subarea,
            project_r1,
            &revision,
        );
        let added_first = submit(&first, 0);
        let project_r2 = added_first["data"]["result"]["project_revision"]
            .as_str()
            .unwrap();
        let executing_r2 = added_first["data"]["result"]["membership"]["issue_revision"]
            .as_str()
            .unwrap();
        assert_ne!(project_r1, project_r2);
        assert_ne!(executing_r2, revision);
        let committed_first = storage_state(&mut reader);
        assert_eq!(committed_first.0, added_first["data"]["confirmed_commit"]);

        let make_second = |request, owner, project_rev, issue_rev| {
            Request::add(
                request,
                &workspace_id,
                project,
                &closed,
                owner,
                project_rev,
                issue_rev,
            )
        };
        let mut unauthorized = make_second(52, &area, project_r2, &revision);
        unauthorized.evidence_actor = "fixture-other-actor";
        for (request, exit, code) in [
            (unauthorized, 7, "AUTHORITY_UNVERIFIED"),
            (
                make_second(52, &subarea, project_r2, &revision),
                7,
                "AUTHORITY_UNVERIFIED",
            ),
            (
                make_second(52, &area, project_r1, &revision),
                5,
                "STALE_REVISION",
            ),
            (
                make_second(52, &area, project_r2, executing_r2),
                5,
                "STALE_REVISION",
            ),
            (
                Request::add(
                    52,
                    &workspace_id,
                    &inactive_project,
                    &closed,
                    &area,
                    "p1",
                    &revision,
                ),
                4,
                "INVALID_TRANSITION",
            ),
            (
                Request::add(
                    52,
                    &workspace_id,
                    project,
                    &executing,
                    &subarea,
                    project_r2,
                    executing_r2,
                ),
                5,
                "ALREADY_EXISTS",
            ),
            (
                make_second(51, &area, project_r2, &revision),
                5,
                "REQUEST_CONFLICT",
            ),
        ] {
            let refused = submit(&request, exit);
            assert_eq!(refused["error"]["code"], code);
            assert_eq!(storage_state(&mut reader), committed_first);
        }

        let second = make_second(52, &area, project_r2, &revision);
        let added_second = submit(&second, 0);
        let head = storage_state(&mut reader);
        assert_eq!(head.0, added_second["data"]["confirmed_commit"]);
        assert_eq!(full_issue_rows(&mut reader), issues_before);

        let tables: Vec<(String, u64, u64, u64)> = reader
            .exec(
                "SELECT table_name, rows_added, rows_deleted, rows_modified
             FROM DOLT_DIFF_STAT(?, ?) ORDER BY table_name",
                (&baseline.0, &head.0),
            )
            .unwrap();
        assert_eq!(
            tables,
            [
                ("issues".to_owned(), 0, 0, 2),
                ("operation_receipts".to_owned(), 3, 0, 0),
                ("project_issues".to_owned(), 2, 0, 0),
                ("projects".to_owned(), 1, 0, 0),
            ]
        );
        let mut show = vec!["project", "show", project, "--contract", "2", "--json"];
        show.extend(target);
        // Project reads use the authenticated endpoint without a workspace path.
        let shown = Command::new(env!("CARGO_BIN_EXE_hive"))
            .env(
                "HIVE_DOLT_USER",
                format!("reader_{}", &launch_nonce(&state).replace('-', "")[..24]),
            )
            .env(
                "HIVE_DOLT_PASSWORD",
                fs::read_to_string(
                    state.join(format!("run-{}/reader-password", launch_nonce(&state))),
                )
                .unwrap()
                .trim(),
            )
            .current_dir(&fixture.root)
            .args(show)
            .output()
            .unwrap();
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(shown["data"]["project"]["status"], "active", "{shown}");
        assert_eq!(shown["data"]["project"]["name"], NAME);
        assert_eq!(shown["data"]["issues"]["items"], json!([executing, closed]));
        assert_eq!(
            shown["data"]["project"]["revision"],
            added_second["data"]["result"]["project_revision"]
        );

        for (request, original) in [
            (&create, &created),
            (&first, &added_first),
            (&second, &added_second),
        ] {
            let replayed = submit(request, 0);
            assert_eq!(replayed["data"]["outcome"], "replayed");
            for field in ["base_commit", "confirmed_commit", "result"] {
                assert_eq!(replayed["data"][field], original["data"][field]);
            }
        }
        assert_eq!(storage_state(&mut reader), head);

        let receipt: (String, String, String, String, String, String, String, String) = reader.exec_first(
            "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                    DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
             FROM operation_receipts WHERE request_id = ?",
            (&second.id,),
        ).unwrap().unwrap();
        let session: u64 = reader
            .query_first("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        drop(reader);
        let mut intent = json!({
            "version": 4, "kind": "project.add-issue", "workspace_id": workspace_id,
            "launch_nonce": launch_nonce(&state), "session_id": session, "request_id": second.id,
            "base_commit": receipt.4, "fingerprint": receipt.2, "project_id": project,
            "area_id": area, "receipt_digest": hash(json!(receipt).to_string().as_bytes()),
        });
        let marker = state.join("pending-request.json");
        let record = |intent: &Value| {
            assert!(!marker.exists());
            fs::write(&marker, intent.to_string()).unwrap();
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        };
        let recover = |outcome| {
            let report = run(&["workspace", "stop"], 0);
            assert_eq!(report["data"]["process_exit"], "pidfd_verified");
            assert_eq!(report["data"]["pending_request"], "recorded");
            assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
            let report = run(&["workspace", "recover", "--dolt-bin", dolt], 0);
            assert_eq!(report["data"]["outcome"], outcome);
            assert!(!marker.exists());
            run(&start, 0);
            assert_eq!(
                storage_state(&mut connect_owned(&state, &port, "reader")),
                head
            );
        };
        record(&intent);
        recover("confirmed");

        let mut writer = connect_owned(&state, &port, "writer");
        writer.query_drop("START TRANSACTION").unwrap();
        writer
            .exec_drop(
                "INSERT INTO project_issues VALUES (?, ?)",
                (project, id(13)),
            )
            .unwrap();
        intent["launch_nonce"] = json!(launch_nonce(&state));
        intent["session_id"] = json!(
            writer
                .query_first::<u64, _>("SELECT CONNECTION_ID()")
                .unwrap()
                .unwrap()
        );
        intent["request_id"] = json!(id(55));
        intent["base_commit"] = json!(head.0);
        intent["area_id"] = json!(subarea);
        record(&intent);
        assert_eq!(submit(&create, 8)["error"]["code"], "COMMIT_UNCERTAIN");
        drop(writer);
        recover("rolled_back");

        // A synthetic receipt with the right row counts and fingerprint is
        // still unverified if its commit edits a protected field on that Issue.
        let dependent = id(13);
        let project_r3 = added_second["data"]["result"]["project_revision"]
            .as_str()
            .unwrap();
        let forged = Request::add(
            56,
            &workspace_id,
            project,
            &dependent,
            &subarea,
            project_r3,
            &revision,
        );
        let proof = declaration(&forged);
        let result = json!({
            "version": 1, "project_id": project, "project_revision": id(91),
            "membership": {"area_id": subarea, "issue_id": dependent, "issue_revision": id(92)},
        });
        let mut writer = connect_owned(&state, &port, "writer");
        writer
            .query_drop("SET SESSION dolt_transaction_commit=1")
            .unwrap();
        writer.query_drop("START TRANSACTION").unwrap();
        writer
            .exec_drop(
                "UPDATE projects SET revision=? WHERE id=?",
                (id(91), project),
            )
            .unwrap();
        writer
            .exec_drop(
                "UPDATE issues SET revision=?, title='dependent' WHERE id=?",
                (id(92), &dependent),
            )
            .unwrap();
        writer
            .exec_drop(
                "INSERT INTO project_issues VALUES (?, ?)",
                (project, &dependent),
            )
            .unwrap();
        writer
            .exec_drop(
                "INSERT INTO operation_receipts (request_id, kind, actor_ref, fingerprint,
             authorization_ref, base_commit, workflow_evidence_json, result_json, created_at)
             VALUES (?, 'project.add-issue', ?, ?, ?, ?, ?, ?, UTC_TIMESTAMP(6))",
                (
                    &forged.id,
                    ACTOR,
                    proof["fingerprint"].as_str().unwrap(),
                    proof["authorization_ref"].as_str().unwrap(),
                    &head.0,
                    proof.to_string(),
                    result.to_string(),
                ),
            )
            .unwrap();
        writer.query_drop("COMMIT").unwrap();
        drop(writer);
        let mut reader = connect_owned(&state, &port, "reader");
        let forged_head = storage_state(&mut reader);
        assert_ne!(forged_head.0, head.0);
        assert_eq!(submit(&forged, 8)["error"]["code"], "COMMIT_UNCERTAIN");
        assert_eq!(storage_state(&mut reader), forged_head);
        assert!(!marker.exists());
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

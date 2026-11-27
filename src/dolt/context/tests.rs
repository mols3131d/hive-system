use std::env;
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use serde_json::{Value, json};

use super::workflow::{CurrentInput, Expected, Locator, Operation, RecordKind, Request};
use super::*;
use crate::cli::DoltTargetArgs;
use crate::dolt::INITIAL_SCHEMA;

const DATABASE: &str = "fixture_context";
const WORKSPACE: &str = "01900000-0000-7000-8000-000000000001";
const ROOT: &str = "01900000-0000-7000-8000-000000000002";
const AREA: &str = "01900000-0000-7000-8000-000000000003";
const ISSUE: &str = "01900000-0000-7000-8000-00000000001a";
const OTHER: &str = "01900000-0000-7000-8000-000000000020";
const PROJECT: &str = "01900000-0000-7000-8000-000000000030";
const UNRELATED: &str = "01900000-0000-7000-8000-000000000031";
const HANDOFF: &str = "01900000-0000-7000-8000-000000000040";
const PROGRESS: &str = "01900000-0000-7000-8000-000000000041";
const ARTIFACT_ISSUE: &str = "01900000-0000-7000-8000-000000000021";
const ARTIFACT: &str = "01900000-0000-7000-8000-000000000051";
const INVOCATION: &str = "01900000-0000-7000-8000-000000000061";
const DIRECTIVE: &str = "01900000-0000-7000-8000-000000000071";
const RECEIPT: &str = "01900000-0000-7000-8000-000000000081";
const MALFORMED_RECEIPT: &str = "01900000-0000-7000-8000-000000000082";
const AT: &str = "2026-01-01 00:00:00";

struct Fixture {
    root: PathBuf,
    server: Option<Child>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut server) = self.server.take() {
            let _ = server.kill();
            let _ = server.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn committed_scope_and_resume() {
    let binary = PathBuf::from(env::var_os("HIVE_DOLT_BIN").expect("set HIVE_DOLT_BIN"));
    assert!(binary.is_absolute());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = env::temp_dir().join(format!("hive-context-{}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let mut fixture = Fixture { root, server: None };
    let database = fixture.root.join(DATABASE);
    fs::create_dir(&database).unwrap();
    let dolt = || {
        let mut command = Command::new(&binary);
        command
            .current_dir(&database)
            .env_clear()
            .env("HOME", &fixture.root)
            .env("DOLT_ROOT_PATH", &fixture.root)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true");
        command
    };
    let run = |args: &[&str]| {
        let output = dolt().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "Dolt {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&[
        "init",
        "--initial-branch",
        "main",
        "--name",
        "Fixture",
        "--email",
        "fixture@example.invalid",
    ]);
    let digest = format!("{:x}", Sha256::digest(INITIAL_SCHEMA));
    let schema = std::str::from_utf8(INITIAL_SCHEMA).unwrap();
    let aliased_issue = ISSUE.to_ascii_uppercase();
    let mut seed = format!(
        "ALTER DATABASE {DATABASE} COLLATE utf8mb4_0900_ai_ci;
         {schema}
         INSERT INTO workspace_meta VALUES (1, '{WORKSPACE}');
         INSERT INTO schema_migrations VALUES (1, '{digest}', '{AT}');
         INSERT INTO workspace_counters VALUES ('issue_number', 4);
         INSERT INTO areas (id, slug, name, revision, archived_at)
         VALUES ('{ROOT}', 'root', 'Root', 'r1', '{AT}');
         INSERT INTO areas (id, parent_id, slug, name, revision)
         VALUES ('{AREA}', '{ROOT}', 'child', 'Child', 'r1');
         INSERT INTO projects (id, name, status, revision) VALUES
         ('{PROJECT}', 'Project', 'active', 'r1'), ('{UNRELATED}', 'Other', 'active', 'r1');
         INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
         VALUES ('{OTHER}', 2, '{ROOT}', 'Other Issue', 'task', 'todo', 'r1', '{AT}', '{AT}'),
                ('{ARTIFACT_ISSUE}', 3, '{AREA}', 'Artifact Issue', 'task', 'todo', 'r1', '{AT}', '{AT}');
         INSERT INTO issues (id, number, area_id, parent_id, title, body, acceptance_criteria,
             type, status, revision, created_at, updated_at)
         VALUES ('{ISSUE}', 1, '{AREA}', '{OTHER}', 'Task', 'é', 'OK', 'task', 'todo', 'r1', '{AT}', '{AT}');
         INSERT INTO project_issues VALUES ('{PROJECT}', '{ISSUE}'), ('{PROJECT}', '{OTHER}'), ('{UNRELATED}', '{aliased_issue}');
         INSERT INTO issue_dependencies VALUES ('{ISSUE}', '{OTHER}');
         INSERT INTO issue_updates VALUES
         ('{HANDOFF}', '{ISSUE}', 'handoff', 'fixture handoff', 'fixture-actor', '{AT}', NULL),
         ('{PROGRESS}', '{ISSUE}', 'progress', 'fixture progress', 'fixture-actor', '{AT}', NULL),
         ('01900000-0000-7000-8000-000000000039', '{OTHER}', 'handoff', 'unrelated', 'fixture-actor', '{AT}', NULL),
         ('01900000-0000-7000-8000-00000000004a', '{ISSUE}', 'handoff', 'tie', 'fixture-actor', '{AT}', NULL);
         INSERT INTO artifacts (id, kind, title, body, author_ref, revision, created_at, substantive_updated_at)
         VALUES ('{ARTIFACT}', 'handoff', 'Handoff', 'é', 'fixture-author', 'artifact-r1', '{AT}', '{AT}'),
                ('01900000-0000-7000-8000-000000000052', 'handoff', 'Ambiguous', 'fixture', 'fixture-author', 'artifact-r1', '{AT}', '{AT}');
         INSERT INTO artifact_issues VALUES ('{ARTIFACT}', '{ARTIFACT_ISSUE}'), ('01900000-0000-7000-8000-000000000052', '{OTHER}');
         INSERT INTO artifact_projects VALUES ('{ARTIFACT}', '{PROJECT}');
         INSERT INTO project_issues VALUES ('{PROJECT}', '{ARTIFACT_ISSUE}');
         INSERT INTO directives (id, scope_kind, area_id, project_id, include_descendants,
             title, body, status, issuer_ref, source_ref, approval_ref, revision, created_at, updated_at)
         VALUES
         ('{DIRECTIVE}', 'workspace', NULL, NULL, 0, 'Workspace', 'fixture', 'active', 'issuer', 'source', 'approval', 'r1', '{AT}', '{AT}'),
         ('01900000-0000-7000-8000-000000000072', 'area', '{ROOT}', NULL, 0, 'Root only', 'fixture', 'active', 'issuer', 'source', 'approval', 'r1', '{AT}', '{AT}'),
         ('01900000-0000-7000-8000-000000000073', 'area', '{ROOT}', NULL, 1, 'Root cascade', 'fixture', 'active', 'issuer', 'source', 'approval', 'r1', '{AT}', '{AT}'),
         ('01900000-0000-7000-8000-000000000074', 'area', '{AREA}', NULL, 0, 'Exact Area', 'fixture', 'active', 'issuer', 'source', 'approval', 'r1', '{AT}', '{AT}'),
         ('01900000-0000-7000-8000-000000000075', 'project', NULL, '{PROJECT}', 0, 'Project', 'fixture', 'active', 'issuer', 'source', 'approval', 'r1', '{AT}', '{AT}'),
         ('01900000-0000-7000-8000-000000000076', 'workspace', NULL, NULL, 0, 'Upcoming', 'fixture', 'active', 'issuer', 'source', 'approval', 'r1', '{AT}', '{AT}');
         UPDATE directives SET valid_from = '2027-01-01' WHERE id = '01900000-0000-7000-8000-000000000076';
         INSERT INTO operation_receipts
             (request_id, kind, fingerprint, actor_ref, authorization_ref,
              workflow_evidence_json, base_commit, result_json, created_at)
         VALUES
         ('{RECEIPT}', 'directive.activate', REPEAT('a', 64), 'fixture-reviewer', 'approval', '{{}}', REPEAT('a', 32), '{{}}', '{AT}'),
         ('{MALFORMED_RECEIPT}', 'directive.activate', REPEAT('a', 64), 'fixture-reviewer', 'approval', '{{}}', REPEAT('a', 32), '[]', '{AT}');
         CREATE USER 'fixture_reader'@'127.0.0.1' IDENTIFIED BY 'synthetic-password';
         GRANT SELECT ON {DATABASE}.* TO 'fixture_reader'@'127.0.0.1';"
    );
    for n in 0..25 {
        use std::fmt::Write as _;
        writeln!(seed,
            "INSERT INTO issue_updates VALUES
             ('01900000-0000-7000-8000-00000001{n:04x}', '{ISSUE}', 'evidence', 'fixture evidence', 'fixture-actor', '{AT}', NULL);"
        ).unwrap();
    }
    let mut sql = dolt()
        .arg("sql")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    sql.stdin
        .take()
        .unwrap()
        .write_all(seed.as_bytes())
        .unwrap();
    assert!(sql.wait().unwrap().success());
    run(&["add", "."]);
    run(&["commit", "-m", "Synthetic context"]);
    // A dirty Issue edit must remain outside the captured committed state.
    run(&[
        "sql",
        "-q",
        &format!("UPDATE issues SET body = 'dirty' WHERE id = '{ISSUE}'"),
    ]);

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    fixture.server = Some(
        dolt()
            .args([
                "sql-server",
                "--host",
                "127.0.0.1",
                "--port",
                &address.port().to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(address).is_err() {
        assert!(
            fixture
                .server
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none()
        );
        assert!(Instant::now() < deadline, "fixture server did not start");
        thread::sleep(Duration::from_millis(50));
    }

    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "dolt::issue::context::tests::read_synthetic_snapshot",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env("HIVE_CONTEXT_FIXTURE_PORT", address.port().to_string())
        .env("HIVE_DOLT_USER", "fixture_reader")
        .env("HIVE_DOLT_PASSWORD", "synthetic-password")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Synthetic snapshot reader failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[test]
#[ignore = "run via committed_scope_and_resume"]
fn read_synthetic_snapshot() {
    let Some(port) = env::var_os("HIVE_CONTEXT_FIXTURE_PORT") else {
        return;
    };
    let target = DoltTargetArgs {
        workspace_id: Some(WORKSPACE.to_owned()),
        database: Some(DATABASE.to_owned()),
        port: Some(port),
    };
    let (mut snapshot, workspace) = Snapshot::open(&target).unwrap();
    assert_eq!(workspace, WORKSPACE);

    for (area, project, issue, code) in [
        (None, None, None, ErrorCode::AmbiguousScope),
        (Some(ROOT), None, Some("#1"), ErrorCode::InvalidArgument),
        (
            None,
            Some(UNRELATED),
            Some("#1"),
            ErrorCode::InvalidArgument,
        ),
        (None, None, Some("#4"), ErrorCode::NotFound),
    ] {
        assert_eq!(
            resolve_scope(&mut snapshot, area, project, issue)
                .unwrap_err()
                .code as u8,
            code as u8
        );
    }

    let project_only = resolve_scope(&mut snapshot, None, Some(PROJECT), None).unwrap();
    assert!(project_only.ancestry.is_empty());
    assert!(
        collect_work(&mut snapshot, &project_only, None)
            .unwrap()
            .is_none()
    );
    let issue_only = resolve_scope(&mut snapshot, None, None, Some(ISSUE)).unwrap();
    assert!(issue_only.project.is_none());
    let scope = resolve_scope(&mut snapshot, Some(AREA), Some(PROJECT), Some("#1")).unwrap();
    assert_eq!(scope.db_commit, snapshot.commit);
    assert_eq!(
        scope
            .ancestry
            .iter()
            .map(|area| area.id.as_str())
            .collect::<Vec<_>>(),
        [AREA, ROOT]
    );
    assert_eq!(scope.project.as_ref().unwrap().id, PROJECT);
    assert!(
        matches!(collect_work(&mut snapshot, &scope, Some(7)), Err(e) if matches!(e.code, ErrorCode::ContextLimit))
    );
    let work = collect_work(&mut snapshot, &scope, Some(8))
        .unwrap()
        .unwrap();
    assert_eq!(work.inline_bytes, 8);
    assert_eq!(work.issue.body.as_deref(), Some("é"));
    assert_eq!(work.issue.acceptance_criteria.as_deref(), Some("OK"));
    assert!(work.issue.parent_id.is_none());
    assert!(work.prerequisites.is_empty());
    assert_eq!(work.omitted_prerequisites, 0);
    assert!(work.prerequisite_blocked);
    assert!(!work.claimed);
    assert!(work.archived_ancestry);
    assert_eq!(work.handoff.as_ref().unwrap().id, HANDOFF);
    assert_eq!(work.updates.len(), REFERENCE_LIMIT);
    assert_eq!(work.updates[0].id, PROGRESS);
    assert_eq!(work.omitted_updates, 1);
    let request = request(None);
    assert!(matches!(
        resume::read_required(&mut snapshot, &scope, &work, 100, &request),
        Err(e) if matches!(e.code, ErrorCode::AuthorityUnverified)
    ));
    let evidence = declaration(
        &snapshot.commit,
        ISSUE,
        "issue_update",
        HANDOFF,
        &snapshot.commit,
        "read_handoff_body",
    );
    let bytes = serde_json::to_vec(&evidence).unwrap();
    let request = Request {
        input: Some(CurrentInput::OperatorRequest(&bytes)),
        ..request
    };
    assert!(matches!(
        resume::read_required(&mut snapshot, &scope, &work, 8 + 14, &request),
        Err(e) if matches!(e.code, ErrorCode::ContextLimit)
    ));
    let body = resume::read_required(&mut snapshot, &scope, &work, 8 + 15, &request)
        .unwrap()
        .unwrap();
    assert_eq!(body.id, HANDOFF);
    assert_eq!(body.revision, snapshot.commit);
    assert_eq!(body.inline_bytes, 15);
    assert_eq!(body.body, "fixture handoff");

    let candidates = directives::collect(&mut snapshot, &scope, request.observed_at).unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| &candidate.id[35..])
            .collect::<Vec<_>>(),
        ["1", "3", "4", "5"]
    );
    assert_eq!(candidates[0].id, DIRECTIVE);
    assert_eq!(candidates[0].revision, "r1");
    assert_eq!(candidates[0].source_ref, "source");
    assert_eq!(candidates[0].issuer_ref, "issuer");
    let receipt =
        directives::lookup_activation_receipt(&mut snapshot, &candidates[0], RECEIPT).unwrap();
    assert_eq!(receipt.actor_ref, "fixture-reviewer");
    assert_eq!(receipt.workflow_evidence, json!({}));
    assert_eq!(receipt.result, json!({}));
    assert!(matches!(
        directives::lookup_activation_receipt(&mut snapshot, &candidates[0], MALFORMED_RECEIPT),
        Err(error) if matches!(error.code, ErrorCode::InvalidRecord)
    ));

    let evidence = declaration(
        &snapshot.commit,
        ARTIFACT_ISSUE,
        "artifact",
        ARTIFACT,
        "artifact-r1",
        "disclose_artifact_body",
    );
    let bytes = serde_json::to_vec(&evidence).unwrap();
    let request = Request {
        input: Some(CurrentInput::OperatorRequest(&bytes)),
        ..request
    };
    let artifact_scope = resolve_scope(&mut snapshot, None, None, Some(ARTIFACT_ISSUE)).unwrap();
    let artifact_work = collect_work(&mut snapshot, &artifact_scope, None)
        .unwrap()
        .unwrap();
    assert!(matches!(
        resume::read_required(&mut snapshot, &artifact_scope, &artifact_work, MAX_INLINE_BYTES, &request),
        Err(e) if matches!(e.code, ErrorCode::Unauthorized)
    ));
    let artifact_scope =
        resolve_scope(&mut snapshot, None, Some(PROJECT), Some(ARTIFACT_ISSUE)).unwrap();
    let body = resume::read_required(
        &mut snapshot,
        &artifact_scope,
        &artifact_work,
        artifact_work.inline_bytes + 2,
        &request,
    )
    .unwrap()
    .unwrap();
    assert_eq!(body.body, "é");
    let competing_scope = resolve_scope(&mut snapshot, None, None, Some(OTHER)).unwrap();
    let competing_work = collect_work(&mut snapshot, &competing_scope, None)
        .unwrap()
        .unwrap();
    assert!(matches!(
        resume::read_required(&mut snapshot, &competing_scope, &competing_work, MAX_INLINE_BYTES, &request),
        Err(e) if matches!(e.code, ErrorCode::AmbiguousScope)
    ));
    let again = collect_work(&mut snapshot, &scope, None).unwrap().unwrap();
    assert_eq!(again.handoff, work.handoff);
    assert_eq!(again.updates, work.updates);
}

#[test]
fn measures_whole_utf8_issue_fields() {
    let issue = IssueDetail {
        id: ISSUE.to_owned(),
        number: 1,
        area_id: AREA.to_owned(),
        parent_id: None,
        title: "Task".to_owned(),
        body: Some("é".to_owned()),
        acceptance_criteria: Some("OK".to_owned()),
        kind: "task".to_owned(),
        status: "todo".to_owned(),
        priority: None,
        hold_reason: None,
        revision: "r1".to_owned(),
        created_at: AT.to_owned(),
        updated_at: AT.to_owned(),
        closed_at: None,
        closed_by_ref: None,
        resolution_note: None,
    };
    assert_eq!(issue.inline_bytes(), 8);
}

#[test]
fn prerequisite_summaries_keep_cross_area_blockers_opaque() {
    let rows = vec![
        (
            OTHER.to_owned(),
            Some(ROOT.to_owned()),
            Some("task".to_owned()),
            Some("todo".to_owned()),
            false,
        ),
        (
            PROGRESS.to_owned(),
            Some(AREA.to_owned()),
            Some("task".to_owned()),
            Some("done".to_owned()),
            true,
        ),
    ];
    assert_eq!(
        summarize_prerequisites(rows, ISSUE, AREA).unwrap(),
        (vec![PROGRESS.to_owned()], 0, true)
    );

    let rows = vec![(
        OTHER.to_owned(),
        Some(ROOT.to_owned()),
        Some("task".to_owned()),
        Some("done".to_owned()),
        false,
    )];
    assert!(matches!(
        summarize_prerequisites(rows, ISSUE, AREA).unwrap_err().code,
        ErrorCode::InvalidRecord
    ));
}

fn request(input: Option<CurrentInput<'_>>) -> Request<'_> {
    Request {
        actor_ref: "fixture-actor",
        bot_id: None,
        invocation_id: INVOCATION,
        observed_at: workflow::parse_time("2026-01-02T00:00:00Z").unwrap(),
        input,
    }
}

fn declaration(
    commit: &str,
    issue: &str,
    kind: &str,
    id: &str,
    revision: &str,
    operation: &str,
) -> Value {
    json!({
        "version": 1, "evidence_id": "fixture-evidence", "revision": "evidence-r1",
        "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
        "workspace_id": WORKSPACE, "actor_ref": "fixture-actor",
        "area_id": AREA, "issue_id": issue, "operations": [operation],
        "record": {"kind": kind, "id": id, "revision": revision},
        "invocation_id": INVOCATION, "db_commit": commit,
        "valid_from": "2026-01-01T00:00:00Z", "expires_at": "2026-01-03T00:00:00Z",
        "withdrawn": false,
    })
}

#[test]
fn validates_current_exact_declaration_and_rejects_historical_or_mismatched_evidence() {
    let commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let expected = Expected {
        workspace_id: WORKSPACE,
        db_commit: commit,
        area_id: AREA,
        issue_id: ISSUE,
        operation: Operation::ReadHandoffBody,
        record_kind: RecordKind::IssueUpdate,
        record_id: HANDOFF,
        record_revision: commit,
    };
    let evidence = declaration(
        commit,
        ISSUE,
        "issue_update",
        HANDOFF,
        commit,
        "read_handoff_body",
    );
    let bytes = serde_json::to_vec(&evidence).unwrap();
    assert!(
        workflow::verify(
            &request(Some(CurrentInput::OperatorRequest(&bytes))),
            &expected
        )
        .is_ok()
    );
    assert!(matches!(
        workflow::verify(&request(None), &expected)
            .unwrap_err()
            .code,
        ErrorCode::AuthorityUnverified
    ));
    for (field, value, code) in [
        ("area_id", json!(ROOT), ErrorCode::Unauthorized),
        ("issue_id", json!(OTHER), ErrorCode::Unauthorized),
        ("workspace_id", json!(OTHER), ErrorCode::Unauthorized),
        ("bot_id", json!(OTHER), ErrorCode::Unauthorized),
        ("actor_ref", json!("other-actor"), ErrorCode::Unauthorized),
        ("invocation_id", json!(OTHER), ErrorCode::Unauthorized),
        (
            "db_commit",
            json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            ErrorCode::Unauthorized,
        ),
        (
            "operations",
            json!(["disclose_artifact_body"]),
            ErrorCode::Unauthorized,
        ),
        (
            "record",
            json!({"kind": "issue_update", "id": PROGRESS, "revision": commit}),
            ErrorCode::Unauthorized,
        ),
        ("withdrawn", json!(true), ErrorCode::AuthorityUnverified),
        (
            "expires_at",
            json!("2026-01-02T00:00:00Z"),
            ErrorCode::AuthorityUnverified,
        ),
        ("project_id", json!(PROJECT), ErrorCode::InvalidArgument),
        (
            "workflow_evidence_json",
            json!({}),
            ErrorCode::InvalidArgument,
        ),
    ] {
        let mut changed = evidence.clone();
        changed[field] = value;
        let changed = serde_json::to_vec(&changed).unwrap();
        assert_eq!(
            workflow::verify(
                &request(Some(CurrentInput::OperatorRequest(&changed))),
                &expected
            )
            .unwrap_err()
            .code as u8,
            code as u8,
            "{field}"
        );
    }

    for (revision, succeeds) in [("evidence-r1", true), ("evidence-r2", false)] {
        let selected = Locator {
            evidence_id: "fixture-evidence",
            revision,
            source_ref: "fixture-source",
            issuer_ref: "fixture-issuer",
        };
        let live = request(Some(CurrentInput::ResolvedSource {
            selected,
            bytes: &bytes,
        }));
        assert_eq!(workflow::verify(&live, &expected).is_ok(), succeeds);
    }
    let oversized = vec![b' '; 16 * 1024 + 1];
    assert!(matches!(
        workflow::verify(
            &request(Some(CurrentInput::OperatorRequest(&oversized))),
            &expected
        )
        .unwrap_err()
        .code,
        ErrorCode::ContextLimit
    ));
}

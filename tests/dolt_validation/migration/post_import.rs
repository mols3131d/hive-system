use std::ffi::OsStr;
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Output};

use mysql::prelude::Queryable;
use serde_json::{Value, json};

use crate::runtime::{
    IssueDecision, connect_owned, fixture_credentials, register_fixture_root, stored_receipt,
};
use crate::{HOST, fs, id};

use super::git_text;

pub(super) struct StagedTarget<'a> {
    pub(super) root: &'a Path,
    pub(super) home: &'a Path,
    pub(super) target: &'a Path,
    pub(super) workspace: &'a str,
    pub(super) area: &'a str,
    pub(super) dolt: &'a OsStr,
    pub(super) report: &'a Value,
}

pub(super) fn exercise(
    staged: StagedTarget<'_>,
    cli: impl Fn() -> Command,
    restage: impl Fn() -> Command,
    sql: impl Fn(&str) -> Value,
) {
    let StagedTarget {
        root,
        home,
        target,
        workspace,
        area,
        dolt,
        report,
    } = staged;
    let baseline = report["target"]["confirmed_dolt_commit"].as_str().unwrap();
    let assets = report["target"]["asset_git_commit"].as_str().unwrap();
    let n = report["counts"]["tasks"]["imported"].as_u64().unwrap();
    assert!(n > 0);
    let snapshot = || {
        let row = sql("SELECT DOLT_HASHOF('HEAD') AS head,
            DOLT_HASHOF_DB('HEAD') AS committed, DOLT_HASHOF_DB('WORKING') AS working,
            DOLT_HASHOF_DB('STAGED') AS staged,
            (SELECT next_value FROM workspace_counters WHERE name = 'issue_number') AS next_number,
            (SELECT COUNT(*) FROM operation_receipts) AS receipts");
        assert_eq!(row[0]["working"], row[0]["committed"]);
        assert_eq!(row[0]["staged"], row[0]["committed"]);
        assert_eq!(
            git_text(&target.join("assets"), home, &["rev-parse", "HEAD"]),
            assets
        );
        assert!(
            git_text(
                &target.join("assets"),
                home,
                &["status", "--porcelain", "--untracked-files=all"]
            )
            .is_empty()
        );
        row[0].clone()
    };
    let initial = snapshot();
    assert_eq!(initial["head"], baseline);
    assert_eq!(initial["next_number"], n + 1);
    assert_eq!(initial["receipts"], 0);
    assert_eq!(report["checks"]["published_inactive"], true);
    assert!(!root.join("state").exists());

    // Exercise real v2 writers using a fixture-only registration for the
    // disposable target. The source and the original XDG state stay untouched.
    let state_home = root.join("post-import-state");
    let state = state_home.join("hive/v2/workspaces").join(workspace);
    register_fixture_root(target, &state, workspace);
    // Backup takes the pre-existing stable lock used by a fresh registration.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(state.join("lock"))
        .unwrap();
    let registration = fs::read(state.join("active-root.json")).unwrap();
    let runtime_cli = || {
        let mut command = cli();
        command.env("XDG_STATE_HOME", &state_home);
        command
    };
    let workspace_cli = || {
        let mut command = runtime_cli();
        command.arg("--workspace").arg(target);
        command
    };
    let backup = root.join("pre-write-backup");
    let backup_receipt = response(
        workspace_cli()
            .args(["backup", "create", "--destination"])
            .arg(&backup)
            .output()
            .unwrap(),
        0,
    );
    let backup_manifest = fs::read(backup.join("manifest.json")).unwrap();
    assert_eq!(backup_receipt["data"]["manifest"]["dolt_commit"], baseline);
    assert_eq!(
        backup_receipt["data"]["manifest"]["asset_git_commit"],
        assets
    );
    assert_eq!(snapshot(), initial);

    let listener = TcpListener::bind((HOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    let running = RunningFixture(Some({
        let mut stop = workspace_cli();
        stop.args(["workspace", "stop"]);
        stop
    }));
    response(
        workspace_cli()
            .args(["workspace", "start", "--port", &port, "--dolt-bin"])
            .arg(dolt)
            .output()
            .unwrap(),
        0,
    );
    let mut reader = connect_owned(&state, &port, "reader");
    let (user, password) = fixture_credentials(&state, "reader");
    let validation = response(
        runtime_cli()
            .env("HIVE_DOLT_USER", user)
            .env("HIVE_DOLT_PASSWORD", password)
            .args([
                "validate",
                "--all",
                "--database",
                "db",
                "--port",
                &port,
                "--workspace-id",
                workspace,
            ])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(validation["context"]["db_commit"], baseline);
    assert_eq!(validation["data"]["diagnostics"], json!([]));
    let issue_rows = |reader: &mut mysql::Conn| {
        reader
            .query::<(u64, String, String), _>(
                "SELECT number, id, status FROM issues ORDER BY number",
            )
            .unwrap()
    };
    let mut expected = issue_rows(&mut reader);
    assert_eq!(
        expected.iter().map(|row| row.0).collect::<Vec<_>>(),
        (1..=n).collect::<Vec<_>>()
    );

    const ACTOR: &str = "synthetic-a15-operator";
    const REASON: &str = "Synthetic cancellation after import";
    let evidence_file = root.join("post-import-decision.json");
    let target_flags = [
        "--database",
        "db",
        "--port",
        port.as_str(),
        "--workspace-id",
        workspace,
    ];
    let create = |request: &str, title: &str| {
        let decision = IssueDecision {
            workspace,
            area,
            actor: ACTOR,
            request,
        }
        .create(title, None, 1);
        fs::write(&evidence_file, decision.to_string()).unwrap();
        response(
            workspace_cli()
                .args([
                    "issue",
                    "create",
                    "--area",
                    area,
                    "--type",
                    "task",
                    "--title",
                    title,
                    "--actor",
                    ACTOR,
                    "--request-id",
                    request,
                    "--evidence-file",
                ])
                .arg(&evidence_file)
                .args(target_flags)
                .output()
                .unwrap(),
            0,
        )["data"]
            .clone()
    };
    let first_request = id(80);
    let first = create(&first_request, "Synthetic Issue after import");
    let first_issue = first["result"]["issue_id"].as_str().unwrap();
    let first_revision = first["result"]["issue_revision"].as_str().unwrap();
    assert_eq!(first["result"]["number"], n + 1);

    let cancel_request = id(81);
    let decision = IssueDecision {
        workspace,
        area,
        actor: ACTOR,
        request: &cancel_request,
    }
    .reconcile("cancel", first_issue, first_revision, REASON);
    fs::write(&evidence_file, decision.to_string()).unwrap();
    let closed = response(
        workspace_cli()
            .args([
                "issue",
                "cancel",
                first_issue,
                "--area",
                area,
                "--actor",
                ACTOR,
                "--if-match",
                &format!("{first_issue}={first_revision}"),
                "--reason",
                REASON,
                "--request-id",
                &cancel_request,
                "--evidence-file",
            ])
            .arg(&evidence_file)
            .args(target_flags)
            .output()
            .unwrap(),
        0,
    )["data"]
        .clone();
    assert_eq!(closed["result"]["issue_id"], first_issue);
    assert_eq!(closed["result"]["status"], "cancelled");

    let second_request = id(82);
    let second = create(&second_request, "Synthetic Issue after cancellation");
    let second_issue = second["result"]["issue_id"].as_str().unwrap();
    assert_ne!(second_issue, first_issue);
    assert_eq!(second["result"]["number"], n + 2);

    expected.extend([
        (n + 1, first_issue.to_owned(), "cancelled".to_owned()),
        (n + 2, second_issue.to_owned(), "backlog".to_owned()),
    ]);
    assert_eq!(issue_rows(&mut reader), expected);
    let mut previous = baseline;
    for (kind, request, result, issue) in [
        ("issue.create", &first_request, &first, first_issue),
        ("issue.cancel", &cancel_request, &closed, first_issue),
        ("issue.create", &second_request, &second, second_issue),
    ] {
        assert_eq!(result["outcome"], "confirmed");
        assert_eq!(result["request_id"], request.as_str());
        assert_eq!(result["base_commit"], previous);
        let receipt = stored_receipt(&mut reader, request);
        assert_eq!(receipt.0, kind);
        assert_eq!(receipt.1, ACTOR);
        assert_eq!(receipt.4, previous);
        assert_eq!(
            serde_json::from_str::<Value>(&receipt.7).unwrap()["issue_id"],
            issue
        );
        let confirmed = result["confirmed_commit"].as_str().unwrap();
        assert_ne!(confirmed, previous);
        previous = confirmed;
    }
    drop(reader);
    let stopped = response(running.stop(), 0);
    assert_eq!(stopped["data"]["process_exit"], "pidfd_verified");
    assert!(!state.join("server.json").exists());
    assert!(!state.join("pending-request.json").exists());

    let written = snapshot();
    assert_eq!(written["head"], previous);
    assert_eq!(written["next_number"], n + 3);
    assert_eq!(written["receipts"], 3);
    assert_ne!(written["head"], initial["head"]);
    let repeated = response(
        restage()
            .env("XDG_STATE_HOME", &state_home)
            .output()
            .unwrap(),
        5,
    );
    assert_eq!(repeated["error"]["code"], "ALREADY_EXISTS");
    assert_eq!(snapshot(), written);
    let restore = response(
        runtime_cli()
            .args(["backup", "restore", "--source"])
            .arg(&backup)
            .arg("--target")
            .arg(target)
            .output()
            .unwrap(),
        5,
    );
    assert_eq!(restore["error"]["code"], "ALREADY_EXISTS");
    assert_eq!(snapshot(), written);
    assert_eq!(
        sql(&format!("SELECT (SELECT COUNT(*) FROM issues AS OF '{baseline}') AS issues,
            (SELECT next_value FROM workspace_counters AS OF '{baseline}' WHERE name = 'issue_number') AS next_number,
            (SELECT COUNT(*) FROM operation_receipts AS OF '{baseline}') AS receipts")),
        json!([{"issues": n, "next_number": n + 1, "receipts": 0}])
    );
    assert_eq!(
        fs::read(backup.join("manifest.json")).unwrap(),
        backup_manifest
    );
    assert_eq!(
        fs::read(state.join("active-root.json")).unwrap(),
        registration
    );
}

fn response(output: Output, status: i32) -> Value {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

struct RunningFixture(Option<Command>);

impl RunningFixture {
    fn stop(mut self) -> Output {
        let output = self.0.as_mut().unwrap().output().unwrap();
        if output.status.success() {
            self.0 = None;
        }
        output
    }
}

impl Drop for RunningFixture {
    fn drop(&mut self) {
        if let Some(command) = self.0.as_mut() {
            let _ = command.output();
        }
    }
}

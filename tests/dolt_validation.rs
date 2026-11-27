#[path = "dolt_validation/artifacts.rs"]
mod artifacts;
#[path = "dolt_validation/bots.rs"]
mod bots;
#[path = "dolt_validation/directives.rs"]
mod directives;
#[path = "dolt_validation/issue_updates.rs"]
mod issue_updates;
#[path = "dolt_validation/receipts.rs"]
mod receipts;
#[path = "dolt_validation/relations.rs"]
mod relations;

use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mysql::prelude::Queryable;
use mysql::{Conn, OptsBuilder};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SCHEMA: &str = include_str!("../assets/schema/dolt/0001_initial.sql");
const SCHEMA_MANIFEST: &str = include_str!("../assets/schema/dolt/0001_schema.sha256");
const CASE_INSENSITIVE: &str = "utf8mb4_0900_ai_ci";
const DATABASE: &str = "fixture_db";
const READER: &str = "fixture_reader";
const ADMIN: &str = "fixture_admin";
const HOST: &str = "127.0.0.1";

fn id(number: u32) -> String {
    format!("01900000-0000-7000-8000-{number:012x}")
}

struct Fixture {
    root: PathBuf,
    dolt: PathBuf,
    password: String,
    server: Option<Child>,
    port: u16,
}

impl Fixture {
    fn start() -> Self {
        Self::start_with_collation(CASE_INSENSITIVE)
    }

    fn start_with_collation(collation: &str) -> Self {
        let dolt = PathBuf::from(
            std::env::var_os("HIVE_DOLT_BIN")
                .expect("point HIVE_DOLT_BIN at the pinned Dolt binary"),
        );
        assert!(dolt.is_absolute(), "HIVE_DOLT_BIN must be an absolute path");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hive-dolt-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let mut fixture = Self {
            root,
            dolt,
            password: format!("fixture-password-{nonce:x}"),
            server: None,
            port: 0,
        };
        fs::create_dir(fixture.root.join("home")).unwrap();
        fs::create_dir(fixture.root.join(DATABASE)).unwrap();
        assert_eq!(
            fixture.local(&["version"], None).trim(),
            "dolt version 2.7.6"
        );
        fixture.local(
            &[
                "init",
                "--initial-branch",
                "main",
                "--name",
                "Fixture",
                "--email",
                "fixture@example.invalid",
            ],
            None,
        );
        fixture.local(
            &["config", "--local", "--add", "user.name", "Fixture"],
            None,
        );
        fixture.local(
            &[
                "config",
                "--local",
                "--add",
                "user.email",
                "fixture@example.invalid",
            ],
            None,
        );
        fixture.local(&["sql"], Some(&seed_sql(collation)));
        fixture.local(&["add", "."], None);
        fixture.local(&["commit", "-m", "Synthetic baseline"], None);
        let password = &fixture.password;
        fixture.local(
            &["sql"],
            Some(&format!(
                "CREATE USER '{READER}'@'{HOST}' IDENTIFIED BY '{password}';
                 CREATE USER '{ADMIN}'@'{HOST}' IDENTIFIED BY '{password}';
                 GRANT SELECT ON {DATABASE}.* TO '{READER}'@'{HOST}';
                 GRANT ALL ON {DATABASE}.* TO '{ADMIN}'@'{HOST}';
                 DROP USER 'root'@'localhost';"
            )),
        );

        let available = TcpListener::bind((HOST, 0)).unwrap();
        fixture.port = available.local_addr().unwrap().port();
        let config = fixture.root.join("server.yml");
        fs::write(
            &config,
            format!(
                "log_level: warning\nmax_logged_query_len: -1\nlistener:\n  host: {HOST}\n  port: {}\n",
                fixture.port
            ),
        )
        .unwrap();
        drop(available);
        fixture.server = Some(
            fixture
                .dolt_command()
                .args(["sql-server", "--config"])
                .arg(config)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let server = fixture.server.as_mut().unwrap();
            assert!(
                server.try_wait().unwrap().is_none(),
                "fixture server exited"
            );
            if TcpStream::connect((HOST, fixture.port)).is_ok() {
                return fixture;
            }
            assert!(Instant::now() < deadline, "fixture server did not start");
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn dolt_command(&self) -> Command {
        let home = self.root.join("home");
        let mut command = Command::new(&self.dolt);
        command
            .current_dir(self.root.join(DATABASE))
            .env_clear()
            .env("HOME", &home)
            .env("DOLT_ROOT_PATH", &home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true");
        command
    }

    fn local(&self, args: &[&str], input: Option<&str>) -> String {
        let mut child = self
            .dolt_command()
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).replace(&self.password, "<redacted>");
        assert!(output.status.success(), "Dolt {} failed: {stderr}", args[0]);
        String::from_utf8(output.stdout).unwrap()
    }

    fn connect(&self, user: &str) -> Conn {
        let timeout = Some(Duration::from_secs(5));
        Conn::new(
            OptsBuilder::new()
                .ip_or_hostname(Some(HOST))
                .tcp_port(self.port)
                .prefer_socket(false)
                .tcp_connect_timeout(timeout)
                .read_timeout(timeout)
                .write_timeout(timeout)
                .db_name(Some(DATABASE))
                .user(Some(user))
                .pass(Some(&self.password)),
        )
        .unwrap()
    }

    fn validate(&self, workspace_id: &str, password: &str, port: u16) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .args([
                "--contract",
                "2",
                "--json",
                "validate",
                "--all",
                "--workspace-id",
                workspace_id,
                "--database",
                DATABASE,
                "--port",
                &port.to_string(),
            ])
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("HIVE_DOLT_USER", READER)
            .env("HIVE_DOLT_PASSWORD", password)
            .output()
            .unwrap()
    }

    fn check(&self, exit: i32, writer: &mut Conn) -> Value {
        let before = storage_state(writer);
        let report = check_output(&self.validate(&id(1), &self.password, self.port), exit);
        assert_eq!(
            storage_state(writer),
            before,
            "validation altered the Dolt state"
        );
        if exit == 0 || report["error"]["code"] == "CONSTRAINT_VIOLATION" {
            assert_eq!(report["context"]["workspace_id"], id(1));
            assert_eq!(report["context"]["db_commit"], before.0);
        }
        report
    }
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

fn seed_sql(collation: &str) -> String {
    let schema_digest = format!("{:x}", Sha256::digest(SCHEMA.as_bytes()));
    let workspace = id(1);
    let area = id(2);
    let subarea = id(3);
    let revision = id(4);
    let parent = id(10);
    let child = id(11);
    let prerequisite = id(12);
    let dependent = id(13);
    let claim = id(14);
    let bot = id(15);
    format!(
        "ALTER DATABASE {DATABASE} COLLATE {collation};
         {SCHEMA}
         INSERT INTO workspace_meta VALUES (1, '{workspace}');
         INSERT INTO schema_migrations VALUES (1, '{schema_digest}', UTC_TIMESTAMP(6));
         INSERT INTO workspace_counters VALUES ('issue_number', 5);
         INSERT INTO areas (id, slug, name, revision) VALUES ('{area}', 'root', 'Root', '{revision}');
         INSERT INTO areas (id, parent_id, slug, name, revision)
           VALUES ('{subarea}', '{area}', 'child', 'Child', '{revision}');
         INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at) VALUES
           ('{parent}', 1, '{area}', 'Parent', 'epic', 'todo', '{revision}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
           ('{prerequisite}', 3, '{area}', 'Prerequisite', 'task', 'todo', '{revision}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6)),
           ('{dependent}', 4, '{subarea}', 'Dependent', 'task', 'todo', '{revision}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6));
         INSERT INTO issues (id, number, area_id, parent_id, title, type, status, revision, created_at, updated_at) VALUES
           ('{child}', 2, '{subarea}', '{parent}', 'Child', 'task', 'in-progress', '{revision}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6));
         UPDATE issues SET status='done', closed_at=UTC_TIMESTAMP(6), closed_by_ref='fixture-reviewer', resolution_note='Accepted'
           WHERE id='{prerequisite}';
         INSERT INTO issue_dependencies VALUES ('{child}', '{prerequisite}'), ('{dependent}', '{prerequisite}');
         INSERT INTO issue_claims VALUES ('{child}', '{claim}', 'fixture-holder', UTC_TIMESTAMP(6));
         INSERT INTO bots (id, slug, display_name, revision) VALUES ('{bot}', 'fixture', 'Fixture', '{revision}');"
    )
}

fn storage_state(connection: &mut Conn) -> (String, String, String, Vec<(String, bool, String)>) {
    let (commit, working, staged): (String, String, String) = connection
        .query_first(
            "SELECT DOLT_HASHOF('HEAD'), DOLT_HASHOF_DB('WORKING'), DOLT_HASHOF_DB('STAGED')",
        )
        .unwrap()
        .unwrap();
    let status = connection.query("SELECT table_name, staged, status FROM dolt_status ORDER BY table_name, staged, status").unwrap();
    (commit, working, staged, status)
}

fn check_output(output: &Output, exit: i32) -> Value {
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(exit), "{report}");
    assert!(output.stderr.is_empty(), "{report}");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], "validate");
    assert_eq!(report["ok"], exit == 0);
    report
}

fn commit(connection: &mut Conn) {
    connection
        .query_drop("CALL DOLT_COMMIT('-am', 'Synthetic validation case')")
        .unwrap();
}

fn diagnostic_codes(report: &Value) -> Vec<&str> {
    assert_eq!(report["error"]["code"], "CONSTRAINT_VIOLATION");
    report["error"]["details"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().unwrap())
        .collect()
}

fn assert_only_diagnostic(report: &Value, code: &str, table: &str, ids: &[&str]) {
    assert_diagnostics(report, &[(code, table, ids)]);
}

fn assert_diagnostics(report: &Value, expected: &[(&str, &str, &[&str])]) {
    assert_eq!(report["error"]["code"], "CONSTRAINT_VIOLATION");
    let diagnostics = report["error"]["details"]["diagnostics"]
        .as_array()
        .unwrap();
    assert_eq!(diagnostics.len(), expected.len(), "{report}");
    for (diagnostic, (code, table, ids)) in diagnostics.iter().zip(expected) {
        assert_eq!(diagnostic["code"], *code, "{diagnostic}");
        assert_eq!(diagnostic["table"], *table, "{diagnostic}");
        let expected_ids = (!ids.is_empty()).then(|| serde_json::json!(ids));
        assert_eq!(
            diagnostic.get("record_ids"),
            expected_ids.as_ref(),
            "{diagnostic}"
        );
    }
}

fn set_issue_status(connection: &mut Conn, issue: &str, status: &str) {
    let terminal = matches!(status, "done" | "cancelled");
    connection
        .exec_drop(
            "UPDATE issues SET status=?, closed_at=IF(?, UTC_TIMESTAMP(6), NULL),
             closed_by_ref=?, resolution_note=? WHERE id=?",
            (
                status,
                terminal,
                terminal.then_some("fixture-reviewer"),
                terminal.then_some("Fixture outcome"),
                issue,
            ),
        )
        .unwrap();
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rejects_active_claims_in_archived_area_ancestry() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let owner = id(5);
    let root = id(2);
    let issue = id(11);
    admin
        .exec_drop(
            "INSERT INTO areas (id, parent_id, slug, name, revision, archived_at) VALUES
             (?, ?, 'owner', 'Owner', ?, NULL),
             (?, ?, 'sibling', 'Archived sibling', ?, UTC_TIMESTAMP(6))",
            (&owner, id(3), id(4), id(6), id(3), id(4)),
        )
        .unwrap();
    admin
        .exec_drop("UPDATE issues SET area_id=? WHERE id=?", (&owner, &issue))
        .unwrap();
    commit(&mut admin);
    let mut previous = fixture.check(0, &mut admin);

    for (archived, previous_exit) in [(&owner, 0), (&root, 4)] {
        admin
            .exec_drop(
                "UPDATE areas SET archived_at=IF(id=?, UTC_TIMESTAMP(6), NULL) WHERE id IN (?, ?)",
                (archived, &owner, &root),
            )
            .unwrap();
        admin.query_drop("CALL DOLT_ADD('areas')").unwrap();
        assert_eq!(fixture.check(previous_exit, &mut admin), previous);
        commit(&mut admin);
        previous = fixture.check(4, &mut admin);
        assert_only_diagnostic(
            &previous,
            "ARCHIVED_AREA_CLAIM",
            "issue_claims",
            &[&issue, &id(14), archived],
        );
    }

    admin
        .exec_drop("DELETE FROM issue_claims WHERE issue_id=?", (&issue,))
        .unwrap();
    set_issue_status(&mut admin, &issue, "todo");
    commit(&mut admin);
    fixture.check(0, &mut admin);
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn requires_done_prerequisites_during_execution_review_and_completion() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let issue = id(11);
    let prerequisite = id(12);
    fixture.check(0, &mut admin);

    for prerequisite_status in ["todo", "cancelled"] {
        set_issue_status(&mut admin, &prerequisite, prerequisite_status);
        commit(&mut admin);
        assert_only_diagnostic(
            &fixture.check(4, &mut admin),
            "UNMET_PREREQUISITE",
            "issue_dependencies",
            &[&issue, &prerequisite],
        );
    }

    admin
        .exec_drop("DELETE FROM issue_claims WHERE issue_id=?", (&issue,))
        .unwrap();
    set_issue_status(&mut admin, &issue, "todo");
    admin
        .exec_drop("UPDATE issues SET type='epic' WHERE id=?", (&issue,))
        .unwrap();
    for status in ["review", "done"] {
        set_issue_status(&mut admin, &issue, status);
        commit(&mut admin);
        assert_only_diagnostic(
            &fixture.check(4, &mut admin),
            "UNMET_PREREQUISITE",
            "issue_dependencies",
            &[&issue, &prerequisite],
        );
    }
    set_issue_status(&mut admin, &issue, "cancelled");
    commit(&mut admin);
    fixture.check(0, &mut admin);

    set_issue_status(&mut admin, &prerequisite, "done");
    for status in ["review", "done"] {
        set_issue_status(&mut admin, &issue, status);
        commit(&mut admin);
        fixture.check(0, &mut admin);
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn requires_terminal_children_of_done_issues_and_epics_in_review() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let parent = id(10);
    let child = id(11);
    fixture.check(0, &mut admin);

    set_issue_status(&mut admin, &parent, "review");
    commit(&mut admin);
    assert_only_diagnostic(
        &fixture.check(4, &mut admin),
        "UNRESOLVED_CHILD",
        "issues",
        &[&parent, &child],
    );

    admin
        .exec_drop("UPDATE issues SET type='task' WHERE id=?", (&parent,))
        .unwrap();
    commit(&mut admin);
    fixture.check(0, &mut admin);

    set_issue_status(&mut admin, &parent, "done");
    commit(&mut admin);
    assert_only_diagnostic(
        &fixture.check(4, &mut admin),
        "UNRESOLVED_CHILD",
        "issues",
        &[&parent, &child],
    );

    admin
        .exec_drop("DELETE FROM issue_claims WHERE issue_id=?", (&child,))
        .unwrap();
    for status in ["cancelled", "done"] {
        set_issue_status(&mut admin, &child, status);
        commit(&mut admin);
        fixture.check(0, &mut admin);
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn reproduces_initial_schema_manifest_across_inherited_collations() {
    use std::fmt::Write as _;

    for collation in ["utf8mb4_0900_bin", CASE_INSENSITIVE] {
        let fixture = Fixture::start_with_collation(collation);
        let mut admin = fixture.connect(ADMIN);
        let mut reader = fixture.connect(READER);
        let before = storage_state(&mut admin);
        let head = &before.0;
        let mut tables: Vec<(String, String)> = reader
            .query(format!("SHOW FULL TABLES AS OF '{head}'"))
            .unwrap();
        tables.sort_unstable();
        let tail = format!(" COLLATE={collation}");
        let mut generated = String::new();
        for (table, kind) in tables {
            assert_eq!(kind, "BASE TABLE");
            let (name, sql): (String, String) = reader
                .query_first(format!("SHOW CREATE TABLE {table} AS OF '{head}'"))
                .unwrap()
                .unwrap();
            assert_eq!(name, table);
            let shape = sql.strip_suffix(&tail).expect("fixture table collation");
            let digest = Sha256::digest(shape.as_bytes());
            writeln!(generated, "{digest:x}  {table}").unwrap();
        }
        println!("Initial schema manifest with {collation}\n{generated}");
        assert_eq!(generated, SCHEMA_MANIFEST);
        assert_eq!(storage_state(&mut admin), before);
        fixture.check(0, &mut admin);
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rejects_only_committed_schema_drift_with_a_matching_ledger() {
    // Constraint names are generated by Dolt 2.7.6 from the initial migration.
    let changes: &[(&str, &[&str])] = &[
        (
            "areas",
            &["ALTER TABLE areas ADD COLUMN fixture_column TEXT"],
        ),
        ("labels", &["ALTER TABLE labels DROP INDEX name"]),
        (
            "directives",
            &["ALTER TABLE directives DROP FOREIGN KEY directives_ibfk_1"],
        ),
        (
            "issues",
            &["ALTER TABLE issues DROP CHECK issues_chk_klp6nseh"],
        ),
        (
            "issues",
            &[
                "ALTER TABLE issues DROP CHECK issues_chk_g3sgfmmr",
                "ALTER TABLE issues ADD CONSTRAINT issues_chk_g3sgfmmr CHECK (priority IS NULL OR priority BETWEEN 0 AND 4)",
            ],
        ),
        (
            "areas",
            &[
                "ALTER TABLE areas MODIFY COLUMN slug VARCHAR(100) COLLATE utf8mb4_0900_bin NOT NULL",
            ],
        ),
    ];
    for (table, statements) in changes {
        let fixture = Fixture::start();
        let mut admin = fixture.connect(ADMIN);
        let baseline = fixture.check(0, &mut admin);
        for statement in *statements {
            admin.query_drop(statement).unwrap();
        }
        let working = storage_state(&mut admin);
        assert_ne!(working.1, working.2);
        assert_eq!(fixture.check(0, &mut admin), baseline);
        admin.exec_drop("CALL DOLT_ADD(?)", (table,)).unwrap();
        let staged = storage_state(&mut admin);
        assert_eq!(staged.1, staged.2);
        assert_eq!(fixture.check(0, &mut admin), baseline);
        commit(&mut admin);
        let report = fixture.check(4, &mut admin);
        assert_eq!(
            report["error"]["code"], "SCHEMA_INCOMPATIBLE",
            "{statements:?}"
        );
        assert_eq!(
            report["error"]["message"],
            format!("The committed definition of {table} differs from storage schema version 1."),
        );
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn validates_committed_issue_number_counter() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let baseline = fixture.check(0, &mut admin);
    admin
        .query_drop("UPDATE workspace_counters SET next_value=4 WHERE name='issue_number'")
        .unwrap();
    assert_eq!(fixture.check(0, &mut admin), baseline);
    admin
        .query_drop("CALL DOLT_ADD('workspace_counters')")
        .unwrap();
    assert_eq!(fixture.check(0, &mut admin), baseline);
    commit(&mut admin);
    assert_only_diagnostic(
        &fixture.check(4, &mut admin),
        "INVALID_ISSUE_COUNTER",
        "workspace_counters",
        &[],
    );

    for statement in [
        "UPDATE workspace_counters SET next_value=3 WHERE name='issue_number'",
        "UPDATE workspace_counters SET next_value=0 WHERE name='issue_number'",
        "DELETE FROM workspace_counters WHERE name='issue_number'",
        "INSERT INTO workspace_counters VALUES ('ISSUE_NUMBER', 5)",
    ] {
        admin.query_drop(statement).unwrap();
        commit(&mut admin);
        assert_only_diagnostic(
            &fixture.check(4, &mut admin),
            "INVALID_ISSUE_COUNTER",
            "workspace_counters",
            &[],
        );
    }
    admin
        .query_drop("DELETE FROM workspace_counters WHERE name='ISSUE_NUMBER'")
        .unwrap();
    admin
        .query_drop("INSERT INTO workspace_counters VALUES ('issue_number', 12)")
        .unwrap();
    commit(&mut admin);
    fixture.check(0, &mut admin);

    admin
        .exec_drop(
            "UPDATE workspace_counters SET next_value=? WHERE name='issue_number'",
            (u64::MAX,),
        )
        .unwrap();
    for (key, number, exit) in [(20, u64::MAX - 1, 0), (21, u64::MAX, 4)] {
        admin.exec_drop(
            "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
             VALUES (?, ?, ?, 'Fixture', 'task', 'todo', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            (id(key), number, id(2), id(4)),
        ).unwrap();
        commit(&mut admin);
        let report = fixture.check(exit, &mut admin);
        if exit == 4 {
            assert_only_diagnostic(&report, "INVALID_ISSUE_COUNTER", "workspace_counters", &[]);
        }
    }

    for statement in [
        "DELETE FROM issue_dependencies",
        "DELETE FROM issue_claims",
        "DELETE FROM issues WHERE parent_id IS NOT NULL",
        "DELETE FROM issues",
        "UPDATE workspace_counters SET next_value=1 WHERE name='issue_number'",
    ] {
        admin.query_drop(statement).unwrap();
    }
    commit(&mut admin);
    fixture.check(0, &mut admin);
    admin
        .query_drop("UPDATE workspace_counters SET next_value=0 WHERE name='issue_number'")
        .unwrap();
    commit(&mut admin);
    assert_only_diagnostic(
        &fixture.check(4, &mut admin),
        "INVALID_ISSUE_COUNTER",
        "workspace_counters",
        &[],
    );
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rejects_noncanonical_issue_values_with_case_insensitive_collation() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let collation: (String, String) = admin
        .query_first("SELECT COLLATION(status), COLLATION(type) FROM issues LIMIT 1")
        .unwrap()
        .unwrap();
    assert_eq!(collation.0, CASE_INSENSITIVE);
    assert_eq!(collation.1, CASE_INSENSITIVE);
    let issue = id(13);

    for (status, kind, diagnostic) in [
        ("in-progress", "task", "INVALID_CLAIM_STATE"),
        ("todo", "TASK", "INVALID_ISSUE_TYPE"),
        ("IN-PROGRESS", "task", "INVALID_ISSUE_STATUS"),
    ] {
        admin
            .exec_drop(
                "UPDATE issues SET status=?, type=? WHERE id=?",
                (status, kind, &issue),
            )
            .unwrap();
        commit(&mut admin);
        let saved: (String, String) = admin
            .exec_first("SELECT status, type FROM issues WHERE id=?", (&issue,))
            .unwrap()
            .unwrap();
        assert_eq!((saved.0.as_str(), saved.1.as_str()), (status, kind));
        let report = fixture.check(4, &mut admin);
        assert_eq!(diagnostic_codes(&report), [diagnostic]);
        assert_eq!(
            report["error"]["details"]["diagnostics"][0]["record_ids"],
            serde_json::json!([&issue])
        );
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn validates_committed_dolt_records_without_writes() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    let identity: String = admin
        .query_first("SELECT workspace_id FROM workspace_meta")
        .unwrap()
        .unwrap();
    assert_eq!(identity, id(1));
    let mut reader = fixture.connect(READER);
    let denied = reader.query_drop("UPDATE workspace_meta SET workspace_id=workspace_id");
    assert!(
        denied.is_err(),
        "the validator's fixture account must be read-only"
    );
    drop(reader);

    assert_eq!(
        fixture.check(0, &mut admin)["data"]["storage_schema_version"],
        1
    );
    let mismatch = fixture.validate(&id(100), &fixture.password, fixture.port);
    assert_eq!(
        check_output(&mismatch, 7)["error"]["code"],
        "AUTHORITY_UNVERIFIED"
    );
    let wrong = fixture.validate(&id(1), "incorrect-fixture-password", fixture.port);
    assert_eq!(check_output(&wrong, 6)["error"]["code"], "UNAUTHORIZED");
    assert!(!String::from_utf8_lossy(&wrong.stdout).contains("incorrect-fixture-password"));
    let unavailable = TcpListener::bind((HOST, 0)).unwrap();
    let offline = fixture.validate(
        &id(1),
        &fixture.password,
        unavailable.local_addr().unwrap().port(),
    );
    // Close the non-Dolt listener after testing a failed handshake.
    assert_eq!(
        check_output(&offline, 7)["error"]["code"],
        "RUNTIME_UNAVAILABLE"
    );

    let baseline = storage_state(&mut admin);
    for sql in [
        format!("UPDATE issues SET area_id='{}' WHERE id='{}'", id(99), id(10)),
        format!("UPDATE issues SET status='done' WHERE id='{}'", id(10)),
        format!("UPDATE issues SET number=2 WHERE id='{}'", id(10)),
        format!("INSERT INTO bot_memories
                  (id, bot_id, kind, recall_tier, topic_key, title, body, source_ref, author_ref,
                   authorization_mode, authorization_ref, revision, created_at, updated_at)
                 VALUES ('{}', '{}', 'durable', NULL, 'fixture', 'Fixture', 'Fixture', 'fixture-source', 'fixture-author',
                         'delegated', 'fixture-delegation', '{}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
            id(16), id(15), id(4)),
    ] {
        assert!(admin.query_drop(&sql).is_err(), "schema accepted a synthetic constraint violation");
    }
    assert_eq!(storage_state(&mut admin), baseline);

    admin
        .exec_drop("UPDATE issues SET parent_id=? WHERE id=?", (id(10), id(13)))
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issue_dependencies VALUES (?, ?)",
            (id(13), id(10)),
        )
        .unwrap();
    // The mixed cycle first exists only in the working set.
    assert_eq!(
        fixture.check(0, &mut admin)["context"]["db_commit"],
        baseline.0
    );
    commit(&mut admin);
    let report = fixture.check(4, &mut admin);
    assert_eq!(diagnostic_codes(&report), ["COMPLETION_CYCLE"]);
    assert_eq!(
        report["error"]["details"]["diagnostics"][0]["record_ids"],
        serde_json::json!([id(10), id(13)])
    );
    assert_eq!(fixture.check(4, &mut admin), report);

    for sql in [
        format!(
            "UPDATE areas SET parent_id='{}' WHERE id='{}'",
            id(3),
            id(2)
        ),
        format!(
            "UPDATE issues SET parent_id='{}' WHERE id='{}'",
            id(11),
            id(10)
        ),
        format!(
            "INSERT INTO issue_dependencies VALUES ('{}','{}')",
            id(10),
            id(13)
        ),
        format!("DELETE FROM issue_claims WHERE issue_id='{}'", id(11)),
        format!(
            "INSERT INTO areas (id, slug, name, revision) VALUES
             ('{}', 'other-root', 'Root', '{}'),
             ('{}', 'other-root', 'Duplicate', '{}')",
            id(20),
            id(4),
            id(21),
            id(4)
        ),
        format!(
            "INSERT INTO areas (id, slug, name, revision) VALUES
             ('invalid-id-fixture', 'invalid-id', 'Malformed', '{}')",
            id(4)
        ),
    ] {
        admin.query_drop(sql).unwrap();
    }
    commit(&mut admin);
    let report = fixture.check(4, &mut admin);
    assert_eq!(
        diagnostic_codes(&report),
        [
            "DUPLICATE_AREA_SLUG",
            "INVALID_ID",
            "AREA_CYCLE",
            "INVALID_CLAIM_STATE",
            "ISSUE_PARENT_CYCLE",
            "PREREQUISITE_CYCLE"
        ]
    );
    assert!(!report.to_string().contains("invalid-id-fixture"));

    // Deliberately corrupt only this disposable database to exercise the read-only reference check.
    admin
        .query_drop("SET SESSION foreign_key_checks=0")
        .unwrap();
    admin
        .query_drop(format!(
            "UPDATE issues SET area_id='{}' WHERE id='{}'",
            id(99),
            id(10)
        ))
        .unwrap();
    admin
        .query_drop("SET SESSION dolt_force_transaction_commit=1")
        .unwrap();
    commit(&mut admin);
    assert!(diagnostic_codes(&fixture.check(4, &mut admin)).contains(&"MISSING_REFERENCE"));

    admin
        .query_drop("UPDATE schema_migrations SET version=2")
        .unwrap();
    commit(&mut admin);
    assert_eq!(
        fixture.check(4, &mut admin)["error"]["code"],
        "SCHEMA_INCOMPATIBLE"
    );
    admin
        .query_drop("UPDATE schema_migrations SET version=1, migration_digest=REPEAT('0', 64)")
        .unwrap();
    commit(&mut admin);
    assert_eq!(
        fixture.check(4, &mut admin)["error"]["code"],
        "SCHEMA_INCOMPATIBLE"
    );

    let digest = format!("{:x}", Sha256::digest(SCHEMA.as_bytes()));
    admin
        .query_drop(format!(
            "UPDATE schema_migrations SET migration_digest='{digest}'"
        ))
        .unwrap();
    admin.query_drop("DROP TABLE workspace_counters").unwrap();
    commit(&mut admin);
    let report = fixture.check(4, &mut admin);
    assert_eq!(report["error"]["code"], "SCHEMA_INCOMPATIBLE");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("workspace_counters")
    );
}

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
        fixture.local(&["sql"], Some(&seed_sql()));
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

fn seed_sql() -> String {
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
        "{SCHEMA}
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
        .query_drop(format!(
            "INSERT INTO issue_dependencies VALUES ('{}','{}')",
            id(11),
            id(10)
        ))
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
        serde_json::json!([id(10), id(11)])
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
            id(12),
            id(11)
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

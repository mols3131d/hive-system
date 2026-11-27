use super::*;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;
#[path = "runtime/area_create.rs"]
mod area_create;
#[path = "runtime/artifact_create.rs"]
mod artifact_create;
#[path = "runtime/artifact_update.rs"]
mod artifact_update;
#[path = "runtime/bot_register.rs"]
mod bot_register;
#[path = "runtime/issue_dependency.rs"]
mod issue_dependency;
#[path = "runtime/issue_parent.rs"]
mod issue_parent;
#[path = "runtime/issue_reconcile.rs"]
mod issue_reconcile;
#[path = "runtime/issue_review.rs"]
mod issue_review;
#[path = "runtime/issue_transition.rs"]
mod issue_transition;
#[path = "runtime/issue_update.rs"]
mod issue_update;
#[path = "runtime/project_planning.rs"]
mod project_planning;

type Receipt = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn stored_receipt(connection: &mut Conn, request: &str) -> Receipt {
    connection.exec_first(
        "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
           FROM operation_receipts WHERE request_id = ?",
        (request,),
    ).unwrap().unwrap()
}

fn record_intent(state: &Path, intent: &Value) {
    let marker = state.join("pending-request.json");
    fs::write(&marker, intent.to_string()).unwrap();
    fs::set_permissions(&marker, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
}

fn random_uuid() -> String {
    fs::read_to_string("/proc/sys/kernel/random/uuid")
        .unwrap()
        .trim()
        .to_owned()
}

fn random_token() -> String {
    random_uuid().replace('-', "")
}

fn prepare(database: &str) -> (Fixture, String, String) {
    let mut fixture = Fixture::initialize(&empty_seed_sql(CASE_INSENSITIVE));
    let state = fixture.root.join("runtime");
    fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
    let grants = state.join("privileges.db");
    let nonce = random_uuid();
    let suffix = &nonce.replace('-', "")[..24];
    fixture.password = random_uuid();
    let writer_password = random_token();
    let reader = format!("reader_{suffix}");
    let writer = format!("writer_{suffix}");
    let config = format!("--doltcfg-dir={}", state.display());
    let privilege_file = format!("--privilege-file={}", grants.display());

    fixture.local(
        &[&config, &privilege_file, "sql"],
        Some(&format!(
            "CREATE USER '{reader}'@'{HOST}' IDENTIFIED BY '{}';
             CREATE USER '{writer}'@'{HOST}' IDENTIFIED BY '{writer_password}';
             GRANT SELECT ON {database}.* TO '{reader}'@'{HOST}';
             GRANT SELECT ON {database}.* TO '{writer}'@'{HOST}';
             GRANT UPDATE ON {database}.workspace_counters TO '{writer}'@'{HOST}';
             DROP USER 'root'@'localhost';",
            fixture.password
        )),
    );

    assert_eq!(fs::metadata(&grants).unwrap().mode() & 0o777, 0o600);
    let grants_for = |name| {
        let output = fixture.local(
            &[
                &config,
                &privilege_file,
                "sql",
                "-q",
                &format!("SHOW GRANTS FOR '{name}'@'{HOST}'"),
            ],
            None,
        );
        let mut grants: Vec<_> = output
            .lines()
            .filter_map(|line| line.trim_matches('|').trim().split_once(" TO "))
            .map(|(grant, _)| grant.to_owned())
            .collect();
        grants.sort_unstable();
        grants
    };
    let select = format!("GRANT SELECT ON `{database}`.*");
    let usage = "GRANT USAGE ON *.*".to_owned();
    assert_eq!(grants_for(&reader), [select.clone(), usage.clone()]);
    assert_eq!(
        grants_for(&writer),
        [
            select,
            format!("GRANT UPDATE ON `{database}`.`workspace_counters`"),
            usage
        ]
    );
    (fixture, nonce, writer_password)
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN"]
fn workspace_runtime_provisions_grants_offline() {
    prepare(DATABASE);
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP and Linux pidfds"]
fn workspace_runtime_probes_ownership_and_graceful_stop() {
    let available = TcpListener::bind((HOST, 0)).unwrap();
    let (mut fixture, nonce, writer_password) = prepare(DATABASE);
    let nonce = &nonce.replace('-', "")[..24];
    let state = fixture.root.join("runtime");
    let db = fixture.root.join(DATABASE);
    let head_query = "SELECT DOLT_HASHOF('HEAD') AS head";
    let baseline = fixture.local(&["sql", "-r", "csv", "-q", head_query], None);
    let pending = state.join("pending-request.json");
    fs::write(&pending, b"synthetic pending request").unwrap();

    fixture.port = available.local_addr().unwrap().port();
    drop(available);
    let server = spawn_legacy_server(&fixture, &db, &state);
    let pid = server.id();
    fixture.server = Some(server);
    let handle = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(
        handle >= 0,
        "pidfd_open failed: {}",
        io::Error::last_os_error()
    );
    let handle = unsafe { OwnedFd::from_raw_fd(handle as i32) };
    fixture.wait_for_listener();

    let proc = PathBuf::from(format!("/proc/{pid}"));
    let identity = process_identity(&proc);
    let actual = fs::metadata(proc.join("exe")).unwrap();
    let expected = fs::metadata(&fixture.dolt).unwrap();
    assert_eq!(
        (actual.dev(), actual.ino()),
        (expected.dev(), expected.ino())
    );
    assert_eq!(fs::read_link(proc.join("cwd")).unwrap(), state);
    assert_only_loopback_listener(&proc, fixture.port);

    let connect = |user: &str, password: &str| {
        let timeout = Some(Duration::from_secs(3));
        Conn::new(
            OptsBuilder::new()
                .ip_or_hostname(Some(HOST))
                .tcp_port(fixture.port)
                .prefer_socket(false)
                .tcp_connect_timeout(timeout)
                .read_timeout(timeout)
                .write_timeout(timeout)
                .db_name(Some(DATABASE))
                .user(Some(user))
                .pass(Some(password)),
        )
    };
    assert_access_denied(connect("root", ""));
    let reader_name = format!("reader_{nonce}");
    assert_access_denied(connect(&reader_name, "wrong-launch-password"));
    let mut reader = connect(&reader_name, &fixture.password)
        .unwrap_or_else(|_| panic!("fresh reader authentication failed"));
    let mut writer = connect(&format!("writer_{nonce}"), &writer_password)
        .unwrap_or_else(|_| panic!("fresh writer authentication failed"));
    let workspace_id: String = reader
        .query_first("SELECT workspace_id FROM workspace_meta AS OF 'HEAD'")
        .unwrap()
        .unwrap();
    assert_eq!(workspace_id, id(1));
    let (head, branch): (String, String) = reader
        .query_first("SELECT DOLT_HASHOF('HEAD'), ACTIVE_BRANCH()")
        .unwrap()
        .unwrap();
    assert_eq!(branch, "main");
    assert_eq!(baseline, format!("head\n{head}\n"));
    let update = "UPDATE workspace_counters SET next_value=2 WHERE name='issue_number'";
    assert!(reader.query_drop(update).is_err());
    assert!(
        writer
            .query_drop(format!(
                "UPDATE workspace_meta SET workspace_id='{}'",
                id(2)
            ))
            .is_err()
    );
    assert!(
        writer
            .query_drop("CREATE TABLE forbidden (id INT PRIMARY KEY)")
            .is_err()
    );
    assert!(
        writer
            .query_drop("CREATE USER 'forbidden'@'127.0.0.1'")
            .is_err()
    );

    require_sql(
        writer.query_drop("SET SESSION dolt_transaction_commit=1"),
        "enable session Dolt commits",
    );
    require_sql(
        writer.query_drop("SET SESSION dolt_force_transaction_commit=0"),
        "disable force commit",
    );
    require_sql(
        writer.query_drop("START TRANSACTION"),
        "begin writer transaction",
    );
    require_sql(writer.query_drop(update), "apply scoped counter update");
    let writer_commit = writer.query_drop("COMMIT");
    // Leave a transaction open at shutdown and verify the committed count from disk.
    writer.query_drop("START TRANSACTION").unwrap();
    writer
        .query_drop("UPDATE workspace_counters SET next_value=3 WHERE name='issue_number'")
        .unwrap();
    assert_eq!(process_identity(&proc), identity);
    assert_only_loopback_listener(&proc, fixture.port);
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            handle.as_raw_fd(),
            libc::SIGTERM,
            0usize,
            0,
        )
    };
    assert_eq!(
        result,
        0,
        "pidfd_send_signal failed: {}",
        io::Error::last_os_error()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(exit) = fixture.server.as_mut().unwrap().try_wait().unwrap() {
            break exit;
        }
        assert!(
            Instant::now() < deadline,
            "owned Dolt did not exit after SIGTERM"
        );
        thread::sleep(Duration::from_millis(25));
    };
    fixture.server.take();
    assert!(exit.success(), "owned Dolt did not exit gracefully: {exit}");
    assert_eq!(fs::read(&pending).unwrap(), b"synthetic pending request");
    require_sql(writer_commit, "publish transaction as limited writer");
    assert_eq!(
        fixture.local(
            &[
                "sql",
                "-r",
                "csv",
                "-q",
                "SELECT next_value FROM workspace_counters AS OF 'HEAD'"
            ],
            None,
        ),
        "next_value\n2\n",
        "the committed write must appear in the selected on-disk database"
    );
    assert_eq!(
        fixture.local(
            &[
                "sql",
                "-r",
                "csv",
                "-q",
                "SELECT next_value FROM workspace_counters"
            ],
            None
        ),
        "next_value\n2\n"
    );
}

fn require_sql(result: Result<(), mysql::Error>, step: &str) {
    match result {
        Ok(()) => {}
        Err(mysql::Error::MySqlError(error)) => {
            panic!(
                "{step}: MySQL error {} (SQLSTATE {})",
                error.code, error.state
            )
        }
        Err(_) => panic!("{step}: SQL connection or client error"),
    }
}

fn assert_access_denied(connection: Result<Conn, mysql::Error>) {
    assert!(
        matches!(connection, Err(mysql::Error::MySqlError(error)) if error.code == 1045),
        "expected MySQL authentication denial"
    );
}

fn spawn_legacy_server(fixture: &Fixture, db: &Path, run: &Path) -> Child {
    fixture
        .dolt_command()
        .current_dir(run)
        .args([
            "sql-server",
            "--skip-root-user-initialization",
            "--host=127.0.0.1",
            &format!("--port={}", fixture.port),
            &format!("--data-dir={}", db.display()),
            &format!("--doltcfg-dir={}", run.display()),
            &format!("--privilege-file={}", run.join("privileges.db").display()),
            "--event-scheduler=OFF",
            "--loglevel=error",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn process_identity(proc: &Path) -> (String, u64) {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let stat = fs::read_to_string(proc.join("stat")).unwrap();
    let (_, fields) = stat.rsplit_once(") ").unwrap();
    let mut fields = fields.split_ascii_whitespace();
    assert!(!matches!(fields.next().unwrap(), "Z" | "X" | "x"));
    let ticks = fields.nth(18).unwrap().parse().unwrap();
    assert_ne!(ticks, 0);
    (boot, ticks)
}

fn assert_only_loopback_listener(proc: &Path, port: u16) {
    let sockets: Vec<_> = fs::read_dir(proc.join("fd"))
        .unwrap()
        .filter_map(|entry| fs::read_link(entry.unwrap().path()).ok())
        .filter_map(|target| {
            target
                .to_str()?
                .strip_prefix("socket:[")?
                .strip_suffix(']')
                .map(str::to_owned)
        })
        .collect();
    let mut listeners = Vec::new();
    for protocol in ["tcp", "tcp6"] {
        let table = fs::read_to_string(proc.join("net").join(protocol)).unwrap();
        for line in table.lines().skip(1) {
            let fields: Vec<_> = line.split_ascii_whitespace().collect();
            if fields[3] == "0A" && sockets.iter().any(|inode| inode == fields[9]) {
                listeners.push(fields[1].to_owned());
            }
        }
    }
    assert_eq!(listeners, [format!("0100007F:{port:04X}")]);
    let unix = fs::read_to_string(proc.join("net/unix")).unwrap();
    for line in unix.lines().skip(1) {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        assert!(
            fields[3] != "00010000" || sockets.iter().all(|inode| inode != fields[6]),
            "Dolt opened an unexpected Unix listener"
        );
    }
}

fn register_fixture(fixture: &Fixture, workspace: &Path, state: &Path) -> Value {
    fs::DirBuilder::new().mode(0o700).create(workspace).unwrap();
    fs::rename(fixture.root.join(DATABASE), workspace.join("db")).unwrap();
    let workspace_id = id(1);
    fs::write(
        workspace.join("hive-workspace.yml"),
        format!(
            "version: 2\nworkspace_id: {workspace_id}\nschema_version: 1\npolicy_revision: {}\n",
            "a".repeat(40)
        ),
    )
    .unwrap();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)
        .unwrap();
    let registration = state.join("active-root.json");
    let registered_root = serde_json::json!({
        "version": 2,
        "workspace_id": workspace_id,
        "workspace_root": workspace
    });
    fs::write(registration, registered_root.to_string()).unwrap();
    registered_root
}

fn lifecycle_command(fixture: &Fixture, workspace: &Path, state_home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hive"));
    command
        .env_clear()
        .env("HOME", fixture.root.join("home"))
        .env("XDG_STATE_HOME", state_home)
        .current_dir(&fixture.root)
        .args(["--contract", "2", "--json", "--workspace"])
        .arg(workspace)
        .arg("workspace");
    command
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP, Linux pidfds and native Git"]
fn workspace_runtime_cli_starts_and_stops_registered_workspace() {
    let fixture = Fixture::initialize(&empty_seed_sql(CASE_INSENSITIVE));
    let baseline = fixture.local(
        &["sql", "-r", "csv", "-q", "SELECT DOLT_HASHOF('HEAD')"],
        None,
    );
    let baseline = baseline.lines().nth(1).unwrap();
    let available = TcpListener::bind((HOST, 0)).unwrap();
    let port = available.local_addr().unwrap().port().to_string();
    drop(available);

    // The workspace selector rejects control characters. The state root
    // comes from XDG and exercises DEL and NEL in the private config.
    let workspace = fixture.root.join("work $HOME $$ \u{2028}\u{2029}");
    let state_home = fixture
        .root
        .join("state ${HOME} $$ \u{7f}\u{85}\u{2028}\u{2029}");
    let workspace_id = id(1);
    let state = state_home.join("hive/v2/workspaces").join(&workspace_id);
    let registered_root = register_fixture(&fixture, &workspace, &state);
    let registration = state.join("active-root.json");
    let bin = fixture.root.join("bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(&fixture.dolt, bin.join("dolt")).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).unwrap();
    let invoke = |args: &[&str]| {
        lifecycle_command(&fixture, &workspace, &state_home)
            .args(args)
            .output()
            .unwrap()
    };
    let start = [
        "start",
        "--dolt-bin",
        fixture.dolt.to_str().unwrap(),
        "--port",
        &port,
    ];
    let copy = fixture.root.join("inactive-copy");
    fs::create_dir(&copy).unwrap();
    fs::copy(
        workspace.join("hive-workspace.yml"),
        copy.join("hive-workspace.yml"),
    )
    .unwrap();
    let mut inactive = registered_root.clone();
    inactive["workspace_root"] = serde_json::json!(copy);
    fs::write(&registration, inactive.to_string()).unwrap();
    let pending = state.join("pending-request.json");
    let checks = std::panic::catch_unwind(|| {
        let refused = invoke(&start);
        fs::write(&registration, registered_root.to_string()).unwrap();
        let started = invoke(&start);
        let start_report: Value = serde_json::from_slice(&started.stdout).unwrap();
        assert!(started.status.success(), "{start_report}");
        assert!(!workspace.join("db/config.yaml").exists());
        let password_file = Path::new(
            start_report["data"]["reader_password_file"]
                .as_str()
                .unwrap(),
        );
        let run = password_file.parent().unwrap();
        assert_eq!(run.parent(), Some(state.as_path()));
        assert_eq!(fs::metadata(run).unwrap().mode() & 0o777, 0o700);
        for name in [
            "server-config.yaml",
            "config-id",
            "reader-password",
            "writer-password",
            "privileges.db",
        ] {
            assert_eq!(fs::metadata(run.join(name)).unwrap().mode() & 0o777, 0o600);
        }
        let password = fs::read_to_string(password_file).unwrap();
        let config = fs::read_to_string(run.join("server-config.yaml")).unwrap();
        assert!(!config.contains(&password));
        assert!(!config.contains(&fs::read_to_string(run.join("writer-password")).unwrap()));
        let expected = &start_report["data"]["committed_identity"];
        assert_eq!(expected["dolt_commit"], baseline);
        for command in ["status", "doctor"] {
            let output = lifecycle_command(&fixture, &workspace, &state_home)
                .env("PATH", &path)
                .env(
                    "HIVE_DOLT_USER",
                    start_report["data"]["reader_user"].as_str().unwrap(),
                )
                .env("HIVE_DOLT_PASSWORD", &password)
                .args([
                    command,
                    "--workspace-id",
                    &workspace_id,
                    "--database",
                    "db",
                    "--port",
                    &port,
                ])
                .output()
                .unwrap();
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(output.status.success(), "{report}");
            assert_eq!(&report["data"]["committed_identity"], expected);
        }
        assert_access_denied(Conn::new(
            OptsBuilder::new()
                .ip_or_hostname(Some(HOST))
                .tcp_port(port.parse().unwrap())
                .prefer_socket(false)
                .tcp_connect_timeout(Some(Duration::from_secs(3)))
                .read_timeout(Some(Duration::from_secs(3)))
                .user(Some("root")),
        ));
        let running = invoke(&["status"]);
        let repeated = invoke(&start);
        fs::write(&pending, b"synthetic pending").unwrap();
        (refused, start_report, running, repeated)
    });
    // Use the same ownership checks for fixture cleanup, including on panic.
    let stopped = invoke(&["stop"]);
    if !stopped.status.success()
        && (state.join("server.json").exists() || state.join("starting").exists())
    {
        // Preserve the disposable store for inspection if shutdown was uncertain.
        let root = fixture.root.clone();
        std::mem::forget(fixture);
        panic!(
            "Cleanup of {} was uncertain: {}",
            root.display(),
            String::from_utf8_lossy(&stopped.stdout)
        );
    }
    let (refused, start_report, running, repeated) =
        checks.unwrap_or_else(|payload| std::panic::resume_unwind(payload));
    let stop_report: Value = serde_json::from_slice(&stopped.stdout).unwrap();
    assert!(stopped.status.success(), "{stop_report}");

    let refusal: Value = serde_json::from_slice(&refused.stdout).unwrap();
    let status: Value = serde_json::from_slice(&running.stdout).unwrap();
    let repeated: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(refusal["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(start_report["data"]["state"], "running");
    assert_eq!(
        start_report["data"]["committed_identity"]["workspace_id"],
        workspace_id
    );
    assert_eq!(status["data"]["runtime"]["process"], "pid_start_verified");
    assert_eq!(repeated["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(stop_report["data"]["state"], "stopped");
    assert_eq!(stop_report["data"]["pending_request"], "recorded");
    assert_eq!(fs::read(pending).unwrap(), b"synthetic pending");
    assert!(!state.join("server.json").exists());
    assert!(state.join("lock").exists());
    assert!(!workspace.join("db/config.yaml").exists());
    assert!(
        !Path::new(
            start_report["data"]["reader_password_file"]
                .as_str()
                .unwrap()
        )
        .parent()
        .unwrap()
        .exists()
    );
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP and Linux pidfds"]
fn workspace_runtime_cli_stops_legacy_launch_and_preserves_its_config() {
    let (mut fixture, nonce, _) = prepare("db");
    let workspace = fixture.root.join("workspace");
    let state_home = fixture.root.join("state");
    let workspace_id = id(1);
    let state = state_home.join("hive/v2/workspaces").join(&workspace_id);
    register_fixture(&fixture, &workspace, &state);
    // The prior Hive launcher created this stable lock before starting Dolt.
    let lock = state.join("lock");
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    let db = workspace.join("db");
    let artifact = db.join("config.yaml");
    let run = state.join(format!("run-{nonce}"));
    fs::rename(fixture.root.join("runtime"), &run).unwrap();
    let binary = fs::metadata(&fixture.dolt).unwrap();
    for (name, data) in [
        ("binary-id", format!("{}:{}", binary.dev(), binary.ino())),
        ("reader-password", fixture.password.clone()),
    ] {
        let file = run.join(name);
        fs::write(&file, data).unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let available = TcpListener::bind((HOST, 0)).unwrap();
    fixture.port = available.local_addr().unwrap().port();
    drop(available);
    let child = spawn_legacy_server(&fixture, &db, &run);
    let pid = child.id();
    fixture.server = Some(child);
    fixture.wait_for_listener();
    let (boot, start_ticks) = process_identity(&PathBuf::from(format!("/proc/{pid}")));
    let record = serde_json::json!({
        "version": 2,
        "workspace_id": workspace_id,
        "workspace_root": workspace,
        "pid": pid,
        "boot_id": boot.trim(),
        "start_ticks": start_ticks,
        "nonce": nonce,
        "endpoint": { "port": fixture.port, "database": "db" },
    });
    fs::write(state.join("server.json"), record.to_string()).unwrap();
    let invoke = |args: &[&str]| {
        let output = lifecycle_command(&fixture, &workspace, &state_home)
            .env(
                "HIVE_DOLT_USER",
                format!("reader_{}", &nonce.replace('-', "")[..24]),
            )
            .env("HIVE_DOLT_PASSWORD", &fixture.password)
            .args(args)
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(output.status.success(), "{report}");
        report
    };
    let port = fixture.port.to_string();
    let status = invoke(&[
        "status",
        "--workspace-id",
        &workspace_id,
        "--database",
        "db",
        "--port",
        &port,
    ]);
    assert_eq!(status["data"]["runtime"]["process"], "pid_start_verified");
    assert_eq!(
        status["data"]["runtime"]["endpoint"],
        "committed_identity_verified"
    );
    let original = fs::read(&artifact).unwrap();
    let inode = fs::metadata(&artifact).unwrap().ino();
    assert_eq!(invoke(&["stop"])["data"]["process_exit"], "pidfd_verified");
    assert!(!run.exists());
    assert_eq!(fs::read(&artifact).unwrap(), original);

    let available = TcpListener::bind((HOST, 0)).unwrap();
    let restart_port = available.local_addr().unwrap().port().to_string();
    drop(available);
    let started = invoke(&[
        "start",
        "--dolt-bin",
        fixture.dolt.to_str().unwrap(),
        "--port",
        &restart_port,
    ]);
    let running_artifact = fs::read(&artifact);
    invoke(&["stop"]);
    assert_eq!(
        started["data"]["committed_identity"],
        status["data"]["committed_identity"]
    );
    assert_eq!(running_artifact.unwrap(), original);
    assert_eq!(fs::read(&artifact).unwrap(), original);
    assert_eq!(fs::metadata(&artifact).unwrap().ino(), inode);
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP, Linux pidfds and native Git"]
fn context_resolves_owned_snapshot_and_clean_asset_commit() {
    let workspace = id(1);
    let area = id(3);
    let issue = id(11);
    let handoff = id(40);
    let artifact = id(41);
    let project = id(42);
    let directive = id(43);
    let activation = id(44);
    let invocation = id(45);
    let bot = id(15);
    let activated = serde_json::json!({
        "id": directive, "revision": "directive-r1", "scope": {"kind": "project", "id": project}
    });
    let fingerprint = "a".repeat(64);
    let approval = serde_json::json!({
        "version": 1, "workspace_id": workspace, "request_id": activation, "fingerprint": fingerprint,
        "actor_ref": "fixture-actor", "reviewer_ref": "fixture-reviewer",
        "approval_ref": "fixture-approval", "decision": "approved", "directive": activated
    });
    let activation_result = serde_json::json!({"version": 1, "directive": activated});
    let fixture = Fixture::initialize(&format!(
        "{}

         INSERT INTO issue_updates VALUES
             ('{handoff}', '{issue}', 'handoff', 'Synthetic handoff', 'fixture-author', '2026-01-01', NULL);
         INSERT INTO artifacts (id, kind, title, body, author_ref, revision, created_at, substantive_updated_at)
             VALUES ('{artifact}', 'report', 'Fixture report', 'Synthetic artifact', 'fixture-author', 'artifact-r1', '2026-01-01', '2026-01-01');
         INSERT INTO artifact_issues VALUES ('{artifact}', '{issue}');
         INSERT INTO projects (id, name, status, revision) VALUES ('{project}', 'Fixture project', 'active', 'project-r1');
         INSERT INTO project_issues VALUES ('{project}', '{issue}');
         INSERT INTO directives (id, scope_kind, project_id, title, body, status, issuer_ref, source_ref, approval_ref, revision, created_at, updated_at)
             VALUES ('{directive}', 'project', '{project}', 'Fixture directive', 'Synthetic direction', 'proposed',
                     'fixture-issuer', 'fixture-source', NULL, 'draft-r1', '2026-01-01', '2026-01-01');
",
        seed_sql(CASE_INSENSITIVE),
    ));
    let base = fixture.local(
        &[
            "sql",
            "-r",
            "csv",
            "-q",
            "SELECT DOLT_HASHOF('HEAD') AS head",
        ],
        None,
    );
    let base = base.lines().last().unwrap();
    fixture.local(&["sql"], Some(&format!(
        "UPDATE directives SET status = 'active', approval_ref = 'fixture-approval', revision = 'directive-r1'
         WHERE id = '{directive}';
         INSERT INTO operation_receipts
             (request_id, kind, fingerprint, actor_ref, authorization_ref, workflow_evidence_json, base_commit, result_json, created_at)
             VALUES ('{activation}', 'directive.activate', '{fingerprint}', 'fixture-actor', 'fixture-approval',
                     '{approval}', '{base}', '{activation_result}', '2026-01-01');"
    )));
    fixture.local(&["add", "."], None);
    fixture.local(&["commit", "-m", "Synthetic atomic activation"], None);
    let head = || {
        fixture
            .local(
                &["sql", "-r", "csv", "-q", "SELECT DOLT_HASHOF('HEAD')"],
                None,
            )
            .lines()
            .last()
            .unwrap()
            .to_owned()
    };
    let note_base = head();
    let request = id(46);
    let note = id(47);
    let revision = id(48);
    let note_body = "Synthetic receipt note body café\r\n";
    let note_hex: String = note_body
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let body_sha256 = format!("{:x}", Sha256::digest(note_body));
    let fingerprint_input = serde_json::json!([
        1,
        "issue.note",
        workspace,
        area,
        issue,
        "fixture-holder",
        id(14),
        id(4),
        "progress",
        body_sha256
    ]);
    let note_fingerprint = format!("{:x}", Sha256::digest(fingerprint_input.to_string()));
    let note_evidence = serde_json::json!({
        "version": 1, "operation": "issue.note", "workspace_id": workspace,
        "request_id": request, "fingerprint": note_fingerprint, "actor_ref": "fixture-holder",
        "area_id": area, "issue_id": issue, "expected_issue_revision": id(4),
        "claim_id": id(14), "authorization_ref": "fixture-note-auth",
        "evidence_id": "fixture-note-evidence", "revision": "r1",
        "source_ref": "fixture-note-source", "issuer_ref": "fixture-note-issuer",
        "valid_from": "2025-01-01T00:00:00Z", "expires_at": "2027-01-01T00:00:00Z",
        "withdrawn": false,
    });
    let unverified_request = id(50);
    let mut unverified_evidence = note_evidence.clone();
    unverified_evidence["request_id"] = unverified_request.clone().into();
    let note_result = serde_json::json!({
        "version": 1, "area_id": area, "issue_id": issue, "note_id": note,
        "issue_revision": revision,
    });
    let mismatches = [
        (id(51), id(52), "progress", "00"),
        (id(53), id(54), "comment", note_hex.as_str()),
    ];
    let mut mismatched_rows = String::new();
    for (request_id, note_id, kind, body_hex) in &mismatches {
        use std::fmt::Write as _;
        let mut evidence = note_evidence.clone();
        evidence["request_id"] = request_id.clone().into();
        let mut result = note_result.clone();
        result["note_id"] = note_id.clone().into();
        writeln!(
            mismatched_rows,
            "INSERT INTO issue_updates VALUES
                ('{note_id}', '{issue}', '{kind}', CONVERT(UNHEX('{body_hex}') USING utf8mb4), 'fixture-holder', '2026-01-01', NULL);
             INSERT INTO operation_receipts VALUES
                ('{request_id}', 'issue.note', '{note_fingerprint}', 'fixture-holder', 'fixture-note-auth',
                 '{evidence}', '{note_base}', '{result}', '2026-01-01');"
        ).unwrap();
    }
    fixture.local(&["sql"], Some(&format!(
        "INSERT INTO issue_updates VALUES
             ('{note}', '{issue}', 'progress', CONVERT(UNHEX('{note_hex}') USING utf8mb4), 'fixture-holder', '2026-01-01', NULL);
         UPDATE issues SET revision = '{revision}' WHERE id = '{issue}';
         INSERT INTO operation_receipts VALUES
             ('{request}', 'issue.note', '{note_fingerprint}', 'fixture-holder', 'fixture-note-auth',
              '{note_evidence}', '{note_base}', '{note_result}', '2026-01-01'),
             ('{unverified_request}', 'issue.note', '{note_fingerprint}', 'fixture-holder', 'fixture-note-auth',
              '{unverified_evidence}', '{base}', '{note_result}', '2026-01-01');
         {mismatched_rows}"
    )));
    fixture.local(&["add", "."], None);
    fixture.local(&["commit", "-m", "Synthetic atomic Issue note"], None);
    let note_commit = head();
    fixture.local(
        &["sql"],
        Some("UPDATE areas SET name = 'Renamed root' WHERE slug = 'root';"),
    );
    fixture.local(&["add", "."], None);
    fixture.local(&["commit", "-m", "Synthetic later commit"], None);

    let root = fixture.root.join("workspace");
    fs::create_dir(&root).unwrap();
    fs::rename(fixture.root.join(DATABASE), root.join("db")).unwrap();
    let policy = fixture.root.join("policy");
    let assets = root.join("assets");
    fs::create_dir_all(policy.join("docs/architecture")).unwrap();
    fs::create_dir_all(assets.join("bots/fixture")).unwrap();
    fs::write(
        policy.join("docs/architecture/dolt-mvp-contract.md"),
        "# Fixture\n\n### Bot and curated memory\n\n| Queen | Synthetic policy |\n\n#### Continuity boundaries and proposed MVP scope\n",
    ).unwrap();
    let soul = assets.join("bots/fixture/SOUL.md");
    fs::write(&soul, "---\nrole: queen\n---\nSynthetic Soul\n").unwrap();
    fs::write(
        assets.join("bots/fixture/AGENTS.md"),
        "excluded fixture management",
    )
    .unwrap();
    let git = |root: &Path, args: &[&str]| {
        let output = Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", fixture.root.join("home"))
            .current_dir(root)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let commit = |root| {
        git(root, &["init", "--quiet"]);
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "Synthetic inspection fixture"]);
        git(root, &["rev-parse", "HEAD"])
    };
    let policy_pin = commit(&policy);
    let asset_pin = commit(&assets);
    fs::write(root.join("hive-workspace.yml"), format!(
        "version: 2\nworkspace_id: {workspace}\nschema_version: 1\npolicy_revision: {policy_pin}\n"
    )).unwrap();
    let local_state = fixture.root.join("state");
    let state = local_state.join("hive/v2/workspaces").join(&workspace);
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
    fs::write(
        state.join("active-root.json"),
        serde_json::json!({
            "version": 2, "workspace_id": workspace, "workspace_root": root
        })
        .to_string(),
    )
    .unwrap();
    let listener = TcpListener::bind((HOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    let fixture_root = fixture.root.clone();
    let dolt = fixture.dolt.clone();
    let invoke_as = |binary: &std::ffi::OsStr, args: &[&str]| {
        Command::new(binary)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", fixture_root.join("home"))
            .env("XDG_STATE_HOME", &local_state)
            .current_dir(&fixture_root)
            .args([
                "--contract",
                "2",
                "--json",
                "--workspace",
                root.to_str().unwrap(),
            ])
            .args(args)
            .output()
    };
    let invoke = |args: &[&str]| invoke_as(std::ffi::OsStr::new(env!("CARGO_BIN_EXE_hive")), args);
    let run = |args: &[&str], exit| {
        let output = invoke(args).unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{report}");
        report
    };

    struct StopOnDrop<F: Fn() -> bool> {
        fixture: Option<Fixture>,
        stop: F,
    }
    impl<F: Fn() -> bool> StopOnDrop<F> {
        fn finish(&mut self) -> bool {
            let fixture = self.fixture.take().expect("armed runtime fixture");
            if (self.stop)() {
                drop(fixture);
                true
            } else {
                eprintln!(
                    "Verified stop failed. Preserving disposable runtime store at {}",
                    fixture.root.display()
                );
                // Fixture::drop would remove the store even though the server may be live.
                std::mem::forget(fixture);
                false
            }
        }
    }
    impl<F: Fn() -> bool> Drop for StopOnDrop<F> {
        fn drop(&mut self) {
            if self.fixture.is_some() {
                self.finish();
            }
        }
    }
    let mut cleanup = StopOnDrop {
        fixture: Some(fixture),
        stop: || {
            invoke(&["workspace", "stop"]).ok().is_some_and(|output| {
                output.status.success()
                    && serde_json::from_slice::<Value>(&output.stdout).is_ok_and(|report| {
                        report["ok"] == true && report["data"]["process_exit"] == "pidfd_verified"
                    })
            })
        },
    };
    let started = run(
        &[
            "workspace",
            "start",
            "--dolt-bin",
            dolt.to_str().unwrap(),
            "--port",
            &port,
        ],
        0,
    );
    let inspect = [
        "context",
        "resolve",
        "--database",
        "db",
        "--port",
        &port,
        "--workspace-id",
        &workspace,
        "--policy-root",
        policy.to_str().unwrap(),
        "--asset-commit",
        &asset_pin,
        "--actor",
        "fixture-actor",
        "--bot",
        &bot,
        "--issue",
        "#4",
    ];
    let before = run(&inspect, 0);
    assert_eq!(before["context"]["asset_commit"], asset_pin);
    assert_eq!(
        before["context"]["db_commit"],
        started["data"]["committed_identity"]["dolt_commit"]
    );
    assert_eq!(before["context"]["policy_revision"], policy_pin);
    assert_eq!(
        before["data"]["sections"][0]["content"]["soul"]["body"],
        "Synthetic Soul\n"
    );
    assert!(!before.to_string().contains("excluded fixture management"));
    assert!(!before.to_string().contains("reader-password"));
    assert!(!before.to_string().contains("Synthetic handoff"));
    assert!(!before.to_string().contains("Synthetic artifact"));
    assert!(!before.to_string().contains("Synthetic direction"));

    let commit = before["context"]["db_commit"].as_str().unwrap();
    let mut work = inspect;
    *work.last_mut().unwrap() = "#2";
    let evidence_path = fixture_root.join("context-evidence.json");
    let rich = [
        work.as_slice(),
        &[
            "--project",
            &project,
            "--evidence-file",
            evidence_path.to_str().unwrap(),
            "--invocation-id",
            &invocation,
        ],
    ]
    .concat();
    let mut evidence = serde_json::json!({
        "version": 1,
        "directive_receipts": [{"directive_id": directive, "request_id": activation}],
    });
    fs::write(&evidence_path, evidence.to_string()).unwrap();
    let proof_missing = run(&rich, 7);
    assert_eq!(proof_missing["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert!(proof_missing.get("data").is_none());

    let declaration = |kind, operation, record, revision| {
        serde_json::json!({
            "version": 1, "evidence_id": "fixture-current-secret-marker", "revision": "grant-r1",
            "source_ref": "fixture-current-source", "issuer_ref": "fixture-current-issuer",
            "workspace_id": workspace, "actor_ref": "fixture-actor", "bot_id": bot, "area_id": area,
            "issue_id": issue, "operations": [operation], "record": {"kind": kind, "id": record, "revision": revision},
            "invocation_id": invocation, "db_commit": commit, "valid_from": "2000-01-01T00:00:00Z",
            "expires_at": "2099-01-01T00:00:00Z", "withdrawn": false,
        })
    };
    evidence["handoff"] = declaration(
        "issue_update",
        "read_handoff_body",
        handoff.as_str(),
        commit,
    );
    evidence["artifacts"] = serde_json::json!([declaration(
        "artifact",
        "disclose_artifact_body",
        artifact.as_str(),
        "artifact-r1"
    )]);
    fs::write(&evidence_path, evidence.to_string()).unwrap();
    let unauthorized = run(&rich, 6);
    assert_eq!(unauthorized["error"]["code"], "UNAUTHORIZED");
    let selected = [rich.as_slice(), &["--include-artifact", artifact.as_str()]].concat();
    let resolved = run(&selected, 0);
    assert_eq!(resolved["context"]["db_commit"], commit);
    assert!(
        resolved["context"]["observed_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    let sections = resolved["data"]["sections"].as_array().unwrap();
    assert_eq!(sections[1]["content"][0]["body"], "Synthetic direction");
    assert_eq!(sections[4]["content"]["body"], "Synthetic handoff");
    assert_eq!(sections[5]["content"][0]["body"], "Synthetic artifact");
    for private in [
        "fixture-current-secret-marker",
        "fixture-current-source",
        "fixture-current-issuer",
        "withdrawn",
        "invocation_id",
        "reader-password",
        "pid_start_verified",
    ] {
        assert!(!resolved.to_string().contains(private), "{private}");
    }
    evidence["handoff"]["db_commit"] = "v".repeat(32).into();
    fs::write(&evidence_path, evidence.to_string()).unwrap();
    let stale = run(&selected, 6);
    assert_eq!(stale["error"]["code"], "UNAUTHORIZED");
    assert!(stale.get("data").is_none());

    let lock = fs::File::open(state.join("lock")).unwrap();
    // SAFETY: the open descriptor identifies the existing workspace lock.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let busy = run(&inspect, 5);
    assert_eq!(busy["error"]["code"], "LOCK_BUSY");
    drop(lock);

    fs::write(soul, "dirty asset").unwrap();
    let dirty = run(&inspect, 5);
    assert_eq!(dirty["error"]["code"], "DIRTY_CONFLICT");
    assert!(dirty.get("data").is_none());
    let show_path = fixture_root.join("operation-evidence.json");
    let operation = |request_id: &str,
                     area_id: &str,
                     issue_id: &str,
                     actor: &str,
                     evidence: Option<&Value>,
                     exit| {
        let mut args = vec![
            "operation",
            "show",
            request_id,
            "--database",
            "db",
            "--port",
            &port,
            "--workspace-id",
            &workspace,
            "--area",
            area_id,
            "--issue",
            issue_id,
            "--actor",
            actor,
            "--invocation-id",
            &invocation,
        ];
        if let Some(proof) = evidence {
            fs::write(&show_path, proof.to_string()).unwrap();
            args.extend(["--evidence-file", show_path.to_str().unwrap()]);
        }
        let report = run(&args, exit);
        assert_eq!(report["command"], "operation show");
        assert_eq!(report["schema_version"], 2);
        for private in [
            "Synthetic receipt note body",
            "fixture-note-auth",
            "fixture-note-evidence",
            "reader-password",
        ] {
            assert!(!report.to_string().contains(private), "{private}");
        }
        report
    };
    let operation_proof = |id: &str| {
        let mut proof = declaration(
            "operation_receipt",
            "inspect_operation_receipt",
            request.as_str(),
            commit,
        );
        proof["record"]["id"] = id.into();
        proof["bot_id"] = Value::Null;
        proof["actor_ref"] = "fixture-holder".into();
        proof
    };
    let proof = operation_proof(&request);
    let report = operation(&request, &area, &issue, "fixture-holder", Some(&proof), 0);
    assert_eq!(report["data"]["result"], note_result);
    assert_eq!(report["data"]["base_commit"], note_base);
    assert_eq!(report["data"]["confirmed_commit"], note_commit);
    assert_eq!(report["data"]["outcome"], "confirmed");
    assert_eq!(report["context"]["db_commit"], commit);
    assert_ne!(commit, note_commit);
    assert_eq!(report["data"]["current_claim"]["claim_id"], id(14));
    let no_evidence = operation(&request, &area, &issue, "fixture-holder", None, 7);
    assert_eq!(no_evidence["error"]["code"], "AUTHORITY_UNVERIFIED");
    let wrong_actor = operation(&request, &area, &issue, "fixture-other", Some(&proof), 6);
    assert_eq!(wrong_actor["error"]["code"], "UNAUTHORIZED");
    let mut out_of_scope = proof.clone();
    out_of_scope["issue_id"] = id(13).into();
    let wrong_issue = operation(
        &request,
        &area,
        &id(13),
        "fixture-holder",
        Some(&out_of_scope),
        7,
    );
    assert_eq!(wrong_issue["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert!(wrong_issue.get("data").is_none());
    let unsupported = operation(
        &activation,
        &area,
        &issue,
        "fixture-holder",
        Some(&operation_proof(&activation)),
        7,
    );
    assert_eq!(unsupported["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert!(unsupported.get("data").is_none());

    let unverified_history = operation(
        &unverified_request,
        &area,
        &issue,
        "fixture-holder",
        Some(&operation_proof(&unverified_request)),
        8,
    );
    assert_eq!(unverified_history["error"]["code"], "COMMIT_UNCERTAIN");
    assert!(unverified_history.get("data").is_none());

    for (request_id, _, _, _) in &mismatches {
        let mismatch = operation(
            request_id,
            &area,
            &issue,
            "fixture-holder",
            Some(&operation_proof(request_id)),
            8,
        );
        assert_eq!(mismatch["error"]["code"], "COMMIT_UNCERTAIN");
        assert!(mismatch.get("data").is_none());
        assert!(!mismatch.to_string().contains(&body_sha256));
    }

    let missing = id(49);
    let missing_proof = operation_proof(&missing);
    let absent = operation(
        &missing,
        &area,
        &issue,
        "fixture-holder",
        Some(&missing_proof),
        3,
    );
    assert_eq!(absent["error"]["code"], "NOT_FOUND");
    let marker = state.join("pending-request.json");
    let marker_bytes = b"synthetic incomplete pending intent";
    fs::write(&marker, marker_bytes).unwrap();
    let unknown = operation(
        &missing,
        &area,
        &issue,
        "fixture-holder",
        Some(&missing_proof),
        8,
    );
    assert_eq!(unknown["error"]["code"], "COMMIT_UNCERTAIN");
    let still_confirmed = operation(&request, &area, &issue, "fixture-holder", Some(&proof), 0);
    assert_eq!(still_confirmed["data"]["confirmed_commit"], note_commit);
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);

    let restart = |change: Option<OfflineFixtureEdit<'_>>, baseline: Option<&std::ffi::OsStr>| {
        let stopped = run(&["workspace", "stop"], 0);
        assert_eq!(stopped["data"]["process_exit"], "pidfd_verified");
        if let Some(change) = change {
            let fixture = cleanup.fixture.as_ref().expect("armed runtime fixture");
            let db = root.join("db");
            match change {
                OfflineFixtureEdit::CommitSql(sql) => {
                    fixture.local_at(&db, &["sql"], Some(sql));
                    fixture.local_at(&db, &["add", "."], None);
                    fixture.local_at(
                        &db,
                        &[
                            "commit",
                            "-m",
                            "Synthetic retained-history or ancestry change",
                        ],
                        None,
                    );
                }
                OfflineFixtureEdit::RestoreHead(commit) => {
                    fixture.local_at(&db, &["reset", "--hard", commit], None);
                }
            }
        }
        let start = [
            "workspace",
            "start",
            "--dolt-bin",
            dolt.to_str().unwrap(),
            "--port",
            &port,
        ];
        if let Some(binary) = baseline {
            let output = invoke_as(binary, &start).unwrap();
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(output.status.success(), "{report}");
        } else {
            run(&start, 0);
        }
    };
    issue_note_writer_fixture(
        &state,
        &fixture_root,
        &port,
        &revision,
        &run,
        &restart,
        &dolt,
    );
    issue_reconcile::exercise(&state, &fixture_root, &port, &dolt, &run, &restart);
    issue_dependency::exercise(&state, &fixture_root, &port, &dolt, &invoke, &restart);
    issue_parent::exercise(&state, &fixture_root, &port, &dolt, &invoke, &restart);
    issue_update::exercise(&state, &fixture_root, &port, &dolt, &run, &restart);
    issue_transition::exercise(&state, &fixture_root, &port, &dolt, &invoke, &restart);
    issue_create_writer_fixture(&state, &fixture_root, &port, &dolt, &run, &restart);
    assert!(cleanup.finish(), "Verified runtime fixture stop failed");
}

enum OfflineFixtureEdit<'a> {
    CommitSql(&'a str),
    RestoreHead(&'a str),
}

fn issue_note_writer_fixture(
    state: &Path,
    fixture_root: &Path,
    port: &str,
    revision: &str,
    run: &impl Fn(&[&str], i32) -> Value,
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
    dolt: &Path,
) {
    use sha2::{Digest, Sha256};
    let workspace = id(1);
    let area = id(3);
    let issue = id(11);
    let claim = id(14);
    let body_path = fixture_root.join("progress.txt");
    let decision_path = fixture_root.join("decision.json");
    let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let append = |kind: &str, request: &str, actor: &str, expected: &str, body: &str, exit| {
        let payload = serde_json::json!([
            1,
            "issue.note",
            workspace,
            area,
            issue,
            actor,
            claim,
            expected,
            kind,
            digest(body.as_bytes()),
        ]);
        let now = fixture_now();
        let evidence = serde_json::json!({
            "decision": "allow",
            "evidence": {
                "version": 1, "operation": "issue.note", "workspace_id": workspace,
                "request_id": request, "fingerprint": digest(payload.to_string().as_bytes()),
                "actor_ref": actor, "bot_id": null, "area_id": area, "issue_id": issue,
                "expected_issue_revision": expected, "claim_id": claim,
                "authorization_ref": "fixture-write-authorization",
                "evidence_id": "fixture-write-evidence", "revision": "r1",
                "source_ref": "fixture-write-source", "issuer_ref": "fixture-write-issuer",
                "valid_from": fixture_timestamp(now - time::Duration::seconds(10)),
                "expires_at": fixture_timestamp(now + time::Duration::hours(1)), "withdrawn": false,
            }
        });
        fs::write(&body_path, body).unwrap();
        fs::write(&decision_path, evidence.to_string()).unwrap();
        let report = run(
            &[
                "issue",
                "note",
                &issue,
                "--kind",
                kind,
                "--workspace-id",
                &workspace,
                "--database",
                "db",
                "--port",
                port,
                "--area",
                &area,
                "--claim",
                &claim,
                "--actor",
                actor,
                "--if-match",
                &format!("{issue}={expected}"),
                "--request-id",
                request,
                "--file",
                body_path.to_str().unwrap(),
                "--evidence-file",
                decision_path.to_str().unwrap(),
            ],
            exit,
        );
        assert_eq!(report["command"], "issue note");
        assert!(!report.to_string().contains("fixture-write-authorization"));
        report
    };

    let note = |request: &str, actor: &str, expected: &str, body: &str, exit| {
        append("progress", request, actor, expected, body, exit)
    };

    let marker = state.join("pending-request.json");
    let blocked = note(
        &id(60),
        "fixture-holder",
        revision,
        "Synthetic progress\r\n",
        8,
    );
    assert_eq!(blocked["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(
        fs::read(&marker).unwrap(),
        b"synthetic incomplete pending intent"
    );
    // The preceding inspection fixture created this deliberately malformed marker.
    fs::remove_file(&marker).unwrap();

    let holder = note(&id(61), "fixture-other", revision, "Synthetic progress", 5);
    assert_eq!(holder["error"]["code"], "CLAIM_MISMATCH");
    let body = "Synthetic progress\r\nλ\n";
    let written = note(&id(60), "fixture-holder", revision, body, 0);
    assert_eq!(written["data"]["outcome"], "confirmed");
    assert!(!written.to_string().contains("Synthetic progress"));
    assert!(!marker.exists());

    let replayed = note(&id(60), "fixture-holder", revision, body, 0);
    assert_eq!(replayed["data"]["outcome"], "replayed");
    for key in ["base_commit", "confirmed_commit", "result"] {
        assert_eq!(written["data"][key], replayed["data"][key]);
    }
    let changed = note(
        &id(60),
        "fixture-holder",
        revision,
        "Synthetic progress\nλ\n",
        5,
    );
    assert_eq!(changed["error"]["code"], "REQUEST_CONFLICT");
    let stale = note(&id(62), "fixture-holder", revision, body, 5);
    assert_eq!(stale["error"]["code"], "STALE_REVISION");

    let connect = |role: &str| connect_owned(state, port, role);
    let mut reader = connect("reader");
    let head: String = reader
        .query_first("SELECT DOLT_HASHOF('HEAD')")
        .unwrap()
        .unwrap();
    assert_eq!(written["data"]["confirmed_commit"], head);
    let actual: (String, Vec<u8>, String, u64, u64) = reader
        .exec_first(
            format!(
                "SELECT i.revision, CAST(u.body AS BINARY), r.base_commit,
             (SELECT COUNT(*) FROM issue_updates AS OF '{head}'),
             (SELECT COUNT(*) FROM operation_receipts AS OF '{head}')
         FROM issues AS OF '{head}' i
         JOIN issue_updates AS OF '{head}' u ON u.issue_id = i.id
         JOIN operation_receipts AS OF '{head}' r ON r.request_id = ?
         WHERE i.id = ? AND u.id = ?"
            ),
            (
                id(60),
                &issue,
                written["data"]["result"]["note_id"].as_str().unwrap(),
            ),
        )
        .unwrap()
        .unwrap();
    assert_eq!(written["data"]["result"]["issue_revision"], actual.0);
    assert_eq!(actual.1, body.as_bytes());
    assert_eq!(written["data"]["base_commit"], actual.2);
    // Compare with the committed base so other fixture receipts can coexist.
    let base = actual.2;
    let before: (u64, u64) = reader
        .query_first(format!(
            "SELECT (SELECT COUNT(*) FROM issue_updates AS OF '{base}'),
                (SELECT COUNT(*) FROM operation_receipts AS OF '{base}')"
        ))
        .unwrap()
        .unwrap();
    assert_eq!((actual.3, actual.4), (before.0 + 1, before.1 + 1));

    let mut prior = connect("writer");
    prior.query_drop("START TRANSACTION").unwrap();
    let session: u64 = prior
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    let pending = write_note_pending_fixture(state, &id(63), &head, session);
    let waiting = note(&id(60), "fixture-holder", revision, body, 8);
    assert_eq!(waiting["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(fs::read_to_string(&marker).unwrap(), pending);

    prior.query_drop("ROLLBACK").unwrap();
    drop(prior);
    let mut observer = connect("writer");
    wait_for_session_exit(&mut observer, session);
    let recovered = note(&id(60), "fixture-holder", revision, body, 0);
    assert_eq!(recovered["data"], replayed["data"]);
    assert!(!marker.exists());

    let receipt: (String, String, String, String, String, String, String, String) = reader
        .exec_first(
            "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
             FROM operation_receipts WHERE CAST(request_id AS BINARY) = ?",
            (id(60),),
        )
        .unwrap()
        .unwrap();
    let mut committed: Value = serde_json::from_str(&pending).unwrap();
    committed["request_id"] = serde_json::json!(id(60));
    committed["base_commit"] = serde_json::json!(receipt.4);
    committed["fingerprint"] = serde_json::json!(receipt.2);
    committed["receipt_digest"] =
        serde_json::json!(digest(serde_json::to_string(&receipt).unwrap().as_bytes()));
    drop(reader);
    drop(observer);
    stopped_note_recovery_fixture(state, dolt, port, run, &connect, committed);
    let owner_parent = id(2);
    let archived_root = id(70);
    restart(Some(OfflineFixtureEdit::CommitSql(&format!(
        "INSERT INTO areas (id, slug, name, revision, archived_at)
            VALUES ('{archived_root}', 'archive-fixture', 'Archive fixture', '{}', UTC_TIMESTAMP(6));
         UPDATE areas SET parent_id = '{archived_root}' WHERE id = '{owner_parent}';", id(4)
    ))), None);
    let mut reader = connect("reader");
    let archived_base = storage_state(&mut reader);
    let current_revision = written["data"]["result"]["issue_revision"]
        .as_str()
        .unwrap();
    let archived = note(&id(64), "fixture-holder", current_revision, body, 4);
    assert_eq!(archived["error"]["code"], "INVALID_TRANSITION");
    let archived_replay = note(&id(60), "fixture-holder", revision, body, 0);
    assert_eq!(archived_replay["data"], replayed["data"]);
    let other_actor = note(&id(60), "fixture-other", revision, body, 7);
    assert_eq!(other_actor["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(storage_state(&mut reader), archived_base);
    assert!(!marker.exists());

    drop(reader);
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "UPDATE areas SET archived_at = NULL WHERE id = '{archived_root}';
         DELETE FROM operation_receipts WHERE request_id = '{}';",
            id(60)
        ))),
        None,
    );
    let mut reader = connect("reader");
    let deleted_base = storage_state(&mut reader);
    let effect =
        |db: &mut Conn| -> (u64, u64, String) {
            db.exec_first(
            "SELECT (SELECT COUNT(*) FROM operation_receipts WHERE CAST(request_id AS BINARY) = ?),
                    (SELECT COUNT(*) FROM issue_updates WHERE CAST(issue_id AS BINARY) = ?),
                    revision FROM issues WHERE CAST(id AS BINARY) = ?",
            (id(60), &issue, &issue),
        ).unwrap().unwrap()
        };
    let deleted_effect = effect(&mut reader);
    assert_eq!(deleted_effect.0, 0);
    assert_eq!(deleted_effect.2, current_revision);
    let reused = note(&id(60), "fixture-holder", current_revision, body, 8);
    assert_eq!(reused["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(storage_state(&mut reader), deleted_base);
    assert_eq!(effect(&mut reader), deleted_effect);
    assert!(!marker.exists());

    if let Some(baseline) = std::env::var_os("HIVE_ISSUE_NOTE_BASELINE_BIN") {
        drop(reader);
        restart(None, Some(&baseline));
        let mut reader = connect("reader");
        let legacy_base = storage_state(&mut reader);
        let legacy = note(&id(65), "fixture-holder", current_revision, body, 7);
        assert_eq!(legacy["error"]["code"], "RUNTIME_UNAVAILABLE");
        let message = legacy["error"]["message"].as_str().unwrap();
        assert!(message.contains("lacks the required Issue note writer grants"));
        assert!(message.contains("workspace stop") && message.contains("workspace start"));
        assert!(!message.contains("writer_") && !message.contains(&launch_nonce(state)));
        assert!(!marker.exists());

        let old_pending = write_note_pending_fixture(state, &id(66), &legacy_base.0, 1);
        let legacy = note(&id(65), "fixture-holder", current_revision, body, 7);
        assert_eq!(legacy["error"]["code"], "RUNTIME_UNAVAILABLE");
        assert!(
            legacy["error"]["message"]
                .as_str()
                .unwrap()
                .contains("reviewed recovery")
        );
        assert_eq!(storage_state(&mut reader), legacy_base);
        drop(reader);
        let stopped = run(&["workspace", "stop"], 0);
        assert_eq!(stopped["data"]["pending_request"], "recorded");
        assert_eq!(fs::read_to_string(&marker).unwrap(), old_pending);
        let start = [
            "workspace",
            "start",
            "--dolt-bin",
            dolt.to_str().unwrap(),
            "--port",
            port,
        ];
        let blocked = run(&start, 7);
        assert_eq!(blocked["error"]["code"], "AUTHORITY_UNVERIFIED");
        assert_eq!(fs::read_to_string(&marker).unwrap(), old_pending);
        // Only the fixture removes its deliberately fabricated marker.
        fs::remove_file(&marker).unwrap();
        run(&start, 0);
    }

    let clean_before_receipt_probe = storage_state(&mut connect("reader"));
    let receipt_ddl = include_str!("../../assets/schema/dolt/0001_initial.sql")
        .split("CREATE TABLE operation_receipts")
        .nth(1)
        .unwrap()
        .split_once(';')
        .unwrap()
        .0;
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "DROP TABLE operation_receipts;
             CALL DOLT_ADD('.');
             CALL DOLT_COMMIT('-m', 'Synthetic receipt table removal');
             CREATE TABLE operation_receipts{receipt_ddl};"
        ))),
        None,
    );
    let mut reader = connect("reader");
    let new_base = storage_state(&mut reader);
    let introduced = written["data"]["confirmed_commit"].as_str().unwrap();
    let old_count: u64 = reader
        .exec_first(
            format!(
                "SELECT COUNT(*) FROM operation_receipts AS OF '{introduced}' WHERE request_id = ?"
            ),
            (id(60),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(old_count, 1);
    let before = effect(&mut reader);
    assert_eq!(before.0, 0);
    let incomplete = note(&id(60), "fixture-holder", current_revision, body, 8);
    assert_eq!(incomplete["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(storage_state(&mut reader), new_base);
    assert_eq!(effect(&mut reader), before);
    assert!(!marker.exists());
    drop(reader);

    // Leave the deliberately truncated history behind in this disposable fixture.
    // The following create tests need the original complete receipt history.
    restart(
        Some(OfflineFixtureEdit::RestoreHead(
            &clean_before_receipt_probe.0,
        )),
        None,
    );
    assert_eq!(
        storage_state(&mut connect("reader")),
        clean_before_receipt_probe
    );

    let mut reader = connect("reader");
    for (kind, request, body) in [
        (
            "evidence",
            id(98),
            "Synthetic evidence for a fresh worker\r\nλ\n",
        ),
        (
            "handoff",
            id(99),
            "Synthetic handoff for a fresh worker\r\nλ\n",
        ),
    ] {
        let expected: String = reader
            .exec_first("SELECT revision FROM issues WHERE id = ?", (&issue,))
            .unwrap()
            .unwrap();
        let appended = append(kind, &request, "fixture-holder", &expected, body, 0);
        assert_eq!(appended["data"]["outcome"], "confirmed");
        assert!(!appended.to_string().contains("Synthetic"));
        let note_id = appended["data"]["result"]["note_id"].as_str().unwrap();
        let stored: (String, Vec<u8>, Option<String>) = reader
            .exec_first(
                "SELECT kind, CAST(body AS BINARY), transition_json FROM issue_updates WHERE id = ?",
                (note_id,),
            )
            .unwrap()
            .unwrap();
        assert_eq!(stored, (kind.to_owned(), body.as_bytes().to_vec(), None));
        let replay = append(kind, &request, "fixture-holder", &expected, body, 0);
        assert_eq!(replay["data"]["outcome"], "replayed");
        assert_eq!(replay["data"]["result"], appended["data"]["result"]);
        let changed = append("progress", &request, "fixture-holder", &expected, body, 5);
        assert_eq!(changed["error"]["code"], "REQUEST_CONFLICT");
    }
    drop(reader);
    restart(
        Some(OfflineFixtureEdit::RestoreHead(
            &clean_before_receipt_probe.0,
        )),
        None,
    );
}

#[cfg(target_os = "linux")]
fn stopped_note_recovery_fixture(
    state: &Path,
    dolt: &Path,
    port: &str,
    run: &impl Fn(&[&str], i32) -> Value,
    connect: &impl Fn(&str) -> Conn,
    committed: Value,
) {
    let server = state.join("server.json");
    let marker = state.join("pending-request.json");
    let proof = state.join("stopped-request.json");
    let dolt_bin = dolt.to_str().unwrap();
    let recover = ["workspace", "recover", "--dolt-bin", dolt_bin];
    let start = ["workspace", "start", "--dolt-bin", dolt_bin, "--port", port];
    let stop = ["workspace", "stop"];
    let record = |marker_value: &Value| {
        fs::write(&marker, marker_value.to_string()).unwrap();
        fs::set_permissions(&marker, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    };
    let refused = |args: &[&str], expected: &Value| {
        let result = run(args, 8);
        assert_eq!(result["error"]["code"], "COMMIT_UNCERTAIN");
        assert_eq!(fs::read_to_string(&marker).unwrap(), expected.to_string());
        assert!(!result.to_string().contains("fixture-holder"));
        assert!(!result.to_string().contains("receipt_digest"));
    };
    let resolved = |outcome| {
        let report = run(&recover, 0);
        assert_eq!(report["data"]["state"], "stopped");
        assert_eq!(report["data"]["process_exit"], "pidfd_verified");
        assert_eq!(report["data"]["outcome"], outcome);
        assert!(!report.to_string().contains("fixture-holder"));
        assert!(!marker.exists() && !proof.exists() && !server.exists());
        assert!(fs::read_dir(state).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            !name.to_string_lossy().starts_with("run-")
                && !name.to_string_lossy().starts_with(".recovery-")
        }));
    };

    let server_record = fs::read(&server).unwrap();
    record(&committed);
    refused(&recover, &committed); // A current launch gives no stopped proof.
    run(&stop, 0);
    let saved = state.join("saved-proof");
    fs::rename(&proof, &saved).unwrap();
    refused(&recover, &committed);
    assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    fs::rename(&saved, &proof).unwrap();
    let held = fs::File::open(state.join("lock")).unwrap();
    // SAFETY: held owns the valid descriptor throughout the contention check.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert_eq!(run(&recover, 5)["error"]["code"], "LOCK_BUSY");
    assert_eq!(fs::read_to_string(&marker).unwrap(), committed.to_string());
    drop(held);
    refused(
        &[
            "workspace",
            "recover",
            "--dolt-bin",
            "/fixture-missing-dolt",
        ],
        &committed,
    );

    // Interruption after removing the launch directory and before unlinking
    // server.json leaves the synced exit proof and exact marker usable.
    fs::write(&server, &server_record).unwrap();
    resolved("confirmed");
    run(&start, 0);
    let mut writer = connect("writer");
    let (head, session, original_title): (String, u64, String) = writer
        .exec_first(
            "SELECT DOLT_HASHOF('HEAD'), CONNECTION_ID(), title FROM issues WHERE id = ?",
            (id(11),),
        )
        .unwrap()
        .unwrap();
    let mut rollback = committed;
    let launch = || serde_json::from_slice::<Value>(&fs::read(&server).unwrap()).unwrap();
    rollback["launch_nonce"] = launch()["nonce"].clone();
    rollback["session_id"] = serde_json::json!(session);
    rollback["request_id"] = serde_json::json!(id(68));
    rollback["base_commit"] = serde_json::json!(head);
    writer.query_drop("START TRANSACTION").unwrap();
    writer
        .exec_drop(
            "UPDATE issues SET title = 'Fixture rolled-back title' WHERE id = ?",
            (id(11),),
        )
        .unwrap();
    record(&rollback);
    drop(writer);
    let active = launch();
    run(&stop, 0);

    let root = Path::new(active["workspace_root"].as_str().unwrap());
    let db = root.join("db");
    let original = root.join("saved-db");
    fs::rename(&db, &original).unwrap();
    fs::create_dir(&db).unwrap();
    fs::rename(original.join(".dolt"), db.join(".dolt")).unwrap();
    refused(&recover, &rollback);
    fs::rename(db.join(".dolt"), original.join(".dolt")).unwrap();
    fs::remove_dir(&db).unwrap();
    fs::rename(&original, &db).unwrap();
    resolved("rolled_back");

    run(&start, 0);
    let mut reader = connect("reader");
    let observed: (String, String) = reader
        .exec_first(
            "SELECT DOLT_HASHOF('HEAD'), title FROM issues WHERE id = ?",
            (id(11),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(observed, (head, original_title.clone()));
    drop(reader);
    let mut unsupported = rollback.clone();
    unsupported["launch_nonce"] = launch()["nonce"].clone();
    unsupported["version"] = serde_json::json!(3);
    unsupported["kind"] = serde_json::json!("issue.create");
    record(&unsupported);
    run(&stop, 0);
    assert!(proof.exists());
    refused(&recover, &unsupported);
    assert!(proof.exists());
    assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    // Only the fixture removes its deliberately unsupported marker.
    fs::remove_file(&marker).unwrap();

    run(&start, 0);
    let mut writer = connect("writer");
    writer
        .exec_drop(
            "UPDATE issues SET title = 'Fixture dirty title' WHERE id = ?",
            (id(11),),
        )
        .unwrap();
    let dirty: bool = writer
        .query_first("SELECT EXISTS(SELECT 1 FROM dolt_status)")
        .unwrap()
        .unwrap();
    assert!(dirty);
    rollback["launch_nonce"] = launch()["nonce"].clone();
    rollback["session_id"] = serde_json::json!(
        writer
            .query_first::<u64, _>("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap()
    );
    record(&rollback);
    drop(writer);
    run(&stop, 0);
    assert!(proof.exists());
    refused(&recover, &rollback);
    assert!(proof.exists());
    assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    // Restore only the deliberately fabricated fixture state for later tests.
    fs::remove_file(&marker).unwrap();
    run(&start, 0);
    let mut writer = connect("writer");
    writer
        .exec_drop(
            "UPDATE issues SET title = ? WHERE id = ?",
            (original_title, id(11)),
        )
        .unwrap();
    let dirty: bool = writer
        .query_first("SELECT EXISTS(SELECT 1 FROM dolt_status)")
        .unwrap()
        .unwrap();
    assert!(!dirty);
}

fn write_note_pending_fixture(state: &Path, request: &str, base: &str, session: u64) -> String {
    let pending = serde_json::json!({
        "version": 1, "workspace_id": id(1), "launch_nonce": launch_nonce(state),
        "session_id": session, "request_id": request, "base_commit": base,
        "fingerprint": "a".repeat(64), "area_id": id(3), "issue_id": id(11),
        "receipt_digest": "b".repeat(64),
    })
    .to_string();
    let marker = state.join("pending-request.json");
    fs::write(&marker, &pending).unwrap();
    fs::set_permissions(&marker, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    pending
}

fn wait_for_session_exit(observer: &mut Conn, session: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while observer
        .exec_first::<u64, _, _>(
            "SELECT ID FROM information_schema.PROCESSLIST WHERE ID = ?",
            (session,),
        )
        .unwrap()
        .is_some()
    {
        assert!(Instant::now() < deadline, "Synthetic session did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}

fn fixture_now() -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    )
    .unwrap()
}

fn fixture_timestamp(at: time::OffsetDateTime) -> String {
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        at.date(),
        at.hour(),
        at.minute(),
        at.second(),
    )
}

fn launch_nonce(state: &Path) -> String {
    let server: Value =
        serde_json::from_slice(&fs::read(state.join("server.json")).unwrap()).unwrap();
    server["nonce"].as_str().unwrap().to_owned()
}

fn connect_owned(state: &Path, port: &str, role: &str) -> Conn {
    let nonce = launch_nonce(state);
    let password = fs::read_to_string(state.join(format!("run-{nonce}/{role}-password"))).unwrap();
    let user = format!("{role}_{}", &nonce.replace('-', "")[..24]);
    Conn::new(
        OptsBuilder::new()
            .ip_or_hostname(Some(HOST))
            .tcp_port(port.parse().unwrap())
            .prefer_socket(false)
            .user(Some(user))
            .pass(Some(password))
            .db_name(Some("db")),
    )
    .unwrap_or_else(|_| panic!("Could not connect the synthetic independent client."))
}

fn issue_create_writer_fixture(
    state: &Path,
    root: &Path,
    port: &str,
    dolt: &Path,
    run: &impl Fn(&[&str], i32) -> Value,
    restart: &impl Fn(Option<OfflineFixtureEdit<'_>>, Option<&std::ffi::OsStr>),
) {
    let workspace = id(1);
    let area = id(3);
    let actor = "fixture-planner";
    let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let decision_path = root.join("create-decision.json");
    let body_path = root.join("issue-body.md");
    let marker = state.join("pending-request.json");
    let create =
        |request: &str, area: &str, title: &str, body: Option<&str>, expiry_hours: i64, exit| {
            let payload = serde_json::json!([
                1,
                "issue.create",
                workspace,
                area,
                actor,
                "task",
                title,
                body.map(|value| digest(value.as_bytes())),
            ]);
            let now = fixture_now();
            let evidence = serde_json::json!({
                "decision": "allow",
                "evidence": {
                    "version": 1, "operation": "issue.create", "workspace_id": workspace,
                    "request_id": request, "fingerprint": digest(payload.to_string().as_bytes()),
                    "actor_ref": actor, "area_id": area,
                    "authorization_ref": "fixture-create-authorization",
                    "evidence_id": "fixture-create-evidence", "revision": "r1",
                    "source_ref": "fixture-create-source", "issuer_ref": "fixture-create-issuer",
                    "valid_from": fixture_timestamp(now - time::Duration::hours(2)),
                    "expires_at": fixture_timestamp(now + time::Duration::hours(expiry_hours)),
                    "withdrawn": false,
                }
            });
            fs::write(&decision_path, evidence.to_string()).unwrap();
            let mut args = vec![
                "issue",
                "create",
                "--workspace-id",
                &workspace,
                "--database",
                "db",
                "--port",
                port,
                "--area",
                area,
                "--type",
                "task",
                "--title",
                title,
                "--actor",
                actor,
                "--request-id",
                request,
                "--evidence-file",
                decision_path.to_str().unwrap(),
            ];
            if let Some(body) = body {
                fs::write(&body_path, body).unwrap();
                args.extend(["--file", body_path.to_str().unwrap()]);
            }
            let report = run(&args, exit);
            assert_eq!(report["command"], "issue create");
            assert!(!report.to_string().contains("fixture-create-authorization"));
            report
        };

    let title = "Fixture café";
    let body = "Synthetic Issue body\r\nλ\n";
    let request = id(80);
    let connect = |role: &str| connect_owned(state, port, role);
    if let Some(baseline) = std::env::var_os("HIVE_ISSUE_CREATE_BASELINE_BIN") {
        restart(None, Some(&baseline));
        let mut reader = connect("reader");
        let before = storage_state(&mut reader);
        let mut observer = connect("writer");
        let mut prior = connect("writer");
        prior.query_drop("START TRANSACTION").unwrap();
        let session: u64 = prior
            .query_first("SELECT CONNECTION_ID()")
            .unwrap()
            .unwrap();
        let pending = write_note_pending_fixture(state, &id(88), &before.0, session);
        let blocked = create(&request, &area, title, Some(body), 1, 8);
        assert_eq!(blocked["error"]["code"], "COMMIT_UNCERTAIN");
        assert_eq!(fs::read_to_string(&marker).unwrap(), pending);
        assert_eq!(storage_state(&mut reader), before);

        prior.query_drop("ROLLBACK").unwrap();
        drop(prior);
        wait_for_session_exit(&mut observer, session);
        drop(observer);
        let legacy = create(&request, &area, title, Some(body), 1, 7);
        assert_eq!(legacy["error"]["code"], "RUNTIME_UNAVAILABLE");
        assert!(
            legacy["error"]["message"]
                .as_str()
                .unwrap()
                .contains("workspace stop")
        );
        assert_eq!(storage_state(&mut reader), before);
        assert!(!marker.exists());
        drop(reader);
        restart(None, None);
    }

    let mut reader = connect("reader");
    let before = storage_state(&mut reader);
    let expired = create(&request, &area, title, Some(body), -1, 7);
    assert_eq!(expired["error"]["code"], "AUTHORITY_UNVERIFIED");
    let missing = create(&request, &id(85), title, Some(body), 1, 7);
    assert_eq!(missing["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(storage_state(&mut reader), before);
    assert!(!marker.exists());

    let created = create(&request, &area, title, Some(body), 1, 0);
    let result = &created["data"]["result"];
    assert_eq!(created["data"]["outcome"], "confirmed");
    assert_eq!(created["data"]["base_commit"], before.0);
    assert_eq!(result["area_id"], area);
    assert_eq!(result["number"], 5);
    let issue = result["issue_id"].as_str().unwrap();
    assert_eq!(&issue[14..15], "7");
    assert_eq!(&result["issue_revision"].as_str().unwrap()[14..15], "7");
    assert!(!created.to_string().contains("Synthetic Issue body"));
    assert!(!marker.exists());

    let after = storage_state(&mut reader);
    assert_eq!(created["data"]["confirmed_commit"], after.0);
    assert!(after.3.is_empty());
    let stored: (String, u64, String, String, String, Vec<u8>, u64) = reader
        .exec_first(
            "SELECT area_id, number, revision, status, title, CAST(body AS BINARY),
                (SELECT next_value FROM workspace_counters WHERE name = 'issue_number')
         FROM issues WHERE id = ?",
            (issue,),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        stored,
        (
            area.clone(),
            5,
            result["issue_revision"].as_str().unwrap().to_owned(),
            "backlog".to_owned(),
            title.to_owned(),
            body.as_bytes().to_owned(),
            6
        )
    );
    let replayed = create(&request, &area, title, Some(body), 1, 0);
    assert_eq!(replayed["data"]["outcome"], "replayed");
    assert_eq!(replayed["data"]["result"], *result);
    assert_eq!(replayed["data"]["confirmed_commit"], after.0);
    let expired = create(&request, &area, title, Some(body), -1, 7);
    assert_eq!(expired["error"]["code"], "AUTHORITY_UNVERIFIED");
    let conflict = create(&request, &area, title, None, 1, 5);
    assert_eq!(conflict["error"]["code"], "REQUEST_CONFLICT");
    let conflict = create(&id(46), &area, title, None, 1, 5);
    assert_eq!(conflict["error"]["code"], "REQUEST_CONFLICT");
    assert_eq!(storage_state(&mut reader), after);

    let missing_ack: (String, String, String, String, String, String, String, String) = reader.exec_first(
        "SELECT kind, actor_ref, fingerprint, authorization_ref, base_commit,
                DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), workflow_evidence_json, result_json
           FROM operation_receipts WHERE request_id = ?", (&request,),
    ).unwrap().unwrap();
    let mut current = connect("writer");
    let mut insert_tables: Vec<String> = current
        .query(
            "SELECT TABLE_NAME FROM information_schema.TABLE_PRIVILEGES
         WHERE TABLE_SCHEMA = 'db' AND PRIVILEGE_TYPE = 'INSERT' ORDER BY TABLE_NAME",
        )
        .unwrap();
    insert_tables.sort_unstable();
    assert_eq!(
        insert_tables,
        [
            "areas",
            "artifact_areas",
            "artifacts",
            "bots",
            "issue_claims",
            "issue_dependencies",
            "issue_updates",
            "issues",
            "operation_receipts",
            "project_issues",
            "projects",
        ]
    );
    let mut prior = connect("writer");
    prior.query_drop("START TRANSACTION").unwrap();
    let session: u64 = prior
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    let intent = serde_json::json!({
        "version": 2, "kind": "issue.create", "workspace_id": workspace,
        "launch_nonce": launch_nonce(state), "session_id": session, "request_id": request,
        "base_commit": before.0, "fingerprint": missing_ack.2, "area_id": area,
        "issue_id": issue, "receipt_digest": digest(serde_json::json!(missing_ack).to_string().as_bytes()),
    }).to_string();
    fs::write(&marker, &intent).unwrap();
    fs::set_permissions(&marker, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let blocked = create(&id(81), &area, title, None, 1, 8);
    assert_eq!(blocked["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(fs::read_to_string(&marker).unwrap(), intent);
    prior.query_drop("ROLLBACK").unwrap();
    drop(prior);
    wait_for_session_exit(&mut current, session);
    assert_eq!(
        create(&request, &area, title, Some(body), 1, 0)["data"],
        replayed["data"]
    );
    assert!(!marker.exists());

    drop(current);
    drop(reader);
    let committed_intent: Value = serde_json::from_str(&intent).unwrap();
    stopped_create_recovery_fixture(state, dolt, port, run, committed_intent);
    let mut reader = connect("reader");
    let mut current = connect("writer");
    assert_eq!(storage_state(&mut reader), after);

    current
        .query_drop("UPDATE workspace_counters SET next_value = 7 WHERE name = 'issue_number'")
        .unwrap();
    let dirty = storage_state(&mut reader);
    assert!(!dirty.3.is_empty());
    let rejected = create(&id(81), &area, title, None, 1, 5);
    assert_eq!(rejected["error"]["code"], "DIRTY_CONFLICT");
    assert_eq!(storage_state(&mut reader), dirty);
    current
        .query_drop("UPDATE workspace_counters SET next_value = 6 WHERE name = 'issue_number'")
        .unwrap();
    assert_eq!(storage_state(&mut reader), after);
    let second = create(&id(81), &area, title, None, 1, 0);
    assert_eq!(second["data"]["result"]["number"], 6);
    let second_id = second["data"]["result"]["issue_id"].as_str().unwrap();
    let no_body: bool = reader
        .exec_first("SELECT body IS NULL FROM issues WHERE id = ?", (second_id,))
        .unwrap()
        .unwrap();
    assert!(no_body);
    let observed_commit = storage_state(&mut reader).0;
    let invocation = id(86);
    let now = fixture_now();
    let inspection = serde_json::json!({
        "version": 1, "evidence_id": "fixture-inspection", "revision": "r1",
        "source_ref": "fixture-source", "issuer_ref": "fixture-issuer",
        "workspace_id": workspace, "actor_ref": actor, "bot_id": null,
        "area_id": area, "issue_id": issue, "operations": ["inspect_operation_receipt"],
        "record": {"kind": "operation_receipt", "id": request, "revision": observed_commit},
        "invocation_id": invocation, "db_commit": observed_commit,
        "valid_from": fixture_timestamp(now - time::Duration::seconds(10)),
        "expires_at": fixture_timestamp(now + time::Duration::hours(1)),
        "withdrawn": false,
    });
    fs::write(&decision_path, inspection.to_string()).unwrap();
    let inspected = run(
        &[
            "operation",
            "show",
            &request,
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
            actor,
            "--invocation-id",
            &invocation,
            "--evidence-file",
            decision_path.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(inspected["data"]["result"], *result);
    assert_eq!(inspected["data"]["confirmed_commit"], after.0);
    assert_eq!(inspected["context"]["db_commit"], observed_commit);
    assert!(inspected["data"]["current_claim"].is_null());

    drop(current);
    drop(reader);

    for (sql, request, error) in [
        (
            format!(
                "UPDATE areas SET archived_at = UTC_TIMESTAMP(6) WHERE id = '{}'",
                id(2)
            ),
            id(82),
            "INVALID_TRANSITION",
        ),
        (
            format!(
                "UPDATE areas SET archived_at = NULL WHERE id = '{}';
                  UPDATE workspace_counters SET next_value = 6 WHERE name = 'issue_number'",
                id(2)
            ),
            id(82),
            "INVALID_RECORD",
        ),
        (
            format!("DELETE FROM issues WHERE id = '{second_id}'"),
            id(82),
            "INVALID_RECORD",
        ),
        (
            "UPDATE workspace_counters SET next_value = 0 WHERE name = 'issue_number'".to_owned(),
            id(82),
            "INVALID_RECORD",
        ),
        (
            "DELETE FROM workspace_counters WHERE name = 'issue_number'".to_owned(),
            id(82),
            "INVALID_RECORD",
        ),
        (
            "INSERT INTO workspace_counters VALUES ('ISSUE_NUMBER', 7)".to_owned(),
            id(82),
            "INVALID_RECORD",
        ),
        (
            "DELETE FROM workspace_counters;
          INSERT INTO workspace_counters VALUES ('issue_number', 18446744073709551615)"
                .to_owned(),
            id(82),
            "INVALID_RECORD",
        ),
        (
            format!(
                "UPDATE workspace_counters SET next_value = 7 WHERE name = 'issue_number';
                  DELETE FROM operation_receipts WHERE request_id = '{}'",
                id(81)
            ),
            id(81),
            "COMMIT_UNCERTAIN",
        ),
    ] {
        restart(Some(OfflineFixtureEdit::CommitSql(&sql)), None);
        let mut reader = connect("reader");
        let unchanged = storage_state(&mut reader);
        let exit = if error == "COMMIT_UNCERTAIN" { 8 } else { 4 };
        assert_eq!(
            create(&request, &area, title, None, 1, exit)["error"]["code"],
            error
        );
        assert_eq!(storage_state(&mut reader), unchanged);
        assert!(!marker.exists());
    }
    // Preserve old commits while introducing a gap in the Issue table history.
    let tables = [
        "project_issues",
        "issue_dependencies",
        "issue_resources",
        "issue_claims",
        "issue_labels",
        "issue_updates",
        "artifact_issues",
        "issues",
    ];
    let drops = tables
        .map(|table| format!("DROP TABLE {table};"))
        .join("\n");
    let definitions = SCHEMA
        .split_inclusive(';')
        .filter(|statement| {
            tables.iter().any(|table| {
                statement
                    .trim_start()
                    .starts_with(&format!("CREATE TABLE {table} ("))
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), tables.len());
    restart(
        Some(OfflineFixtureEdit::CommitSql(&format!(
            "{drops}
         CALL DOLT_COMMIT('-Am', 'Synthetic Issue table drop');
         {}
         UPDATE workspace_counters SET next_value = 6 WHERE name = 'issue_number';",
            definitions.join("\n")
        ))),
        None,
    );
    let mut reader = connect("reader");
    let recreated = storage_state(&mut reader);
    let old_number: Option<u64> = reader
        .exec_first(
            format!(
                "SELECT number FROM issues AS OF '{}' WHERE id = ?",
                second["data"]["confirmed_commit"].as_str().unwrap(),
            ),
            (second_id,),
        )
        .unwrap();
    assert_eq!(old_number, Some(6));
    let newer_issues: Option<u64> = reader
        .query_first("SELECT number FROM issues WHERE number >= 6 LIMIT 1")
        .unwrap();
    assert!(newer_issues.is_none());
    let diff: Option<u64> = reader.exec_first(
        format!(
            "SELECT 1 FROM dolt_diff_issues AS OF '{}' WHERE from_number >= ? OR to_number >= ? LIMIT 1",
            recreated.0,
        ),
        (6, 6),
    ).unwrap();
    assert!(diff.is_none());
    let diagnostic: Option<(String, u64, String)> =
        reader.query_first("SHOW WARNINGS LIMIT 1").unwrap();
    assert!(
        matches!(diagnostic, Some((_, _, message)) if message.contains("cannot render full diff"))
    );

    let rejected = create(&id(87), &area, title, None, 1, 4);
    assert_eq!(rejected["error"]["code"], "INVALID_RECORD");
    assert_eq!(
        rejected["error"]["message"],
        "Retained Issue number history could not be verified."
    );
    assert_eq!(storage_state(&mut reader), recreated);
    assert!(!marker.exists());
}

fn stopped_create_recovery_fixture(
    state: &Path,
    dolt: &Path,
    port: &str,
    run: &impl Fn(&[&str], i32) -> Value,
    committed: Value,
) {
    let marker = state.join("pending-request.json");
    let proof = state.join("stopped-request.json");
    let dolt_bin = dolt.to_str().unwrap();
    let recover = ["workspace", "recover", "--dolt-bin", dolt_bin];
    let start = ["workspace", "start", "--dolt-bin", dolt_bin, "--port", port];
    let stop = || {
        let result = run(&["workspace", "stop"], 0);
        assert_eq!(result["data"]["process_exit"], "pidfd_verified");
        assert_eq!(result["data"]["pending_request"], "recorded");
        assert!(proof.exists());
    };
    let record = |intent: &Value| {
        fs::write(&marker, intent.to_string()).unwrap();
        fs::set_permissions(&marker, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    };
    let resolve = |outcome| {
        let result = run(&recover, 0);
        assert_eq!(result["data"]["state"], "stopped");
        assert_eq!(result["data"]["process_exit"], "pidfd_verified");
        assert_eq!(result["data"]["outcome"], outcome);
        assert!(!marker.exists() && !proof.exists() && !state.join("server.json").exists());
    };
    let connect = |role: &str| connect_owned(state, port, role);
    let mut reader = connect("reader");
    let committed_db = storage_state(&mut reader);
    drop(reader);

    record(&committed);
    stop();
    assert_eq!(fs::read_to_string(&marker).unwrap(), committed.to_string());
    resolve("confirmed");
    run(&start, 0);

    let mut writer = connect("writer");
    let (head, session, next): (String, u64, u64) = writer
        .query_first(
            "SELECT DOLT_HASHOF('HEAD'), CONNECTION_ID(), next_value FROM workspace_counters
             WHERE name = 'issue_number'",
        )
        .unwrap()
        .unwrap();
    assert_eq!(head, committed_db.0);
    assert_eq!(next, 6);
    let committed_request = committed["request_id"].as_str().unwrap().to_owned();
    let mut rollback = committed;
    let issue = id(89);
    let request = id(90);
    rollback["launch_nonce"] = launch_nonce(state).into();
    rollback["session_id"] = session.into();
    rollback["request_id"] = request.clone().into();
    rollback["issue_id"] = issue.clone().into();
    rollback["base_commit"] = head.into();
    writer.query_drop("START TRANSACTION").unwrap();
    writer.exec_drop(
        "UPDATE workspace_counters SET next_value = ? WHERE name = 'issue_number' AND next_value = ?",
        (next + 1, next),
    ).unwrap();
    assert_eq!(writer.affected_rows(), 1);
    writer.exec_drop(
        "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
         VALUES (?, ?, ?, 'Synthetic rolled-back Issue', 'task', 'backlog', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
        (&issue, next, id(3), id(91)),
    ).unwrap();
    writer
        .exec_drop(
            "INSERT INTO operation_receipts
         SELECT ?, kind, fingerprint, actor_ref, authorization_ref, workflow_evidence_json,
                base_commit, result_json, created_at
         FROM operation_receipts WHERE request_id = ?",
            (&request, &committed_request),
        )
        .unwrap();
    assert_eq!(writer.affected_rows(), 1);
    record(&rollback);
    drop(writer);
    stop();
    resolve("rolled_back");
    run(&start, 0);

    let mut reader = connect("reader");
    assert_eq!(storage_state(&mut reader), committed_db);
    let effects: (u64, u64, u64) = reader
        .exec_first(
            "SELECT next_value, (SELECT COUNT(*) FROM issues WHERE id = ?),
                    (SELECT COUNT(*) FROM operation_receipts WHERE request_id = ?)
             FROM workspace_counters WHERE name = 'issue_number'",
            (&issue, &request),
        )
        .unwrap()
        .unwrap();
    assert_eq!(effects, (next, 0, 0));
    let session: u64 = reader
        .query_first("SELECT CONNECTION_ID()")
        .unwrap()
        .unwrap();
    rollback["launch_nonce"] = launch_nonce(state).into();
    rollback["session_id"] = session.into();
    drop(reader);

    record(&rollback);
    stop();
    let proof_bytes = fs::read(&proof).unwrap();
    rollback["receipt_digest"] = "c".repeat(64).into();
    record(&rollback);
    let rejected = run(&recover, 8);
    assert_eq!(rejected["error"]["code"], "COMMIT_UNCERTAIN");
    assert_eq!(fs::read_to_string(&marker).unwrap(), rollback.to_string());
    assert_eq!(fs::read(&proof).unwrap(), proof_bytes);
    assert_eq!(run(&start, 7)["error"]["code"], "AUTHORITY_UNVERIFIED");
    // Continuing the test requires removing the fixture's deliberately altered marker.
    fs::remove_file(&marker).unwrap();
    run(&start, 0);
}

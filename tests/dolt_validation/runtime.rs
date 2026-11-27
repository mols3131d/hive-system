use super::*;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::Path;

fn random_token() -> String {
    fs::read_to_string("/proc/sys/kernel/random/uuid")
        .unwrap()
        .trim()
        .replace('-', "")
}

fn prepare() -> (Fixture, String, String) {
    let mut fixture = Fixture::initialize(&empty_seed_sql(CASE_INSENSITIVE));
    let state = fixture.root.join("runtime");
    fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
    let grants = state.join("privileges.db");
    let nonce = random_token()[..24].to_owned();
    fixture.password = random_token();
    let writer_password = random_token();
    let reader = format!("reader_{nonce}");
    let writer = format!("writer_{nonce}");
    let config = format!("--doltcfg-dir={}", state.display());
    let privilege_file = format!("--privilege-file={}", grants.display());

    fixture.local(
        &[&config, &privilege_file, "sql"],
        Some(&format!(
            "CREATE USER '{reader}'@'{HOST}' IDENTIFIED BY '{}';
             CREATE USER '{writer}'@'{HOST}' IDENTIFIED BY '{writer_password}';
             GRANT SELECT ON {DATABASE}.* TO '{reader}'@'{HOST}';
             GRANT SELECT ON {DATABASE}.* TO '{writer}'@'{HOST}';
             GRANT UPDATE ON {DATABASE}.workspace_counters TO '{writer}'@'{HOST}';
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
    let select = format!("GRANT SELECT ON `{DATABASE}`.*");
    let usage = "GRANT USAGE ON *.*".to_owned();
    assert_eq!(grants_for(&reader), [select.clone(), usage.clone()]);
    assert_eq!(
        grants_for(&writer),
        [
            select,
            format!("GRANT UPDATE ON `{DATABASE}`.`workspace_counters`"),
            usage
        ]
    );
    (fixture, nonce, writer_password)
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN"]
fn workspace_runtime_provisions_grants_offline() {
    prepare();
}

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP and Linux pidfds"]
fn workspace_runtime_probes_ownership_and_graceful_stop() {
    let available = TcpListener::bind((HOST, 0)).unwrap();
    let (mut fixture, nonce, writer_password) = prepare();
    let state = fixture.root.join("runtime");
    let db = fixture.root.join(DATABASE);
    let head_query = "SELECT DOLT_HASHOF('HEAD') AS head";
    let baseline = fixture.local(&["sql", "-r", "csv", "-q", head_query], None);
    let pending = state.join("pending-request.json");
    fs::write(&pending, b"synthetic pending request").unwrap();

    fixture.port = available.local_addr().unwrap().port();
    drop(available);
    let server = fixture
        .dolt_command()
        .current_dir(&state)
        .args([
            "sql-server",
            "--skip-root-user-initialization",
            "--host=127.0.0.1",
            &format!("--port={}", fixture.port),
            &format!("--data-dir={}", db.display()),
            &format!("--doltcfg-dir={}", state.display()),
            &format!("--privilege-file={}", state.join("privileges.db").display()),
            "--event-scheduler=OFF",
            "--loglevel=error",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
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

#[test]
#[ignore = "requires pinned Dolt at HIVE_DOLT_BIN, local TCP and Linux pidfds"]
fn workspace_runtime_cli_starts_and_stops_registered_workspace() {
    let fixture = Fixture::initialize(&empty_seed_sql(CASE_INSENSITIVE));
    let available = TcpListener::bind((HOST, 0)).unwrap();
    let port = available.local_addr().unwrap().port().to_string();
    drop(available);

    let workspace = fixture.root.join("workspace");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&workspace)
        .unwrap();
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
    let state_home = fixture.root.join("state");
    let state = state_home.join("hive/v2/workspaces").join(&workspace_id);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&state)
        .unwrap();
    let registration = state.join("active-root.json");
    let registered_root = serde_json::json!({
        "version": 2,
        "workspace_id": workspace_id,
        "workspace_root": workspace
    });
    fs::write(&registration, registered_root.to_string()).unwrap();

    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .env_clear()
            .env("HOME", fixture.root.join("home"))
            .env("XDG_STATE_HOME", &state_home)
            .current_dir(&fixture.root)
            .args(["--contract", "2", "--json", "--workspace"])
            .arg(&workspace)
            .arg("workspace")
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
}

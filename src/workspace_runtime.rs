use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::num::NonZeroU16;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Registration;
use super::unix::{
    Loopback, Server, check_identity, check_server, find_state, process_evidence, process_start,
    read_json,
};
use crate::cli::{CommonOptions, Format, WorkspaceCommand};
use crate::config::json_path;
use crate::config_file::ScopedDirectory;
use crate::config_v2::{Candidate, read_explicit, select_workspace};
use crate::dolt::{DOLT_VERSION, DoltTarget, Snapshot, is_uuid, parse_port};
use crate::error::{ErrorCode, HiveError};

const WAIT: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_millis(25);
const DATABASE: &str = "db";

pub(super) fn run(
    options: &CommonOptions,
    command: &WorkspaceCommand,
    format: &mut Format,
) -> Result<Value, HiveError> {
    with_registered(
        options,
        format,
        false,
        |workspace, state, db, _, noms, _| {
            let id = workspace.workspace_id();
            let root = json_path(&workspace.root.path)?;

            if state.contains(c"starting")? {
                return Err(unverified("An interrupted launch requires inspection."));
            }
            let server: Option<Server> = read_json(Some(state), "server.json")?;
            match command {
                WorkspaceCommand::Start { dolt_bin, port } => {
                    if server.is_some() {
                        return Err(unverified(
                            "A server record already exists for this workspace.",
                        ));
                    }
                    if state.contains(c"pending-request.json")? {
                        return Err(unverified(
                            "A pending request requires recovery before start.",
                        ));
                    }
                    start(state, db, noms, id, root, dolt_bin, parse_port(port)?)
                }
                WorkspaceCommand::Stop => {
                    let server =
                        server.ok_or_else(|| unverified("No owned server is recorded."))?;
                    check_server(&server, id, root)?;
                    stop(state, noms, &server)
                }
                WorkspaceCommand::Status(_) => unreachable!(),
            }
        },
    )
}

fn with_registered<T>(
    options: &CommonOptions,
    format: &mut Format,
    existing_lock: bool,
    operation: impl FnOnce(
        &Candidate,
        &ScopedDirectory,
        &ScopedDirectory,
        &ScopedDirectory,
        &ScopedDirectory,
        &File,
    ) -> Result<T, HiveError>,
) -> Result<T, HiveError> {
    let workspace = select_workspace(options, format)?.candidate;
    let id = workspace.workspace_id();
    let root = json_path(&workspace.root.path)?;
    let state = find_state(id, &workspace.root.path)?
        .ok_or_else(|| unverified("No active-root registration exists for this workspace."))?;
    require_private_dir(&state)?;
    require_local_filesystem(&state)?;
    let held_lock = if existing_lock {
        lock_file(regular(&state, "lock")?, libc::LOCK_EX)?
    } else {
        lock(&state)?
    };

    // Resolve again after taking the lock. A moved path or replaced descriptor
    // must not give a different root or UUID access to this state.
    let selected = read_explicit(&workspace.root.path)?;
    if workspace.root.file_identity()? != selected.root.file_identity()?
        || selected.workspace_id() != id
        || find_state(id, &workspace.root.path)?
            .ok_or_else(|| unverified("The workspace state directory disappeared."))?
            .file_identity()?
            != state.file_identity()?
    {
        return Err(unverified("The selected workspace or its state changed."));
    }
    let registration: Registration = read_json(Some(&state), "active-root.json")?
        .ok_or_else(|| unverified("No active-root registration exists for this workspace."))?;
    check_identity(
        registration.version,
        &registration.workspace_id,
        &registration.workspace_root,
        id,
        root,
    )?;
    let db = child_dir(&workspace.root, DATABASE)?;
    let native = child_dir(&db, ".dolt")?;
    let noms = child_dir(&native, "noms")?;
    require_local_filesystem(&noms)?;
    regular(&noms, "manifest")?;

    operation(&workspace, &state, &db, &native, &noms, &held_lock)
}

pub(super) fn backup(
    options: &CommonOptions,
    destination: &Path,
    format: &mut Format,
) -> Result<Value, HiveError> {
    with_registered(
        options,
        format,
        true,
        |workspace, state, db, native, noms, held_lock| {
            let check_stopped = || {
                for dir in [&workspace.root, state, db, native, noms] {
                    require_current_directory(dir)?;
                }
                let expected = held_lock.metadata().map_err(io_error)?;
                let current = regular(state, "lock")?.metadata().map_err(io_error)?;
                if (expected.dev(), expected.ino()) != (current.dev(), current.ino()) {
                    return Err(unverified("The workspace lock file changed."));
                }
                let selected = read_explicit(&workspace.root.path)?;
                let registration: Registration = read_json(Some(state), "active-root.json")?
                    .ok_or_else(|| unverified("The active-root registration disappeared."))?;
                check_identity(
                    registration.version,
                    &registration.workspace_id,
                    &registration.workspace_root,
                    workspace.workspace_id(),
                    json_path(&workspace.root.path)?,
                )?;
                if selected.workspace_id() != workspace.workspace_id()
                    || selected.policy_revision() != workspace.policy_revision()
                    || state.contains(c"pending-request.json")?
                    || state.contains(c"starting")?
                    || state.contains(c"server.json")?
                    || native.contains(c"sql-server.info")?
                {
                    return Err(unverified(
                        "Backup requires a stopped and unchanged workspace with no pending request.",
                    ));
                }
                for entry in fs::read_dir(at(state, "")).map_err(io_error)? {
                    let entry = entry.map_err(io_error)?;
                    if !matches!(
                        entry.file_name().to_str(),
                        Some("lock" | "active-root.json")
                    ) {
                        return Err(unverified(
                            "Unrecognized workspace runtime state requires inspection.",
                        ));
                    }
                }
                // Dolt takes its native lock when opening the store. Probe it
                // before running offline commands, which acquire it independently.
                lock_file(regular(noms, "LOCK")?, libc::LOCK_EX)?;
                Ok(())
            };
            check_stopped()?;
            crate::workspace_backup::create(
                workspace,
                state,
                db,
                native,
                noms,
                destination,
                check_stopped,
            )
        },
    )
}

fn start(
    state: &ScopedDirectory,
    db: &ScopedDirectory,
    noms: &ScopedDirectory,
    id: &str,
    root: &str,
    executable: &Path,
    port: NonZeroU16,
) -> Result<Value, HiveError> {
    if !executable.is_absolute() || port.get() < 1024 {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Specify an absolute Dolt binary and a port between 1024 and 65535.",
        ));
    }
    let executable = executable.canonicalize().map_err(io_error)?;
    let binary = fs::metadata(&executable).map_err(io_error)?;
    if !binary.is_file() {
        return Err(unavailable("The selected Dolt binary is unavailable."));
    }
    // Reserve the requested endpoint while preparing offline. Readiness checks
    // below catch another listener winning the race between release and bind.
    let available = TcpListener::bind(("127.0.0.1", port.get()))
        .map_err(|_| unavailable("The requested loopback port is unavailable."))?;
    let self_pid = i32::try_from(std::process::id())
        .map_err(|_| unavailable("The local PID is outside the supported range."))?;
    let capability = pidfd(self_pid)?;
    send_signal(&capability, 0)?;
    let nonce = token()?;
    let run = create_run(state, &nonce)?;
    let command = || dolt_command(&executable, &run.path);
    let prepare = || -> Result<(), HiveError> {
        let mut version = command();
        version.arg("version").stdout(Stdio::piped());
        let version = run_checked(&mut version, None)?;
        let version = String::from_utf8_lossy(&version);
        if version.trim() != format!("dolt version {DOLT_VERSION}") {
            return Err(HiveError::new(
                ErrorCode::SchemaIncompatible,
                format!("Workspace lifecycle requires Dolt {DOLT_VERSION}."),
            ));
        }
        let reader = token()?;
        let writer = token()?;
        let reader_user = reader_name(&nonce);
        let writer_user = credential_name("writer", &nonce);
        // This working directory is new and contains no database or server
        // metadata. Embedded SQL cannot attach to the selected workspace here.
        let mut provision = command();
        provision
            .arg(path_flag(
                "--privilege-file=",
                &run.path.join("privileges.db"),
            ))
            .arg("sql");
        let sql = format!(
            "CREATE USER '{reader_user}'@'127.0.0.1' IDENTIFIED BY '{reader}';\n             CREATE USER '{writer_user}'@'127.0.0.1' IDENTIFIED BY '{writer}';\n             GRANT SELECT ON db.* TO '{reader_user}'@'127.0.0.1';\n             GRANT SELECT ON db.* TO '{writer_user}'@'127.0.0.1';\n             GRANT UPDATE ON db.workspace_counters TO '{writer_user}'@'127.0.0.1';\n             DROP USER 'root'@'localhost';"
        );
        run_checked(&mut provision, Some(sql.as_bytes()))?;
        for (name, contents) in [
            ("reader-password", reader),
            ("writer-password", writer),
            ("binary-id", format!("{}:{}", binary.dev(), binary.ino())),
        ] {
            write_new(&run, name, contents.as_bytes())?;
        }
        regular(&run, "privileges.db")?
            .sync_all()
            .map_err(io_error)?;
        sync(&run)?;
        record_launch_intent(state, &run, db, &nonce)
    };
    if let Err(error) = prepare() {
        if !state.contains(c"starting")? {
            remove_run(&run)?;
        }
        return Err(error);
    }

    drop(available);
    let mut launch = command();
    launch.args(server_args(&db.path, &run.path, port));
    let mut child = match launch.spawn() {
        Ok(child) => child,
        Err(error) => {
            clear_intent(state, &run)?;
            return Err(io_error(error));
        }
    };
    let pid = i32::try_from(child.id()).map_err(|_| unavailable("Dolt PID is out of range."))?;
    let handle = pidfd(pid)?;
    let ready = (|| -> Result<Value, HiveError> {
        let proc = proc_dir(pid);
        let stat = fs::read_to_string(proc.join("stat")).map_err(io_error)?;
        let (_, start_ticks) = process_start(&stat)
            .ok_or_else(|| unverified("Could not read the launched process start identity."))?;
        let server = Server {
            version: 2,
            workspace_id: id.to_owned(),
            workspace_root: root.to_owned(),
            pid,
            boot_id: boot()?,
            start_ticks,
            nonce,
            endpoint: Loopback {
                port,
                database: DATABASE.to_owned(),
            },
        };
        let deadline = Instant::now() + WAIT;
        loop {
            if exited(&handle, Duration::ZERO)? {
                return Err(unavailable("Dolt exited before becoming ready."));
            }
            if owns_listener(&proc, port)? {
                break;
            }
            if Instant::now() >= deadline {
                return Err(unavailable(
                    "Dolt did not acquire the requested loopback listener.",
                ));
            }
            thread::sleep(RETRY);
        }
        let identity = verify(&server, state, &run, noms, &handle)?;
        let bytes = serde_json::to_vec(&server)
            .map_err(|_| unavailable("Could not encode the server record."))?;
        write_new(&run, "server.json", &bytes)?;
        // A hard link publishes the complete record without replacing a
        // competing file. The intent remains until publication is synced.
        fs::hard_link(at(&run, "server.json"), at(state, "server.json")).map_err(io_error)?;
        sync(state)?;
        fs::remove_file(at(&run, "server.json")).map_err(io_error)?;
        fs::remove_file(at(state, "starting")).map_err(io_error)?;
        sync(state)?;
        Ok(json!({
            "state": "running",
            "workspace_id": id,
            "workspace_root": root,
            "database": DATABASE,
            "port": port,
            "reader_user": reader_name(&server.nonce),
            "reader_password_file": json_path(&run.path.join("reader-password"))?,
            "committed_identity": identity,
            "verification": {
                "active_root": "matched",
                "process": "pid_start_verified",
                "storage": "selected_store_lock_verified",
                "endpoint": "owned_loopback_listener_verified"
            }
        }))
    })();
    if ready.is_err()
        && stop_child(&handle, &mut child).is_ok()
        && !state.contains(c"server.json")?
    {
        clear_intent(state, &run)?;
    }
    ready
}

fn stop(
    state: &ScopedDirectory,
    noms: &ScopedDirectory,
    server: &Server,
) -> Result<Value, HiveError> {
    if server.endpoint.database != DATABASE {
        return Err(unverified(
            "The recorded database is outside the supported workspace layout.",
        ));
    }
    let run = child_dir(state, &format!("run-{}", server.nonce))?;
    require_private_dir(&run)?;
    // Acquire a kernel process handle before checking mutable procfs entries.
    let handle = pidfd(server.pid)?;
    verify(server, state, &run, noms, &handle)?;
    send_signal(&handle, libc::SIGTERM)?;
    if !exited(&handle, WAIT)? {
        return Err(unavailable(
            "Dolt exit was not confirmed; runtime state was preserved.",
        ));
    }
    fs::remove_file(at(state, "server.json")).map_err(io_error)?;
    sync(state)?;
    remove_run(&run)?;
    Ok(json!({
        "state": "stopped",
        "workspace_id": server.workspace_id,
        "workspace_root": server.workspace_root,
        "process_exit": "pidfd_verified",
        "pending_request": if state.contains(c"pending-request.json")? { "recorded" } else { "unrecorded" },
    }))
}

/// Hold the existing lifecycle lock and process handle around one authenticated read.
pub(crate) fn with_owned_snapshot<T>(
    workspace: &Candidate,
    target: DoltTarget<'_>,
    read: impl FnOnce(&mut Snapshot) -> Result<T, HiveError>,
) -> Result<T, HiveError> {
    let id = workspace.workspace_id();
    let root = json_path(&workspace.root.path)?;
    let state = find_state(id, &workspace.root.path)?
        .ok_or_else(|| unverified("No active-root registration exists for this workspace."))?;
    require_private_dir(&state)?;
    require_local_filesystem(&state)?;
    if !state.contains(c"lock")? {
        return Err(unverified(
            "The existing workspace lifecycle lock is unavailable.",
        ));
    }
    let lock = lock_file(regular(&state, "lock")?, libc::LOCK_SH)?;
    let check_selection = || -> Result<(), HiveError> {
        let selected = read_explicit(&workspace.root.path)?;
        let lock_now = regular(&state, "lock")?.metadata().map_err(io_error)?;
        let held = lock.metadata().map_err(io_error)?;
        if (held.dev(), held.ino()) != (lock_now.dev(), lock_now.ino())
            || workspace.root.file_identity()? != selected.root.file_identity()?
            || id != selected.workspace_id()
            || workspace.policy_revision() != selected.policy_revision()
            || find_state(id, &workspace.root.path)?
                .ok_or_else(|| unverified("The workspace state directory disappeared."))?
                .file_identity()?
                != state.file_identity()?
        {
            return Err(unverified("The selected workspace or its state changed."));
        }
        let registration: Registration = read_json(Some(&state), "active-root.json")?
            .ok_or_else(|| unverified("No active-root registration exists for this workspace."))?;
        check_identity(
            registration.version,
            &registration.workspace_id,
            &registration.workspace_root,
            id,
            root,
        )?;
        if state.contains(c"starting")? || state.contains(c"pending-request.json")? {
            return Err(unverified(
                "The workspace has an unresolved runtime or write intent.",
            ));
        }
        Ok(())
    };
    check_selection()?;
    let server: Server = read_json(Some(&state), "server.json")?
        .ok_or_else(|| unverified("No owned server is recorded."))?;
    check_server(&server, id, root)?;
    if target.workspace_id != id
        || target.database != DATABASE
        || server.endpoint.database != target.database
        || server.endpoint.port != target.port
    {
        return Err(unverified(
            "The explicit target differs from the owned Dolt endpoint.",
        ));
    }
    let db = child_dir(&workspace.root, DATABASE)?;
    let noms = child_dir(&child_dir(&db, ".dolt")?, "noms")?;
    require_local_filesystem(&noms)?;
    drop(regular(&noms, "manifest")?);
    let run = child_dir(&state, &format!("run-{}", server.nonce))?;
    require_private_dir(&run)?;
    let handle = pidfd(server.pid)?;
    let result = verify_with(&server, &state, &run, &noms, &handle, read);
    check_selection()?;
    result
}

fn verify(
    server: &Server,
    state: &ScopedDirectory,
    run: &ScopedDirectory,
    noms: &ScopedDirectory,
    handle: &OwnedFd,
) -> Result<Value, HiveError> {
    verify_with(server, state, run, noms, handle, |snapshot| {
        Ok(snapshot.runtime_identity())
    })
}

fn verify_with<T>(
    server: &Server,
    state: &ScopedDirectory,
    run: &ScopedDirectory,
    noms: &ScopedDirectory,
    handle: &OwnedFd,
    read: impl FnOnce(&mut Snapshot) -> Result<T, HiveError>,
) -> Result<T, HiveError> {
    let proc = proc_dir(server.pid);
    let root = Path::new(&server.workspace_root).join(DATABASE);
    let check = || -> Result<(), HiveError> {
        verify_pidfd(handle, server.pid)?;
        if exited(handle, Duration::ZERO)? || process_evidence(server)? != "pid_start_verified" {
            return Err(unverified(
                "The recorded Dolt process identity is unavailable.",
            ));
        }
        let cwd = fs::metadata(proc.join("cwd")).map_err(io_error)?;
        let exe = fs::metadata(proc.join("exe")).map_err(io_error)?;
        let binary_id = read_private(run, "binary-id")?;
        let cmdline = fs::read(proc.join("cmdline")).map_err(io_error)?;
        let args = server_args(&root, &run.path, server.endpoint.port);
        for dir in [state, run, noms] {
            require_current_directory(dir)?;
        }
        let actual = cmdline.strip_suffix(&[0]).unwrap_or(&cmdline);
        if (cwd.dev(), cwd.ino()) != run.file_identity()?
            || binary_id != format!("{}:{}", exe.dev(), exe.ino())
            || !actual
                .split(|byte| *byte == 0)
                .skip(1)
                .eq(args.iter().map(|arg| arg.as_bytes()))
        {
            return Err(unverified(
                "Dolt executable, launch arguments, or canonical directories changed.",
            ));
        }
        if !owns_store(&proc, noms, server.pid)? {
            return Err(unverified(
                "Dolt does not hold the selected store's exclusive lock.",
            ));
        }
        if !owns_listener(&proc, server.endpoint.port)? {
            return Err(unverified("Dolt does not own the exact loopback listener."));
        }
        Ok(())
    };
    check()?;
    let password = read_private(run, "reader-password")?;
    if !is_uuid(&password) {
        return Err(unverified("The private launch credential is invalid."));
    }
    let target = DoltTarget {
        workspace_id: &server.workspace_id,
        database: DATABASE,
        port: server.endpoint.port,
    };
    let mut snapshot = Snapshot::open_runtime(target, &reader_name(&server.nonce), &password)?;
    check()?;
    let result = read(&mut snapshot);
    check()?;
    result
}

fn owns_store(proc: &Path, noms: &ScopedDirectory, pid: i32) -> Result<bool, HiveError> {
    let store_lock = regular(noms, "LOCK")?.metadata().map_err(io_error)?;
    for entry in fs::read_dir(proc.join("fd")).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let Ok(file) = fs::metadata(entry.path()) else {
            continue;
        };
        if (file.dev(), file.ino()) != (store_lock.dev(), store_lock.ino()) {
            continue;
        }
        let info =
            fs::read_to_string(proc.join("fdinfo").join(entry.file_name())).map_err(io_error)?;
        let pid = pid.to_string();
        if info.lines().filter_map(|line| line.strip_prefix("lock:")).any(|lock| {
            let fields: Vec<_> = lock.split_ascii_whitespace().collect();
            matches!(fields.get(1..5), Some(["FLOCK", "ADVISORY", "WRITE", owner]) if *owner == pid)
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn owns_listener(proc: &Path, port: NonZeroU16) -> Result<bool, HiveError> {
    let ours = fs::metadata("/proc/self/ns/net").map_err(io_error)?;
    let theirs = fs::metadata(proc.join("ns/net")).map_err(io_error)?;
    if (ours.dev(), ours.ino()) != (theirs.dev(), theirs.ino()) {
        return Ok(false);
    }
    let sockets: Vec<_> = fs::read_dir(proc.join("fd"))
        .map_err(io_error)?
        .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
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
        let table = fs::read_to_string(proc.join("net").join(protocol)).map_err(io_error)?;
        for line in table.lines().skip(1) {
            let fields: Vec<_> = line.split_ascii_whitespace().collect();
            if fields.len() >= 10
                && fields[3] == "0A"
                && sockets.iter().any(|inode| inode == fields[9])
            {
                listeners.push(fields[1].to_owned());
            }
        }
    }
    let unix = fs::read_to_string(proc.join("net/unix")).map_err(io_error)?;
    let unix_listener = unix.lines().skip(1).any(|line| {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        fields.len() >= 7
            && fields[3] == "00010000"
            && sockets.iter().any(|inode| inode == fields[6])
    });
    Ok(!unix_listener && listeners == [format!("0100007F:{:04X}", port.get())])
}

fn pidfd(pid: i32) -> Result<OwnedFd, HiveError> {
    // SAFETY: pidfd_open takes a PID and flags only.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(unavailable("Could not acquire a Linux process handle."));
    }
    // SAFETY: successful pidfd_open returned a new owned descriptor.
    let handle = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    verify_pidfd(&handle, pid)?;
    Ok(handle)
}

fn verify_pidfd(handle: &OwnedFd, pid: i32) -> Result<(), HiveError> {
    // The kernel translates Pid to this procfs mount's PID namespace.
    // Match that view before trusting evidence from /proc/<pid>.
    let info = fs::read_to_string(format!("/proc/self/fdinfo/{}", handle.as_raw_fd()))
        .map_err(|_| unverified("The pidfd's procfs identity is unavailable."))?;
    if !pidfd_matches(&info, pid) {
        return Err(unverified(
            "The pidfd does not identify the selected PID in procfs.",
        ));
    }
    Ok(())
}

fn pidfd_matches(info: &str, pid: i32) -> bool {
    let mut fields = info.lines().filter_map(|line| line.strip_prefix("Pid:"));
    pid > 0
        && fields
            .next()
            .and_then(|value| value.trim().parse::<i32>().ok())
            == Some(pid)
        && fields.next().is_none()
}

fn send_signal(handle: &OwnedFd, signal: i32) -> Result<(), HiveError> {
    // SAFETY: the descriptor remains open and the optional siginfo pointer is null.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            handle.as_raw_fd(),
            signal,
            0usize,
            0,
        )
    };
    if result != 0 {
        return Err(unavailable(
            "Could not signal the owned Linux process handle.",
        ));
    }
    Ok(())
}

fn exited(handle: &OwnedFd, timeout: Duration) -> Result<bool, HiveError> {
    let mut fd = libc::pollfd {
        fd: handle.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll receives one initialized descriptor held alive by handle.
    let result = unsafe { libc::poll(&mut fd, 1, timeout.as_millis() as i32) };
    if result < 0 || fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(unavailable("Could not observe Linux process exit."));
    }
    Ok(fd.revents & libc::POLLIN != 0)
}

fn stop_child(handle: &OwnedFd, child: &mut Child) -> Result<(), HiveError> {
    if !exited(handle, Duration::ZERO)? {
        send_signal(handle, libc::SIGTERM)?;
        if !exited(handle, WAIT)? {
            return Err(unavailable("Dolt exit was not confirmed."));
        }
    }
    child.wait().map_err(io_error)?;
    Ok(())
}

fn proc_dir(pid: i32) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}"))
}

fn boot() -> Result<String, HiveError> {
    read_uuid("/proc/sys/kernel/random/boot_id")
}

fn token() -> Result<String, HiveError> {
    read_uuid("/proc/sys/kernel/random/uuid")
}

fn read_uuid(path: &str) -> Result<String, HiveError> {
    let value = fs::read_to_string(path).map_err(io_error)?;
    let value = value.trim_end();
    if !is_uuid(value) {
        return Err(unavailable("Linux did not provide a canonical UUID."));
    }
    Ok(value.to_owned())
}

fn credential_name(role: &str, nonce: &str) -> String {
    let compact = nonce.replace('-', "");
    format!("{role}_{}", &compact[..24])
}

fn reader_name(nonce: &str) -> String {
    credential_name("reader", nonce)
}

fn path_flag(flag: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(flag);
    arg.push(path);
    arg
}

fn server_args(db: &Path, run: &Path, port: NonZeroU16) -> Vec<OsString> {
    vec![
        "sql-server".into(),
        "--skip-root-user-initialization".into(),
        "--host=127.0.0.1".into(),
        format!("--port={port}").into(),
        path_flag("--data-dir=", db),
        path_flag("--doltcfg-dir=", run),
        path_flag("--privilege-file=", &run.join("privileges.db")),
        "--event-scheduler=OFF".into(),
        "--loglevel=error".into(),
    ]
}

fn dolt_command(executable: &Path, directory: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(directory)
        .env_clear()
        .env("HOME", directory)
        .env("DOLT_ROOT_PATH", directory)
        .env("DOLT_DISABLE_VERSION_CHECKS", "true")
        .env("DOLT_DISABLE_EVENT_FLUSH", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: umask is async-signal-safe and touches only the child process.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o077);
            Ok(())
        });
    }
    command
}

fn run_checked(command: &mut Command, input: Option<&[u8]>) -> Result<Vec<u8>, HiveError> {
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().map_err(io_error)?;
    let deadline = Instant::now() + WAIT;
    let result = (|| -> Result<Vec<u8>, HiveError> {
        if let Some(input) = input {
            child
                .stdin
                .take()
                .ok_or_else(|| unavailable("Dolt stdin is unavailable."))?
                .write_all(input)
                .map_err(io_error)?;
        }
        loop {
            if let Some(exit) = child.try_wait().map_err(io_error)? {
                if !exit.success() {
                    return Err(unavailable(
                        "Dolt rejected an offline runtime preparation step.",
                    ));
                }
                let mut output = Vec::new();
                if let Some(stdout) = child.stdout.take() {
                    stdout
                        .take(1024)
                        .read_to_end(&mut output)
                        .map_err(io_error)?;
                }
                return Ok(output);
            }
            if Instant::now() >= deadline {
                return Err(unavailable("Offline Dolt runtime preparation timed out."));
            }
            thread::sleep(RETRY);
        }
    })();
    if result.is_err() {
        // This child was spawned here and has not been handed to another caller.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn lock(state: &ScopedDirectory) -> Result<File, HiveError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(at(state, "lock"))
        .map_err(io_error)?;
    lock_file(file, libc::LOCK_EX)
}

fn lock_file(file: File, mode: libc::c_int) -> Result<File, HiveError> {
    require_private_file(&file)?;
    // SAFETY: the descriptor belongs to file and remains open while locked.
    if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::WouldBlock {
            Err(HiveError::new(
                ErrorCode::LockBusy,
                "The workspace lifecycle lock is busy.",
            ))
        } else {
            Err(io_error(error))
        };
    }
    Ok(file)
}

fn at(directory: &ScopedDirectory, name: &str) -> PathBuf {
    PathBuf::from(format!(
        "/proc/self/fd/{}",
        directory.directory_file().as_raw_fd()
    ))
    .join(name)
}

fn child_dir(directory: &ScopedDirectory, name: &str) -> Result<ScopedDirectory, HiveError> {
    directory
        .open_child_directory(OsStr::new(name))?
        .ok_or_else(|| unverified("A required workspace or runtime directory is missing."))
}

fn create_run(state: &ScopedDirectory, nonce: &str) -> Result<ScopedDirectory, HiveError> {
    let name = format!("run-{nonce}");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(at(state, &name))
        .map_err(io_error)?;
    child_dir(state, &name)
}

fn regular(directory: &ScopedDirectory, name: &str) -> Result<File, HiveError> {
    crate::config_file::open_regular_child(directory.directory_file(), OsStr::new(name))?
        .ok_or_else(|| unverified("A required workspace or runtime file is missing."))
}

fn read_private(directory: &ScopedDirectory, name: &str) -> Result<String, HiveError> {
    let file = regular(directory, name)?;
    require_private_file(&file)?;
    let mut value = String::new();
    file.take(129)
        .read_to_string(&mut value)
        .map_err(io_error)?;
    if value.len() > 128 {
        return Err(unverified(
            "A private runtime record exceeds its size limit.",
        ));
    }
    Ok(value)
}

fn write_new(directory: &ScopedDirectory, name: &str, bytes: &[u8]) -> Result<(), HiveError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(at(directory, name))
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn sync(directory: &ScopedDirectory) -> Result<(), HiveError> {
    directory.directory_file().sync_all().map_err(io_error)
}

fn remove_run(run: &ScopedDirectory) -> Result<(), HiveError> {
    // Only remove the exact child we opened beneath the private state directory.
    require_current_directory(run)?;
    fs::remove_dir_all(&run.path).map_err(io_error)
}

fn require_current_directory(directory: &ScopedDirectory) -> Result<(), HiveError> {
    let current = ScopedDirectory::open_absolute(&directory.path)
        .map_err(|_| unverified("A canonical runtime directory is unavailable."))?;
    if current.file_identity()? != directory.file_identity()? {
        return Err(unverified("A canonical runtime directory changed."));
    }
    Ok(())
}

fn record_launch_intent(
    state: &ScopedDirectory,
    run: &ScopedDirectory,
    db: &ScopedDirectory,
    nonce: &str,
) -> Result<(), HiveError> {
    for directory in [state, run, db] {
        require_current_directory(directory)?;
    }
    // A killed CLI must leave durable evidence before it can spawn Dolt.
    write_new(state, "starting", nonce.as_bytes())?;
    sync(state)
}

fn clear_intent(state: &ScopedDirectory, run: &ScopedDirectory) -> Result<(), HiveError> {
    remove_run(run)?;
    fs::remove_file(at(state, "starting")).map_err(io_error)?;
    sync(state)
}

fn require_private_dir(dir: &ScopedDirectory) -> Result<(), HiveError> {
    let file = dir.directory_file().metadata().map_err(io_error)?;
    // SAFETY: geteuid takes no arguments.
    if file.uid() != unsafe { libc::geteuid() } || file.mode() & 0o777 != 0o700 {
        return Err(unverified(
            "Runtime state requires an owned directory with mode 0700.",
        ));
    }
    Ok(())
}

pub(crate) fn require_local_filesystem(dir: &ScopedDirectory) -> Result<(), HiveError> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the directory descriptor and output buffer remain valid.
    if unsafe { libc::fstatfs(dir.directory_file().as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    // SAFETY: successful fstatfs initialized the buffer.
    let kind = unsafe { stat.assume_init() }.f_type;
    if !matches!(kind, libc::EXT4_SUPER_MAGIC | libc::TMPFS_MAGIC) {
        return Err(unverified(
            "Workspace lifecycle requires an ext-family filesystem or tmpfs.",
        ));
    }
    Ok(())
}

fn require_private_file(file: &File) -> Result<(), HiveError> {
    let meta = file.metadata().map_err(io_error)?;
    // SAFETY: geteuid takes no arguments.
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o777 != 0o600
        || meta.nlink() != 1
    {
        return Err(unverified(
            "Runtime state requires a private, unshared file with mode 0600.",
        ));
    }
    Ok(())
}

fn unverified(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::AuthorityUnverified, message)
}

fn unavailable(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::RuntimeUnavailable, message)
}

fn io_error(error: io::Error) -> HiveError {
    HiveError::new(
        ErrorCode::IoError,
        format!("Workspace runtime I/O failed ({:?}).", error.kind()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pidfd_requires_one_matching_positive_procfs_pid() {
        assert!(pidfd_matches("pos:\t0\nPid:\t123\nNSpid:\t123\t45\n", 123));
        for info in [
            "Pid:\t45\nNSpid:\t45\t123\n",
            "NSpid:\t123\n",
            "Pid:\t0\n",
            "Pid:\t-1\n",
            "Pid:\tabc\n",
            "Pid:\t123\nPid:\t123\n",
        ] {
            assert!(!pidfd_matches(info, 123), "{info:?}");
        }
        assert!(!pidfd_matches("Pid:\t0\n", 0));
    }

    #[test]
    fn replaced_database_directory_refuses_launch_before_recording_intent() {
        let nonce = token().unwrap();
        let root = std::env::temp_dir().join(format!("hive-runtime-{nonce}"));
        let db_path = root.join("db");
        let state_path = root.join("state");
        fs::create_dir_all(&db_path).unwrap();
        fs::create_dir(&state_path).unwrap();
        let db = ScopedDirectory::open_absolute(&db_path).unwrap();
        let state = ScopedDirectory::open_absolute(&state_path).unwrap();
        let run = create_run(&state, &nonce).unwrap();

        fs::rename(&db_path, root.join("moved")).unwrap();
        fs::create_dir(&db_path).unwrap();
        let refused = record_launch_intent(&state, &run, &db, &nonce);
        let marker_exists = state.contains(c"starting").unwrap();

        fs::remove_dir(&db_path).unwrap();
        fs::rename(root.join("moved"), &db_path).unwrap();
        let accepted = record_launch_intent(&state, &run, &db, &nonce);
        let intent = state.read(OsStr::new("starting")).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert!(matches!(
            refused.unwrap_err().code,
            ErrorCode::AuthorityUnverified
        ));
        assert!(!marker_exists);
        accepted.unwrap();
        assert_eq!(intent.as_deref(), Some(nonce.as_bytes()));
    }
}

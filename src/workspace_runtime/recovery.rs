use super::*;
use std::os::unix::fs::PermissionsExt;

use crate::dolt::issue_note::{Pending, uncertain};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) const PROOF: &str = "stopped-request.json";
const MAX_PROOF_BYTES: usize = 64 * 1024;
// Dolt 2.7.6 can publish roots through the chunk journal without replacing
// the manifest. Bind both the stable manifest and the mutable journal files.
const JOURNAL: &str = "vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv";
type FileId = (u64, u64);

pub(super) struct Store {
    root: FileId,
    pub(super) db: ScopedDirectory,
    pub(super) dolt: ScopedDirectory,
    pub(super) noms: ScopedDirectory,
}

impl Store {
    pub(super) fn open(root: &ScopedDirectory) -> Result<Self, HiveError> {
        let db = child_dir(root, DATABASE)?;
        let dolt = child_dir(&db, ".dolt")?;
        let noms = child_dir(&dolt, "noms")?;
        require_local_filesystem(&noms)?;
        drop(regular(&noms, "manifest")?);
        Ok(Self {
            root: root.file_identity()?,
            db,
            dolt,
            noms,
        })
    }

    fn lock(&self, file: &File) -> Result<(), HiveError> {
        // SAFETY: file stays open throughout the operation, retaining the store lock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
            || !same_file(file, &regular(&self.noms, "LOCK")?)?
        {
            return Err(uncertain());
        }
        Ok(())
    }

    fn identity(&self, lock: &File) -> Result<StoreIdentity, HiveError> {
        for directory in [&self.db, &self.dolt, &self.noms] {
            require_current_directory(directory)?;
        }
        if !same_file(lock, &regular(&self.noms, "LOCK")?)? {
            return Err(uncertain());
        }
        Ok(StoreIdentity {
            root: self.root,
            db: self.db.file_identity()?,
            dolt: self.dolt.file_identity()?,
            noms: self.noms.file_identity()?,
            lock: file_id(lock)?,
            manifest: stamp(&regular(&self.noms, "manifest")?)?,
            journal: optional_stamp(&self.noms, JOURNAL)?,
            journal_index: optional_stamp(&self.noms, "journal.idx")?,
            repo_state: stamp(&regular(&self.dolt, "repo_state.json")?)?,
        })
    }

    fn copy_to(&self, target: &Path) -> Result<(), HiveError> {
        let dest = target.join(".dolt");
        fs::create_dir_all(dest.join("noms")).map_err(io_error)?;
        fs::write(dest.join("config.json"), b"{}").map_err(io_error)?;
        copy_file(
            regular(&self.dolt, "repo_state.json")?,
            &dest.join("repo_state.json"),
        )?;
        copy_noms(&self.noms, &dest.join("noms"), true)
    }
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoreIdentity {
    root: FileId,
    db: FileId,
    dolt: FileId,
    noms: FileId,
    lock: FileId,
    manifest: FileStamp,
    journal: Option<FileStamp>,
    journal_index: Option<FileStamp>,
    repo_state: FileStamp,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FileStamp {
    id: FileId,
    changed: (i64, i64),
    bytes: u64,
}

fn file_id(file: &File) -> Result<FileId, HiveError> {
    let meta = file.metadata().map_err(io_error)?;
    Ok((meta.dev(), meta.ino()))
}

fn stamp(file: &File) -> Result<FileStamp, HiveError> {
    let meta = file.metadata().map_err(io_error)?;
    Ok(FileStamp {
        id: (meta.dev(), meta.ino()),
        changed: (meta.ctime(), meta.ctime_nsec()),
        bytes: meta.len(),
    })
}

fn optional_stamp(dir: &ScopedDirectory, name: &str) -> Result<Option<FileStamp>, HiveError> {
    dir.open_regular(OsStr::new(name))?
        .as_ref()
        .map(stamp)
        .transpose()
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    file: FileStamp,
    sha256: String,
}

impl Fingerprint {
    fn pending(entry: &pending::PendingEntry) -> Result<Self, HiveError> {
        Ok(Self {
            file: stamp(&entry.file)?,
            sha256: format!("{:x}", Sha256::digest(&entry.bytes)),
        })
    }

    fn engine(file: &File) -> Result<Self, HiveError> {
        let metadata = stamp(file)?;
        let mut digest = Sha256::new();
        io::copy(&mut &*file, &mut digest).map_err(io_error)?;
        if metadata != stamp(file)? {
            return Err(uncertain());
        }
        Ok(Self {
            file: metadata,
            sha256: format!("{:x}", digest.finalize()),
        })
    }
}

pub(super) struct StopCapture {
    marker: Fingerprint,
    engine: Fingerprint,
    store_lock: File,
    run: FileId,
}

/// Record identity without interpreting the request. Only the typed recovery
/// check can establish that the marker belongs to this launch and clear it.
pub(super) fn capture(
    state: &ScopedDirectory,
    store: &Store,
    server: &Server,
    run: &ScopedDirectory,
) -> Result<Option<StopCapture>, HiveError> {
    let Ok(Some(entry)) = pending::read_pending(state) else {
        return Ok(None);
    };
    let exe = File::open(proc_dir(server.pid).join("exe")).map_err(io_error)?;
    let (dev, ino) = file_id(&exe)?;
    if read_private(run, "binary-id")? != format!("{dev}:{ino}") {
        return Err(uncertain());
    }
    Ok(Some(StopCapture {
        marker: Fingerprint::pending(&entry)?,
        engine: Fingerprint::engine(&exe)?,
        store_lock: regular(&store.noms, "LOCK")?,
        run: run.file_identity()?,
    }))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoppedRequest {
    version: u8,
    server: Server,
    state: FileId,
    lifecycle_lock: FileId,
    run: FileId,
    marker: Fingerprint,
    engine: Fingerprint,
    store: StoreIdentity,
}

impl StopCapture {
    // The caller has verified exit on the very pidfd used for the owned stop.
    pub(super) fn record_exit(
        self,
        state: &ScopedDirectory,
        store: &Store,
        server: &Server,
    ) -> Result<(), HiveError> {
        store.lock(&self.store_lock)?;
        let entry = pending::read_pending(state)?.ok_or_else(uncertain)?;
        if self.marker != Fingerprint::pending(&entry)? {
            return Err(uncertain());
        }
        let proof = StoppedRequest {
            version: 1,
            server: server.clone(),
            state: state.file_identity()?,
            lifecycle_lock: file_id(&regular(state, "lock")?)?,
            run: self.run,
            marker: self.marker,
            engine: self.engine,
            store: store.identity(&self.store_lock)?,
        };
        let bytes = serde_json::to_vec(&proof).map_err(|_| uncertain())?;
        if bytes.len() > MAX_PROOF_BYTES {
            return Err(uncertain());
        }
        let mut staged = tempfile::Builder::new()
            .prefix(".stopped-request-")
            .tempfile_in(at(state, ""))
            .map_err(io_error)?;
        require_private_file(staged.as_file())?;
        staged.write_all(&bytes).map_err(io_error)?;
        staged.as_file().sync_all().map_err(io_error)?;
        staged
            .persist(at(state, PROOF))
            .map_err(|error| io_error(error.error))?;
        sync(state)
    }
}

pub(super) fn recover(
    state: &ScopedDirectory,
    workspace_root: &ScopedDirectory,
    id: &str,
    root: &str,
    executable: &Path,
    check_selection: &dyn Fn() -> Result<(), HiveError>,
) -> Result<Value, HiveError> {
    if !executable.is_absolute() {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Specify the absolute path to the Dolt binary used by the stopped launch.",
        ));
    }
    let resolve = || -> Result<Value, HiveError> {
        require_no_launch_intent(state)?;
        let store = Store::open(workspace_root)?;
        let server: Option<Server> = read_json(Some(state), "server.json")?;
        let proof: StoppedRequest = read_proof(state)?;
        check_server(&proof.server, id, root)?;
        if proof.version != 1
            || proof.server.endpoint.database != DATABASE
            || proof.state != state.file_identity()?
            || proof.lifecycle_lock != file_id(&regular(state, "lock")?)?
            || server
                .as_ref()
                .is_some_and(|server| *server != proof.server)
        {
            return Err(uncertain());
        }
        let entry = pending::read_pending(state)?.ok_or_else(uncertain)?;
        if proof.marker != Fingerprint::pending(&entry)? {
            return Err(uncertain());
        }
        let marker = Pending::read(&entry.bytes, id, &proof.server.nonce)?;
        let store_lock = regular(&store.noms, "LOCK")?;
        store.lock(&store_lock)?;
        if proof.store != store.identity(&store_lock)? {
            return Err(uncertain());
        }

        let engine = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(executable)
            .map_err(io_error)?;
        if !engine.metadata().map_err(io_error)?.is_file()
            || proof.engine != Fingerprint::engine(&engine)?
        {
            return Err(uncertain());
        }
        // Execute the checked inode even if the supplied path is replaced.
        let binary = PathBuf::from(format!("/proc/self/fd/{}", engine.as_raw_fd()));
        let outcome = inspect_copy(state, &store, id, &binary, &marker)?;

        check_selection()?;
        require_no_launch_intent(state)?;
        let current = read_json::<Server>(Some(state), "server.json")?;
        if current != server
            || proof.store != store.identity(&store_lock)?
            || proof.engine.file != stamp(&engine)?
            || proof.marker != Fingerprint::pending(&entry)?
        {
            return Err(uncertain());
        }
        finish_stop(state, &proof, server.is_some())?;
        pending::clear(state, entry)?;
        // Retain the exit proof until marker removal is durable. An old proof
        // cannot match any other marker or launch.
        if fs::remove_file(at(state, PROOF)).is_ok() {
            let _ = sync(state);
        }
        Ok(json!({
            "state": "stopped",
            "workspace_id": id,
            "workspace_root": root,
            "process_exit": "pidfd_verified",
            "outcome": outcome,
            "pending_request": "unrecorded",
        }))
    };
    resolve().map_err(|_| uncertain())
}

fn read_proof(state: &ScopedDirectory) -> Result<StoppedRequest, HiveError> {
    let file = regular(state, PROOF)?;
    require_private_file(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_PROOF_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_PROOF_BYTES {
        return Err(uncertain());
    }
    serde_json::from_slice(&bytes).map_err(|_| uncertain())
}

fn finish_stop(
    state: &ScopedDirectory,
    proof: &StoppedRequest,
    has_server: bool,
) -> Result<(), HiveError> {
    let name = format!("run-{}", proof.server.nonce);
    if let Some(run) = state.open_child_directory(OsStr::new(&name))? {
        require_private_dir(&run)?;
        if run.file_identity()? != proof.run {
            return Err(uncertain());
        }
        remove_run(&run)?;
    }
    if has_server {
        fs::remove_file(at(state, "server.json")).map_err(io_error)?;
    }
    sync(state)
}

fn copy_noms(src: &ScopedDirectory, dest: &Path, top: bool) -> Result<(), HiveError> {
    for entry in fs::read_dir(at(src, "")).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name();
        if top && name == "oldgen" {
            let oldgen = child_dir(src, "oldgen")?;
            let target = dest.join("oldgen");
            fs::create_dir(&target).map_err(io_error)?;
            copy_noms(&oldgen, &target, false)?;
            continue;
        }
        if name == "LOCK" {
            continue;
        }
        let file = src.open_regular(&name)?.ok_or_else(uncertain)?;
        copy_file(file, &dest.join(name))?;
    }
    require_current_directory(src)
}

fn copy_file(mut file: File, target: &Path) -> Result<(), HiveError> {
    let before = stamp(&file)?;
    let mut copy = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)
        .map_err(io_error)?;
    io::copy(&mut file, &mut copy).map_err(io_error)?;
    if stamp(&file)? != before {
        return Err(uncertain());
    }
    Ok(())
}

fn inspect_copy(
    state: &ScopedDirectory,
    store: &Store,
    id: &str,
    executable: &Path,
    marker: &Pending,
) -> Result<&'static str, HiveError> {
    let scratch = tempfile::Builder::new()
        .prefix(".recovery-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(at(state, ""))
        .map_err(io_error)?;
    require_current_directory(state)?;
    let directory = state
        .path
        .join(scratch.path().file_name().ok_or_else(uncertain)?);
    let cwd = directory.as_path();
    let nonce = token()?;
    let user = reader_name(&nonce);
    let password = token()?;
    // Provision only SELECT, with no live credentials or canonical database in
    // this directory. The temporary instance also disables writes at the server.
    let mut provision = dolt_command(executable, cwd);
    provision
        .arg(path_flag("--privilege-file=", &cwd.join("privileges.db")))
        .arg("sql");
    let sql = format!(
        "CREATE USER '{user}'@'127.0.0.1' IDENTIFIED BY '{password}';
         GRANT SELECT ON db.* TO '{user}'@'127.0.0.1';
         DROP USER 'root'@'localhost';"
    );
    run_checked(&mut provision, Some(sql.as_bytes()))?;

    let db = cwd.join(DATABASE);
    store.copy_to(&db)?;
    let copy = ScopedDirectory::open_absolute(&db.join(".dolt/noms"))?;
    let available = TcpListener::bind(("127.0.0.1", 0)).map_err(io_error)?;
    let port =
        NonZeroU16::new(available.local_addr().map_err(io_error)?.port()).ok_or_else(uncertain)?;
    let mut launch = dolt_command(executable, cwd);
    // This launch can only write local config into the private scratch copy.
    launch
        .args(legacy_server_args(&db, cwd, port))
        .arg("--readonly");
    let parent = i32::try_from(std::process::id()).map_err(|_| uncertain())?;
    // SAFETY: the callback only makes async-signal-safe system calls. Check
    // parent identity after registration to cover exit just before prctl.
    unsafe {
        launch.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
    drop(available);
    let mut child = launch.spawn().map_err(io_error)?;
    let pid = i32::try_from(child.id()).map_err(|_| uncertain())?;
    let result = (|| {
        let handle = pidfd(pid)?;
        let inspect = || {
            let proc = proc_dir(pid);
            wait_for_listener(&handle, &proc, port)?;
            let check = || -> Result<(), HiveError> {
                verify_pidfd(&handle, pid)?;
                if exited(&handle, Duration::ZERO)?
                    || !owns_store(&proc, &copy, pid)?
                    || !owns_listener(&proc, port)?
                {
                    return Err(uncertain());
                }
                Ok(())
            };
            check()?;
            let target = DoltTarget {
                workspace_id: id,
                database: DATABASE,
                port,
            };
            let mut snapshot = Snapshot::open_runtime(target, &user, &password)?;
            let outcome = marker.resolve(&mut snapshot)?;
            check()?;
            Ok(outcome)
        };
        let outcome = inspect();
        stop_child(&handle, &mut child)?;
        outcome
    })();
    if result.is_err() && child.try_wait().map_err(io_error)?.is_none() {
        // This is our own unregistered child and can only see the private copy.
        let _ = child.kill();
        child.wait().map_err(io_error)?;
    }
    scratch.close().map_err(io_error)?;
    result
}

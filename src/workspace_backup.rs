use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config_file::ScopedDirectory;
use crate::config_git::with_asset_bundle;
use crate::config_v2::Candidate;
use crate::dolt::{DOLT_VERSION, INITIAL_SCHEMA, STORAGE_SCHEMA_VERSION, is_commit_hash, is_uuid};
use crate::error::{ErrorCode, HiveError};
use crate::workspace_init::{Installed, fd_path, sync_tree};
use crate::workspace_status::require_local_filesystem;

const MAX_FILES: usize = 2048;
const MAX_MANIFEST_BYTES: usize = 512 * 1024;
const MAX_SERVER_CONFIG_BYTES: usize = 64 * 1024;

#[derive(Serialize, Eq, PartialEq)]
struct DigestEntry {
    path: String,
    bytes: u64,
    sha256: String,
}

pub(crate) fn create(
    workspace: &Candidate,
    state: &ScopedDirectory,
    db: &ScopedDirectory,
    native: &ScopedDirectory,
    noms: &ScopedDirectory,
    destination: &Path,
    check_stopped: impl Fn() -> Result<(), HiveError>,
) -> Result<Value, HiveError> {
    let invalid_destination = || {
        HiveError::new(
            ErrorCode::InvalidArgument,
            "Select a new backup directory outside the workspace and its runtime state.",
        )
    };
    let destination = if destination.is_absolute() {
        destination.to_owned()
    } else {
        std::env::current_dir().map_err(io_error)?.join(destination)
    };
    if destination
        .components()
        .any(|part| part == Component::ParentDir)
    {
        return Err(invalid_destination());
    }
    let name = destination.file_name().ok_or_else(invalid_destination)?;
    let parent =
        ScopedDirectory::open_absolute(destination.parent().ok_or_else(invalid_destination)?)?;
    if parent.path.starts_with(&workspace.root.path) || parent.path.starts_with(&state.path) {
        return Err(invalid_destination());
    }
    let destination_path = crate::config::json_path(&parent.path.join(name))?.to_owned();
    require_local_filesystem(&parent)?;
    let parent_fd = fd_path(parent.directory_file());
    match fs::symlink_metadata(parent_fd.join(name)) {
        Ok(_) => return Err(destination_exists()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    let stage = tempfile::Builder::new()
        .prefix(".hive-backup-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(&parent_fd)
        .map_err(io_error)?;
    let stage_name = stage.path().file_name().expect("staging child name");
    let stage_dir = parent.require_child_directory(stage_name)?;
    let stage_fd = fd_path(stage_dir.directory_file());
    let scratch = stage_fd.join("scratch");
    let payload = stage_fd.join("snapshot");
    for dir in [&scratch, &payload, &payload.join("dolt")] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .map_err(io_error)?;
    }
    let scratch_dir = stage_dir.require_child_directory(OsStr::new("scratch"))?;
    let payload_dir = stage_dir.require_child_directory(OsStr::new("snapshot"))?;
    let installed = Installed::check(scratch_dir.directory_file())?;
    // Native verification restores into a new repo and needs an identity for
    // its temporary initialization. The published store keeps source history.
    for (key, value) in [("user.name", "Hive"), ("user.email", "backup@hive.invalid")] {
        installed.dolt(&scratch, &["config", "--global", "--add", key, value], None)?;
    }
    let source = fd_path(db.directory_file());
    // The parent process owns these descriptors for the duration of all native
    // commands. The URL contains fixed ASCII components even for unusual paths.
    let process_fd =
        |dir: &ScopedDirectory| format!("/proc/{}/fd/{}", std::process::id(), dir.as_raw_fd());
    let backup_url = format!("file://{}/dolt", process_fd(&payload_dir));
    let verify_source = || {
        check_stopped()?;
        verify_native_layout(state, db, native, noms)?;
        verify_db(&installed, &source, workspace.workspace_id())
    };
    let revision = verify_source()?;
    let bundle = Path::new(&process_fd(&payload_dir)).join("assets.bundle");
    with_asset_bundle(
        &workspace.root,
        Path::new(&process_fd(&scratch_dir)),
        &bundle,
        |asset_revision, recheck_assets| {
            // The source cwd is pinned by the parent before exec.
            installed.dolt(&source, &["backup", "sync-url", &backup_url], None)?;
            installed.dolt(&scratch, &["backup", "restore", &backup_url, "db"], None)?;
            let restored_db = scratch.join("db");
            installed.dolt(&restored_db, &["fsck", "--quiet"], None)?;
            let restored = verify_db(&installed, &restored_db, workspace.workspace_id())?;
            if restored != revision {
                return Err(unverified(
                    "The staged Dolt backup differs from the selected committed revision.",
                ));
            }
            // This is an empty lock file created by the local native transport.
            for lock in ["dolt/LOCK", "dolt/oldgen/LOCK"] {
                let lock = payload.join(lock);
                match fs::symlink_metadata(&lock) {
                    Ok(meta) if meta.is_file() && meta.nlink() == 1 && meta.len() == 0 => {
                        fs::remove_file(lock).map_err(io_error)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    _ => {
                        return Err(unverified(
                            "The native backup produced an unexpected lock file.",
                        ));
                    }
                }
            }
            fs::set_permissions(&bundle, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
            let files = digest_payload(&payload_dir)?;
            let manifest = json!({
                "format_version": 1,
                "workspace_id": workspace.workspace_id(),
                "storage_schema_version": STORAGE_SCHEMA_VERSION,
                "dolt_version": DOLT_VERSION,
                "dolt_commit": revision,
                "asset_git_commit": asset_revision,
                "policy_revision": workspace.policy_revision(),
                "created_at_unix_seconds": SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
                    unverified("The system time cannot be represented in the backup manifest.")
                })?.as_secs(),
                "consistency_state": "verified_stopped_committed",
                "conversations_included": false,
                "files": files,
            });
            let bytes = serde_json::to_vec_pretty(&manifest)
                .map_err(|_| unverified("Could not encode the backup manifest."))?;
            if bytes.len() > MAX_MANIFEST_BYTES {
                return Err(unverified("The backup manifest exceeds its size limit."));
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(payload.join("manifest.json"))
                .map_err(io_error)?;
            file.write_all(&bytes).map_err(io_error)?;
            sync_tree(payload_dir.directory_file())?;
            if payload_dir.read_bounded(&[OsStr::new("manifest.json")], MAX_MANIFEST_BYTES)?
                != Some(bytes)
                || digest_payload(&payload_dir)? != files
                || verify_source()? != revision
            {
                return Err(HiveError::new(
                    ErrorCode::ConcurrentChange,
                    "The workspace or staged backup changed.",
                ));
            }
            recheck_assets()?;
            check_stopped()?;
            if !parent.same_directory(&ScopedDirectory::open_absolute(&parent.path)?)?
                || !stage_dir.same_directory(&parent.require_child_directory(stage_name)?)?
                || !payload_dir
                    .same_directory(&stage_dir.require_child_directory(OsStr::new("snapshot"))?)?
            {
                return Err(HiveError::new(
                    ErrorCode::ConcurrentChange,
                    "The destination parent or staging directory changed.",
                ));
            }
            rustix::fs::renameat_with(
                stage_dir.directory_file(),
                "snapshot",
                parent.directory_file(),
                name,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|error| {
                if error == rustix::io::Errno::EXIST {
                    destination_exists()
                } else {
                    io_error(error.into())
                }
            })?;
            parent.directory_file().sync_all().map_err(|_| {
                HiveError::new(
                    ErrorCode::UnknownWriteOutcome,
                    "The backup was published but destination durability could not be confirmed.",
                )
            })?;
            Ok(json!({"destination": destination_path, "manifest": manifest}))
        },
    )
}

fn verify_native_layout(
    state_dir: &ScopedDirectory,
    db: &ScopedDirectory,
    native: &ScopedDirectory,
    noms: &ScopedDirectory,
) -> Result<(), HiveError> {
    let unsupported =
        || unverified("The Dolt store layout, configuration or refs are unsupported for backup.");
    let state = native
        .read(OsStr::new("repo_state.json"))?
        .ok_or_else(unsupported)?;
    let config = native
        .read(OsStr::new("config.json"))?
        .ok_or_else(unsupported)?;
    let config: Value = serde_json::from_slice(&config).map_err(|_| unsupported())?;
    if serde_json::from_slice::<Value>(&state).ok()
        != Some(json!({"head": "refs/heads/main", "remotes": {}, "backups": {}, "branches": {}}))
        || !config.as_object().is_some_and(|config| {
            config.iter().all(|(key, value)| match key.as_str() {
                "user.name" | "user.email" => value.is_string(),
                "sqlserver.global.server_uuid" => value
                    .as_str()
                    .is_some_and(|id| is_uuid(id) && id.as_bytes()[14] == b'4'),
                _ => false,
            })
        })
    {
        return Err(unsupported());
    }
    for entry in fs::read_dir(fd_path(db.directory_file())).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        match entry.file_name().to_str() {
            Some(".dolt") if entry.file_type().map_err(io_error)?.is_dir() => {}
            Some("config.yaml") => verify_server_config(state_dir, db)?,
            Some(".doltcfg") => {
                let config = db.require_child_directory(OsStr::new(".doltcfg"))?;
                for file in fs::read_dir(fd_path(config.directory_file())).map_err(io_error)? {
                    let file = file.map_err(io_error)?;
                    if file.file_name() != "branch_control.db"
                        || !file.file_type().map_err(io_error)?.is_file()
                    {
                        return Err(unsupported());
                    }
                }
            }
            _ => return Err(unsupported()),
        }
    }
    // Reject opaque configuration, credentials, symlinks and extensions
    // before giving native Dolt the selected repository.
    for entry in fs::read_dir(fd_path(native.directory_file())).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let kind = entry.file_type().map_err(io_error)?;
        let valid = match entry.file_name().to_str() {
            Some("noms" | "temptf" | "tmp" | "stats") => kind.is_dir(),
            Some("repo_state.json" | "config.json") => kind.is_file(),
            _ => false,
        };
        if !valid {
            return Err(unsupported());
        }
    }
    let oldgen = noms.require_child_directory(OsStr::new("oldgen"))?;
    // Native sync can leave an empty tmp directory after a cross-filesystem export.
    for name in ["temptf", "tmp"] {
        if let Some(temp) = native.open_child_directory(OsStr::new(name))?
            && fs::read_dir(fd_path(temp.directory_file()))
                .map_err(io_error)?
                .next()
                .is_some()
        {
            return Err(unsupported());
        }
    }
    for directory in [noms, &oldgen] {
        require_local_filesystem(directory)?;
        for entry in fs::read_dir(fd_path(directory.directory_file())).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let meta = fs::symlink_metadata(entry.path()).map_err(io_error)?;
            if entry.file_name() == "oldgen" && std::ptr::eq(directory, noms) && meta.is_dir() {
                continue;
            }
            if !meta.is_file()
                || meta.nlink() != 1
                || !entry.file_name().to_str().is_some_and(native_filename)
            {
                return Err(unsupported());
            }
        }
    }
    Ok(())
}

fn verify_server_config(state: &ScopedDirectory, db: &ScopedDirectory) -> Result<(), HiveError> {
    let unsupported = || unverified("The Dolt server configuration is unsupported for backup.");
    let bytes = db
        .read_bounded(&[OsStr::new("config.yaml")], MAX_SERVER_CONFIG_BYTES)
        .map_err(|_| unsupported())?
        .ok_or_else(unsupported)?;
    let config: Value = crate::config_file::parse_v2(&bytes, "Dolt server configuration")
        .map_err(|_| unsupported())?;
    let run = Path::new(config["cfg_dir"].as_str().ok_or_else(unsupported)?);
    let nonce = run
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix("run-"))
        .filter(|nonce| is_uuid(nonce))
        .ok_or_else(unsupported)?;
    let expected_run = state.path.join(format!("run-{nonce}"));
    let privilege_path = expected_run.join("privileges.db");
    let expected_data = db.path.to_str().ok_or_else(unsupported)?;
    let expected_cfg = expected_run.to_str().ok_or_else(unsupported)?;
    let expected_privilege = privilege_path.to_str().ok_or_else(unsupported)?;
    let port = config["listener"]["port"]
        .as_u64()
        .filter(|port| (1024..=65535).contains(port))
        .ok_or_else(unsupported)?;
    // These paths are the previous launch's configuration values. Compare
    // them as data without opening either the run directory or privilege file.
    if config
        != json!({
            "log_level": "error",
            "behavior": {"event_scheduler": "OFF"},
            "listener": {"host": "127.0.0.1", "port": port},
            "data_dir": expected_data,
            "cfg_dir": expected_cfg,
            "privilege_file": expected_privilege,
        })
    {
        return Err(unsupported());
    }
    Ok(())
}

fn native_filename(name: &str) -> bool {
    matches!(name, "manifest" | "LOCK" | "journal.idx")
        || is_commit_hash(name.strip_suffix(".darc").unwrap_or(name))
}

fn verify_db(
    installed: &Installed<'_>,
    db: &Path,
    workspace_id: &str,
) -> Result<String, HiveError> {
    let rejected =
        || unverified("Could not verify the Dolt main revision, workspace identity or schema.");
    let head = installed.query(
        db,
        concat!(
            "SELECT ACTIVE_BRANCH() AS branch, DOLT_HASHOF('HEAD') AS revision, ",
            "DOLT_HASHOF_DB('HEAD') AS head_root, DOLT_HASHOF_DB('STAGED') AS staged_root, ",
            "DOLT_HASHOF_DB('WORKING') AS work_root"
        ),
    )?;
    let [row] = head.as_slice() else {
        return Err(rejected());
    };
    let revision = row["revision"]
        .as_str()
        .filter(|value| is_commit_hash(value))
        .ok_or_else(rejected)?;
    let root = row["head_root"]
        .as_str()
        .filter(|value| is_commit_hash(value))
        .ok_or_else(rejected)?;
    let count =
        |table: &str| installed.query(db, &format!("SELECT COUNT(*) AS count FROM {table}"));
    let empty = [json!({"count": 0})];
    if row["branch"] != "main" {
        return Err(rejected());
    }
    if row["staged_root"] != root || row["work_root"] != root || count("dolt_status")? != empty {
        return Err(HiveError::new(
            ErrorCode::DirtyConflict,
            "The Dolt database has staged or working changes.",
        ));
    }
    if installed.query(db, "SELECT is_merging FROM dolt_merge_status")?
        != [json!({"is_merging": false})]
        || count("dolt_conflicts")? != empty
        || count("dolt_constraint_violations")? != empty
    {
        return Err(unverified(
            "Dolt merges, conflicts and constraint violations require resolution before backup.",
        ));
    }
    if count("dolt_branches")? != [json!({"count": 1})]
        || count("dolt_remote_branches")? != empty
        || count("dolt_tags")? != empty
        || !installed
            .dolt(db, &["stash", "list"], None)?
            .trim()
            .is_empty()
    {
        return Err(unverified(
            "Backup supports only an unstashed Dolt main branch without tags or remote refs.",
        ));
    }
    if installed.query(
        db,
        "SELECT singleton_id, workspace_id FROM workspace_meta LIMIT 2",
    )? != [json!({"singleton_id": 1, "workspace_id": workspace_id})]
        || installed.query(
            db,
            "SELECT version, migration_digest FROM schema_migrations LIMIT 2",
        )? != [
            json!({"version": STORAGE_SCHEMA_VERSION, "migration_digest": format!("{:x}", Sha256::digest(INITIAL_SCHEMA))}),
        ]
    {
        return Err(rejected());
    }
    if count("issue_claims")? != empty {
        return Err(unverified(
            "Active claims require resolution before backup.",
        ));
    }
    installed.verify_schema(db, revision)?;
    Ok(revision.to_owned())
}

fn digest_payload(payload: &ScopedDirectory) -> Result<Vec<DigestEntry>, HiveError> {
    let unsupported = || unverified("The native backup produced an unsupported file.");
    for entry in fs::read_dir(fd_path(payload.directory_file())).map_err(io_error)? {
        if !matches!(
            entry.map_err(io_error)?.file_name().to_str(),
            Some("dolt" | "assets.bundle" | "manifest.json")
        ) {
            return Err(unsupported());
        }
    }
    let store = payload.require_child_directory(OsStr::new("dolt"))?;
    let oldgen = store.require_child_directory(OsStr::new("oldgen"))?;
    let mut files = Vec::new();
    for (prefix, directory) in [("dolt", &store), ("dolt/oldgen", &oldgen)] {
        for entry in fs::read_dir(fd_path(directory.directory_file())).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            if prefix == "dolt"
                && entry.file_name() == "oldgen"
                && entry.file_type().map_err(io_error)?.is_dir()
            {
                continue;
            }
            let name = entry.file_name();
            let name = name
                .to_str()
                .filter(|name| !matches!(*name, "LOCK" | "journal.idx") && native_filename(name))
                .ok_or_else(unsupported)?;
            if files.len() >= MAX_FILES - 1 {
                return Err(unverified("The backup contains too many files."));
            }
            files.push(digest_file(directory, name, format!("{prefix}/{name}"))?);
        }
    }
    files.push(digest_file(
        payload,
        "assets.bundle",
        "assets.bundle".to_owned(),
    )?);
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn digest_file(
    directory: &ScopedDirectory,
    name: &str,
    path: String,
) -> Result<DigestEntry, HiveError> {
    let mut file = directory
        .open_regular(OsStr::new(name))?
        .ok_or_else(|| unverified("A staged backup file disappeared."))?;
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(unverified("The backup contains an unsupported file."));
    }
    let mut hash = Sha256::new();
    let bytes = io::copy(&mut file, &mut hash).map_err(io_error)?;
    if bytes != meta.len() {
        return Err(unverified("A backup file changed while being read."));
    }
    Ok(DigestEntry {
        path,
        bytes,
        sha256: format!("{:x}", hash.finalize()),
    })
}

fn destination_exists() -> HiveError {
    HiveError::new(
        ErrorCode::AlreadyExists,
        "The selected backup destination already exists.",
    )
}

fn unverified(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::AuthorityUnverified, message)
}

fn io_error(error: io::Error) -> HiveError {
    HiveError::new(
        ErrorCode::IoError,
        format!("Could not access local backup files ({error})."),
    )
}

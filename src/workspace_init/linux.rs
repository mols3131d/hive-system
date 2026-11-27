use std::ffi::CString;
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use crate::config::{current_dir, json_path};
use crate::config_v2;
use crate::dolt::{DOLT_VERSION, STORAGE_SCHEMA_VERSION};
use crate::error::{ErrorCode, HiveError};

mod tools;

const POLICY_REVISION: &str = "c66b3c50150e18426485d134de92c6b311dac125";
const STAGE_PREFIX: &str = ".hive-init-incomplete-";

pub(super) fn run(target: &Path) -> Result<Value, HiveError> {
    let input = json_path(target)?;
    if input.trim().is_empty() || input.contains("://") || input.chars().any(char::is_control) {
        return Err(invalid_target("Supply an explicit local directory path."));
    }
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        current_dir()?.join(target)
    };
    let (parent, name, original) = inspect(&target)?;
    let workspace_id = new_uuid().map_err(|_| {
        HiveError::new(
            ErrorCode::IoError,
            "Could not generate a workspace identity.",
        )
    })?;
    let stage_name = format!("{STAGE_PREFIX}{workspace_id}");
    let stage = fd_path(&parent).join(&stage_name);
    private_dir(&stage)?;
    let scratch = stage.join("scratch");
    let payload = stage.join("workspace");
    let result: Result<_, HiveError> = (|| {
        private_dir(&scratch)?;
        // The engines need an absolute HOME that also resolves after exec.
        let home = target
            .parent()
            .expect("inspected target has a parent")
            .join(&stage_name)
            .join("scratch");
        let tools = tools::Installed::check(&scratch, &home)?;
        private_dir(&payload)?;
        for name in ["db", "assets", "conversations"] {
            private_dir(&payload.join(name))?;
        }
        let db_commit = tools.initialize_db(&payload.join("db"), &workspace_id)?;
        let asset_commit = tools.initialize_assets(&payload.join("assets"))?;
        Ok((db_commit, asset_commit))
    })();

    // Keep failed payloads in a clearly marked, private sibling. Remove
    // isolated tool state on normal exit, even when an engine fails.
    let cleanup = fs::remove_dir_all(&scratch);
    let (db_commit, asset_commit) = match result {
        Ok(commits) => {
            cleanup.map_err(|_| io_error("Could not remove isolated tool state."))?;
            commits
        }
        Err(error) => {
            if !payload.exists() && fs::remove_dir(&stage).is_ok() {
                return Err(error);
            }
            return Err(HiveError::new(
                error.code,
                format!("{error} Incomplete staging: {stage_name}."),
            ));
        }
    };

    let (current_parent, _, current) = inspect(&target)?;
    if !same_directory(&parent, &current_parent)? || !same_directory(&original, &current)? {
        return Err(HiveError::new(
            ErrorCode::ConcurrentChange,
            "The workspace target or its parent changed during initialization.",
        ));
    }

    let descriptor = format!(
        "version: 2\nworkspace_id: {workspace_id}\nschema_version: {STORAGE_SCHEMA_VERSION}\npolicy_revision: {POLICY_REVISION}\n"
    );
    let marker = payload.join(config_v2::MARKER);
    (|| {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&marker)?
            .write_all(descriptor.as_bytes())
    })()
    .map_err(|_| io_error("Could not write the staged workspace descriptor."))?;
    let bytes = fs::read(&marker)
        .map_err(|_| io_error("Could not read the staged workspace descriptor."))?;
    config_v2::verify_descriptor(&bytes)?;
    let output = json!({
        "workspace_root": json_path(&target)?,
        "workspace_id": workspace_id,
        "policy_revision": POLICY_REVISION,
        "storage_schema_version": STORAGE_SCHEMA_VERSION,
        "dolt_version": DOLT_VERSION,
        "db_commit": db_commit,
        "asset_commit": asset_commit,
    });

    let stage_fd = open_directory(&stage)?;
    // SAFETY: Both descriptors are pinned and both names are single components.
    // Linux rejects replacement if the destination acquired any entries.
    let published = unsafe {
        libc::renameat(
            stage_fd.as_raw_fd(),
            c"workspace".as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
        )
    };
    if published != 0 {
        let error = io::Error::last_os_error();
        let _ = fs::remove_file(&marker);
        let code = match error.raw_os_error() {
            Some(libc::ENOTEMPTY | libc::EEXIST | libc::ENOTDIR | libc::EISDIR) => {
                ErrorCode::ConcurrentChange
            }
            _ => ErrorCode::IoError,
        };
        return Err(HiveError::new(
            code,
            format!(
                "Could not publish the verified workspace ({error}). Incomplete staging: {stage_name}."
            ),
        ));
    }
    // Publication succeeded. Only the now-empty staging container may remain.
    let mut output = output;
    if fs::remove_dir(&stage).is_err() {
        output["staging_directory"] = json!(stage_name);
    }
    Ok(output)
}

fn inspect(target: &Path) -> Result<(File, CString, File), HiveError> {
    let name = target
        .file_name()
        .ok_or_else(|| invalid_target("The workspace target must name an empty directory."))?;
    let name = CString::new(name.as_encoded_bytes())
        .map_err(|_| invalid_target("The workspace target has an invalid component."))?;
    if target.components().any(|component| {
        component == Component::ParentDir
            || matches!(component, Component::Normal(name) if name == ".dolt" || name.as_encoded_bytes().starts_with(STAGE_PREFIX.as_bytes()))
    }) {
        return Err(invalid_target("The workspace target contains a disallowed path component."));
    }

    let mut directory = open_directory(Path::new("/"))?;
    let root = dir_metadata(&directory)?;
    require_trusted(&directory, &root, false)?;
    let parent = target.parent().expect("target has a final component");
    let mut path = PathBuf::from("/");
    for component in parent.components() {
        if let Component::Normal(name) = component {
            refuse_workspace_ancestor(&directory)?;
            directory = open_directory(&fd_path(&directory).join(name))?;
            path.push(name);
            let child = dir_metadata(&directory)?;
            // A managed Linux namespace may display a system-owned /tmp
            // mount and its root under the same unmapped UID.
            let system_tmp = path == Path::new("/tmp")
                && child.uid() == root.uid()
                && child.dev() != root.dev()
                && child.mode() & 0o1000 != 0
                && filesystem_kind(&directory)? == libc::TMPFS_MAGIC;
            require_trusted(&directory, &child, system_tmp)?;
        }
    }
    refuse_workspace_ancestor(&directory)?;
    require_local_filesystem(&directory)?;
    let original =
        open_directory(&fd_path(&directory).join(std::ffi::OsStr::from_bytes(name.to_bytes())))?;
    let child = dir_metadata(&original)?;
    if child.dev() != dir_metadata(&directory)?.dev() {
        return Err(invalid_target(
            "The workspace target must share its parent's filesystem.",
        ));
    }
    // SAFETY: geteuid takes no arguments.
    if child.uid() != unsafe { libc::geteuid() } || child.mode() & 0o022 != 0 {
        return Err(invalid_target(
            "The empty target must be owned by the invoking user and protected from other writers.",
        ));
    }
    match fs::read_dir(fd_path(&original)).and_then(|mut entries| entries.next().transpose()) {
        Ok(None) => Ok((directory, name, original)),
        Ok(Some(_)) => Err(HiveError::new(
            ErrorCode::AlreadyExists,
            "The workspace target is not empty.",
        )),
        Err(_) => Err(io_error("Could not inspect the workspace target contents.")),
    }
}

fn open_directory(path: &Path) -> Result<File, HiveError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => HiveError::new(
                ErrorCode::NotFound,
                "The target and its parent directories must already exist.",
            ),
            io::ErrorKind::NotADirectory => {
                invalid_target("The target path must contain only real directories.")
            }
            _ => io_error("Could not open a directory for workspace initialization."),
        })
}

fn fd_path(directory: &File) -> PathBuf {
    Path::new("/proc/self/fd").join(directory.as_raw_fd().to_string())
}

fn dir_metadata(directory: &File) -> Result<Metadata, HiveError> {
    directory
        .metadata()
        .map_err(|_| io_error("Could not inspect workspace directory ownership."))
}

fn same_directory(left: &File, right: &File) -> Result<bool, HiveError> {
    let left = dir_metadata(left)?;
    let right = dir_metadata(right)?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

fn require_trusted(
    directory: &File,
    metadata: &Metadata,
    system_tmp: bool,
) -> Result<(), HiveError> {
    // SAFETY: geteuid takes no arguments.
    let user = unsafe { libc::geteuid() };
    if metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0 {
        return Err(invalid_target(
            "A target path component permits untrusted writes.",
        ));
    }
    if metadata.uid() == user || metadata.uid() == 0 || system_tmp {
        return Ok(());
    }
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: The directory descriptor and output buffer are valid.
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), stat.as_mut_ptr()) } == 0
        // SAFETY: fstatvfs succeeded and initialized the buffer.
        && unsafe { stat.assume_init() }.f_flag & libc::ST_RDONLY != 0
    {
        return Ok(());
    }
    Err(invalid_target(
        "A target path component can be replaced by another user.",
    ))
}

fn refuse_workspace_ancestor(directory: &File) -> Result<(), HiveError> {
    let path = fd_path(directory);
    let has = |name| match fs::symlink_metadata(path.join(name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(io_error("Could not check the workspace target ancestry.")),
    };
    for name in ["hive.yml", config_v2::MARKER] {
        if has(name)? {
            return Err(invalid_target(
                "The workspace target is inside an existing Hive workspace.",
            ));
        }
    }
    match open_directory(&path.join(".dolt")) {
        Ok(dolt) => {
            let dolt = fd_path(&dolt);
            // Dolt also uses ~/.dolt for global settings. Either repository
            // marker is enough to refuse, including a damaged or symlinked one.
            for name in ["repo_state.json", "noms"] {
                match fs::symlink_metadata(dolt.join(name)) {
                    Ok(_) => {
                        return Err(invalid_target(
                            "The workspace target is inside an existing Dolt repository.",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(io_error("Could not inspect the Dolt repository marker."));
                    }
                }
            }
        }
        Err(error) if matches!(error.code, ErrorCode::NotFound) => {}
        Err(error) => return Err(error),
    }
    let git_marker = path.join(".git");
    let git_worktree = match fs::symlink_metadata(&git_marker) {
        // A tooling placeholder may be an empty .git directory.
        Ok(meta) if meta.is_dir() => {
            let git = open_directory(&git_marker)?;
            fs::read_dir(fd_path(&git))
                .and_then(|mut entries| entries.next().transpose())
                .map_err(|_| io_error("Could not inspect the Git marker."))?
                .is_some()
        }
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return Err(io_error("Could not inspect the Git marker.")),
    };
    if git_worktree || (has("HEAD")? && has("objects")?) {
        return Err(invalid_target(
            "The workspace target is inside an existing Git repository.",
        ));
    }
    Ok(())
}

fn require_local_filesystem(directory: &File) -> Result<(), HiveError> {
    if !matches!(
        filesystem_kind(directory)?,
        libc::EXT4_SUPER_MAGIC | libc::TMPFS_MAGIC
    ) {
        return Err(invalid_target(
            "Workspace initialization requires an ext-family filesystem or tmpfs.",
        ));
    }
    Ok(())
}

fn filesystem_kind(directory: &File) -> Result<libc::c_long, HiveError> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: The directory descriptor and output buffer are valid.
    if unsafe { libc::fstatfs(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io_error(
            "Could not determine the workspace target filesystem.",
        ));
    }
    // SAFETY: fstatfs succeeded and initialized the buffer.
    Ok(unsafe { stat.assume_init() }.f_type)
}

fn private_dir(path: &Path) -> Result<(), HiveError> {
    DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|_| io_error("Could not create a private workspace staging directory."))
}

fn new_uuid() -> io::Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = format!("{:032x}", u128::from_be_bytes(bytes));
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn invalid_target(message: &str) -> HiveError {
    HiveError::new(ErrorCode::InvalidArgument, message)
}

fn io_error(message: &str) -> HiveError {
    HiveError::new(ErrorCode::IoError, message)
}

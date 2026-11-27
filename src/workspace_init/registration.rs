use super::{
    dir_metadata, fd_path, open_directory, refuse_workspace_ancestor, rename_entry,
    require_local_filesystem, require_trusted_component, same_directory, same_inode,
    sync_directory, verify_entry,
};
use crate::config::{absolute_env, json_path};
use crate::config_file::{MAX_FILE_BYTES, ScopedDirectory};
use crate::config_v2::xdg_root;
use crate::error::{ErrorCode, HiveError};
use crate::workspace_status::Registration;

use std::ffi::OsStr;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

const ACTIVE_ROOT: &str = "active-root.json";
const TEMP_RECORD: &str = "active-root.json.tmp";

pub(super) struct FreshRegistration {
    workspaces: PathBuf,
    record: Registration,
}

impl FreshRegistration {
    pub(super) fn resolve(root: &Path, id: &str) -> Result<Self, HiveError> {
        let state = xdg_root(
            "XDG_STATE_HOME",
            absolute_env("HOME").as_deref(),
            ".local/state",
        )?;
        Self::new(&state, root, id)
    }

    pub(super) fn new(state: &Path, root: &Path, id: &str) -> Result<Self, HiveError> {
        let root: PathBuf = root.components().collect();
        let state: PathBuf = state.components().collect();
        let workspaces = state.join("workspaces");
        if workspaces.join(id).starts_with(&root)
            || state.components().any(|part| part == Component::ParentDir)
        {
            return Err(refused(
                "Runtime state requires a path outside the workspace without parent traversal.",
            ));
        }
        Ok(Self {
            workspaces,
            record: Registration {
                version: 2,
                workspace_id: id.to_owned(),
                workspace_root: json_path(&root)?.to_owned(),
            },
        })
    }

    pub(super) fn publish(&self, target: &File) -> Result<Published, HiveError> {
        let bytes = serde_json::to_vec(&self.record)
            .map_err(|_| refused("Could not encode the active-root registration."))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(refused(
                "The active-root registration exceeds its size limit.",
            ));
        }

        let workspaces = open_state_parent(&self.workspaces, target)?;
        let id = &self.record.workspace_id;
        // Exclusive UUID directory creation refuses any previous or partial
        // state, including an empty directory, without touching its lock.
        create_directory(&workspaces, id.as_ref()).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                refused("Runtime state already exists for the fresh workspace ID.")
            } else {
                state_io(error)
            }
        })?;

        (|| {
            let state = open_directory(&fd_path(&workspaces).join(id))?;
            require_private_directory(&state)?;
            let lock = lock(&state)?;
            let mut record = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(fd_path(&state).join(TEMP_RECORD))
                .map_err(state_io)?;
            record.write_all(&bytes).map_err(state_io)?;
            record.sync_all().map_err(state_io)?;
            super::require_entries(&state, &["lock", TEMP_RECORD])?;
            verify_entry(&state, TEMP_RECORD, &record)?;
            rename_entry(&state, TEMP_RECORD, &state, ACTIVE_ROOT)?;
            sync_directory(&state)?;
            sync_directory(&workspaces)?;
            Ok(Published {
                state,
                path: self.workspaces.join(id),
                lock,
                record,
                bytes,
            })
        })()
        .map_err(|error: HiveError| {
            HiveError::new(
                error.code,
                format!("{error} Incomplete runtime state for {id} was preserved."),
            )
        })
    }
}

pub(super) struct Published {
    state: File,
    path: PathBuf,
    lock: File,
    record: File,
    bytes: Vec<u8>,
}

impl Published {
    pub(super) fn preserved(&self, error: HiveError) -> HiveError {
        let id = self
            .path
            .file_name()
            .expect("workspace ID component")
            .to_string_lossy();
        HiveError::new(
            error.code,
            format!("{error} Runtime state for {id} was preserved."),
        )
    }

    pub(super) fn verify(&self) -> Result<(), HiveError> {
        let current = ScopedDirectory::open_absolute(&self.path)?;
        if !same_directory(current.handle(), &self.state)? {
            return Err(refused("The fresh workspace state directory changed."));
        }
        verify_entry(&self.state, "lock", &self.lock)?;
        verify_entry(&self.state, ACTIVE_ROOT, &self.record)?;
        if current.read(OsStr::new(ACTIVE_ROOT))?.as_deref() != Some(&self.bytes) {
            return Err(refused("The fresh active-root registration changed."));
        }
        Ok(())
    }
}

fn open_state_parent(path: &Path, target: &File) -> Result<File, HiveError> {
    let target = dir_metadata(target)?;
    let mut directory = open_directory(Path::new("/"))?;
    let check_ancestry = |directory: &File| {
        refuse_workspace_ancestor(directory).map_err(|error| {
            HiveError::new(
                error.code,
                format!("Unsafe runtime state ancestry. {error}"),
            )
        })
    };
    check_ancestry(&directory)?;
    let root = dir_metadata(&directory)?;
    let mut traversed = PathBuf::from("/");
    let private_start = path.components().count() - 3; // hive/v2/workspaces
    for (index, component) in path.components().enumerate() {
        let Component::Normal(name) = component else {
            continue;
        };
        let entry = fd_path(&directory).join(name);
        let child = match open_directory(&entry) {
            Err(error) if matches!(error.code, ErrorCode::NotFound) => {
                require_local_filesystem(&directory)?;
                match create_directory(&directory, name) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(state_io(error)),
                }
                open_directory(&entry)?
            }
            result => result?,
        };
        traversed.push(name);
        let metadata = dir_metadata(&child)?;
        if same_inode(&metadata, &target) {
            return Err(refused("Runtime state must be outside the workspace."));
        }
        require_trusted_component(&child, &traversed, &metadata, &root)?;
        if index >= private_start {
            require_private_directory(&child)?;
        }
        check_ancestry(&child)?;
        // An existing child may have just been created by another init
        // whose sync is still pending. Persist this link before descending.
        sync_directory(&directory)?;
        directory = child;
    }
    require_local_filesystem(&directory)?;
    Ok(directory)
}

fn create_directory(parent: &File, name: &OsStr) -> io::Result<()> {
    DirBuilder::new()
        .mode(0o700)
        .create(fd_path(parent).join(name))
}

fn require_private_directory(directory: &File) -> Result<(), HiveError> {
    let metadata = dir_metadata(directory)?;
    // SAFETY: geteuid takes no arguments.
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
        return Err(refused(
            "Runtime state requires an owned directory with mode 0700.",
        ));
    }
    Ok(())
}

// Same stable inode, file constraints and nonblocking flock as workspace
// start/stop. The file is never replaced or removed.
fn lock(state: &File) -> Result<File, HiveError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(fd_path(state).join("lock"))
        .map_err(state_io)?;
    let metadata = file.metadata().map_err(state_io)?;
    // SAFETY: geteuid takes no arguments.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(refused(
            "Runtime state requires a private, unshared file with mode 0600.",
        ));
    }
    // SAFETY: The returned file keeps the descriptor open and locked.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::WouldBlock {
            Err(HiveError::new(
                ErrorCode::LockBusy,
                "The workspace lifecycle lock is busy.",
            ))
        } else {
            Err(state_io(error))
        };
    }
    Ok(file)
}

fn refused(message: &str) -> HiveError {
    HiveError::new(ErrorCode::AuthorityUnverified, message)
}

fn state_io(error: io::Error) -> HiveError {
    HiveError::new(
        ErrorCode::IoError,
        format!("Workspace registration I/O failed ({:?}).", error.kind()),
    )
}

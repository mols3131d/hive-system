use std::ffi::{CString, OsStr};
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{current_dir, json_path};
use crate::dolt::{DOLT_VERSION, STORAGE_SCHEMA_VERSION};
use crate::error::{ErrorCode, HiveError};
use crate::{config_file, config_v2};

mod tools;

const POLICY_REVISION: &str = "c66b3c50150e18426485d134de92c6b311dac125";
const PAYLOAD_ENTRIES: [&str; 3] = ["db", "assets", "conversations"];

struct Prepared {
    db: File,
    dolt: File,
    assets: File,
    git: File,
    conversations: File,
    asset_commit: String,
    asset_object_digest: [u8; 32],
}

impl Prepared {
    fn entries(&self) -> [(&'static str, &File); 3] {
        [
            ("db", &self.db),
            ("assets", &self.assets),
            ("conversations", &self.conversations),
        ]
    }

    fn verify(&self, target: &File) -> Result<(), HiveError> {
        for (name, directory) in self.entries() {
            verify_entry(target, name, directory)?;
        }
        verify_entry(&self.db, ".dolt", &self.dolt)?;
        verify_entry(&self.assets, ".git", &self.git)?;
        config_v2::verify_bootstrap_layout(target)?;
        verify_git_revision(&self.git, &self.asset_commit, &self.asset_object_digest)
    }
}

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
    let target_json = json_path(&target)?;
    let (parent, original) = inspect(&target)?;
    lock_target(&original)?;
    require_entries(&original, &[])?;
    let check_path = || check_target_path(&target, &parent, &original);
    let workspace_id = new_uuid().map_err(|_| {
        HiveError::new(
            ErrorCode::IoError,
            "Could not generate a workspace identity.",
        )
    })?;
    let stage_name = format!("{}{workspace_id}", config_v2::INIT_STAGE_PREFIX);
    let stage_entry = fd_path(&parent).join(&stage_name);
    private_dir(&stage_entry)?;
    let stage = open_directory(&stage_entry)?;
    let scratch = fd_path(&stage).join("scratch");
    let payload = fd_path(&stage).join("workspace");
    let result: Result<_, HiveError> = (|| {
        private_dir(&scratch)?;
        let scratch_fd = open_directory(&scratch)?;
        let tools = tools::Installed::check(&scratch_fd)?;
        private_dir(&payload)?;
        for name in PAYLOAD_ENTRIES {
            private_dir(&payload.join(name))?;
        }
        let db = open_directory(&payload.join("db"))?;
        let assets = open_directory(&payload.join("assets"))?;
        let conversations = open_directory(&payload.join("conversations"))?;
        let (db_commit, dolt) = tools.initialize_db(&fd_path(&db), &workspace_id)?;
        let (asset_commit, git) = tools.initialize_assets(&fd_path(&assets))?;
        let asset_object_digest = git_object_digest(&git, &asset_commit)?;
        Ok((
            db_commit,
            Prepared {
                db,
                dolt,
                assets,
                git,
                conversations,
                asset_commit,
                asset_object_digest,
            },
        ))
    })();

    // Keep verified or partial payloads identifiable on refusal. Clean up
    // only our scratch tree, and remove the empty staging container on success.
    let with_stage = |error: HiveError| {
        HiveError::new(
            error.code,
            format!("{error} Incomplete staging under the original parent: {stage_name}."),
        )
    };
    let cleanup = fs::remove_dir_all(&scratch);
    let (db_commit, prepared) = match result {
        Ok(commits) => {
            cleanup.map_err(|_| with_stage(io_error("Could not remove isolated tool state.")))?;
            commits
        }
        Err(error) => {
            return Err(
                if !payload.exists() && fs::remove_dir(&stage_entry).is_ok() {
                    error
                } else {
                    with_stage(error)
                },
            );
        }
    };

    let descriptor = format!(
        "version: 2\nworkspace_id: {workspace_id}\nschema_version: {STORAGE_SCHEMA_VERSION}\npolicy_revision: {POLICY_REVISION}\nbootstrap_layout: {}\n",
        config_v2::BOOTSTRAP_LAYOUT,
    );
    let mut marker = OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .mode(0o600)
        .open(payload.join(config_v2::MARKER))
        .map_err(|_| {
            with_stage(io_error(
                "Could not create the staged workspace descriptor.",
            ))
        })?;
    marker
        .write_all(descriptor.as_bytes())
        .map_err(|_| with_stage(io_error("Could not write the staged workspace descriptor.")))?;
    let mut bytes = Vec::new();
    marker
        .rewind()
        .and_then(|()| marker.read_to_end(&mut bytes))
        .map_err(|_| with_stage(io_error("Could not read the staged workspace descriptor.")))?;
    config_v2::verify_descriptor(&bytes).map_err(with_stage)?;

    let payload_fd = open_directory(&payload).map_err(with_stage)?;
    sync_directory(&parent).map_err(with_stage)?;
    publish(
        &payload_fd,
        &original,
        &prepared,
        &marker,
        &bytes,
        check_path,
    )
    .map_err(|error| {
        with_stage(HiveError::new(
            error.code,
            format!("{error} Any moved entries remain in the original target directory."),
        ))
    })?;

    let mut output = json!({
        "workspace_root": target_json,
        "workspace_id": workspace_id,
        "policy_revision": POLICY_REVISION,
        "storage_schema_version": STORAGE_SCHEMA_VERSION,
        "dolt_version": DOLT_VERSION,
        "db_commit": db_commit,
        "asset_commit": prepared.asset_commit,
    });
    if fs::remove_dir(&payload)
        .and_then(|()| fs::remove_dir(&stage_entry))
        .is_err()
    {
        output["staging_directory"] = json!(stage_name);
    }
    Ok(output)
}

fn check_target_path(target: &Path, parent: &File, original: &File) -> Result<(), HiveError> {
    let changed = || {
        HiveError::new(
            ErrorCode::ConcurrentChange,
            "The workspace target or its parent changed during initialization.",
        )
    };
    let (current_parent, current) = inspect(target).map_err(|_| changed())?;
    if !same_directory(parent, &current_parent)? || !same_directory(original, &current)? {
        return Err(changed());
    }
    // The safe walk pins each component. Confirm that the two names also
    // resolve to those inodes in case they moved during the walk itself.
    let named_parent =
        fs::symlink_metadata(target.parent().expect("inspected target has a parent"))
            .map_err(|_| changed())?;
    let named_target = fs::symlink_metadata(target).map_err(|_| changed())?;
    if !same_inode(&dir_metadata(parent)?, &named_parent)
        || !same_inode(&dir_metadata(original)?, &named_target)
    {
        return Err(changed());
    }
    Ok(())
}

fn lock_target(target: &File) -> Result<(), HiveError> {
    // SAFETY: The descriptor stays open through preparation and publication.
    if unsafe { libc::flock(target.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    Err(if error.kind() == io::ErrorKind::WouldBlock {
        HiveError::new(
            ErrorCode::ConcurrentChange,
            "The workspace target is already locked for initialization.",
        )
    } else {
        HiveError::new(
            ErrorCode::IoError,
            format!("Could not lock the workspace target ({error})."),
        )
    })
}

fn require_entries(target: &File, expected: &[&str]) -> Result<(), HiveError> {
    let mut count = 0;
    for entry in fs::read_dir(fd_path(target))
        .map_err(|_| io_error("Could not inspect the workspace target contents."))?
    {
        let entry =
            entry.map_err(|_| io_error("Could not inspect the workspace target contents."))?;
        if !expected
            .iter()
            .any(|name| entry.file_name().as_encoded_bytes() == name.as_bytes())
        {
            return Err(HiveError::new(
                if expected.is_empty() {
                    ErrorCode::AlreadyExists
                } else {
                    ErrorCode::ConcurrentChange
                },
                "The workspace target contains unexpected entries.",
            ));
        }
        count += 1;
    }
    if count != expected.len() {
        return Err(HiveError::new(
            ErrorCode::ConcurrentChange,
            "A published workspace entry was removed.",
        ));
    }
    Ok(())
}

fn publish(
    payload: &File,
    target: &File,
    prepared: &Prepared,
    descriptor: &File,
    descriptor_bytes: &[u8],
    mut check_path: impl FnMut() -> Result<(), HiveError>,
) -> Result<(), HiveError> {
    check_path()?;
    require_entries(target, &[])?;
    let pending_name = config_v2::INIT_PENDING
        .to_str()
        .expect("ASCII publication sentinel name");
    let pending = create_publication_sentinel(target, pending_name)?;
    for (name, directory) in prepared.entries() {
        verify_entry(payload, name, directory)?;
        move_entry(payload, target, name)?;
    }

    let [db, assets, conversations] = PAYLOAD_ENTRIES;
    require_entries(target, &[db, assets, conversations, pending_name])?;
    check_path()?;
    verify_entry(target, pending_name, &pending)?;
    // SAFETY: The target FD is pinned and the sentinel name is a single component.
    if unsafe { libc::unlinkat(target.as_raw_fd(), config_v2::INIT_PENDING.as_ptr(), 0) } != 0 {
        return Err(io_error(
            "Could not clear the incomplete workspace publication sentinel.",
        ));
    }

    check_path()?;
    prepared.verify(target)?;
    for (_, directory) in prepared.entries() {
        sync_tree(directory)?;
    }
    sync_directory(target)?;
    descriptor
        .sync_all()
        .map_err(|_| io_error("Could not sync the staged workspace descriptor."))?;
    sync_directory(payload)?;

    // The canonical descriptor remains absent through every verification.
    // The no-clobber move is the readiness event and is never rolled back.
    check_path()?;
    prepared.verify(target)?;
    require_entries(target, &PAYLOAD_ENTRIES)?;
    verify_entry(payload, config_v2::MARKER, descriptor)?;
    verify_descriptor_bytes(descriptor, descriptor_bytes)?;
    check_path()?;
    move_entry(payload, target, config_v2::MARKER)?;
    target.sync_all().map_err(|_| {
        HiveError::new(
            ErrorCode::UnknownWriteOutcome,
            "The workspace descriptor was published, but its durability could not be confirmed.",
        )
    })
}

fn sync_directory(directory: &File) -> Result<(), HiveError> {
    directory
        .sync_all()
        .map_err(|_| io_error("Could not sync a workspace directory."))
}

fn sync_tree(directory: &File) -> Result<(), HiveError> {
    let sync_error = |_| io_error("Could not sync the verified workspace payload.");
    for entry in fs::read_dir(fd_path(directory)).map_err(sync_error)? {
        let entry = entry.map_err(sync_error)?;
        if entry.file_type().map_err(sync_error)?.is_dir() {
            let child = open_directory(&entry.path())?;
            sync_tree(&child)?;
        } else {
            let file = config_file::open_regular_child(directory, &entry.file_name())?.ok_or_else(
                || {
                    HiveError::new(
                        ErrorCode::ConcurrentChange,
                        "A prepared workspace file disappeared.",
                    )
                },
            )?;
            file.sync_all().map_err(sync_error)?;
        }
    }
    sync_directory(directory)
}

fn verify_git_revision(git: &File, commit: &str, digest: &[u8; 32]) -> Result<(), HiveError> {
    // The bootstrap commit is installed as a loose object and a loose main
    // reference. Read only these anchored Git files after moving the tree.
    let refs = config_file::open_scoped_directory(git, OsStr::new("refs"), &[OsStr::new("heads")])?
        .ok_or_else(unverified_asset_commit)?;
    if read_git_file(git, "HEAD", 128)? != b"ref: refs/heads/main\n"
        || read_git_file(&refs, "main", 128)? != format!("{commit}\n").as_bytes()
        || git_object_digest(git, commit)? != *digest
    {
        return Err(unverified_asset_commit());
    }
    Ok(())
}

fn git_object_digest(git: &File, commit: &str) -> Result<[u8; 32], HiveError> {
    let (fanout, object) = commit.split_at(2);
    let objects =
        config_file::open_scoped_directory(git, OsStr::new("objects"), &[OsStr::new(fanout)])?
            .ok_or_else(unverified_asset_commit)?;
    let bytes = read_git_file(&objects, object, 16 * 1024)?;
    Ok(Sha256::digest(bytes).into())
}

fn read_git_file(directory: &File, name: &str, limit: u64) -> Result<Vec<u8>, HiveError> {
    let mut bytes = Vec::new();
    config_file::open_regular_child(directory, OsStr::new(name))?
        .ok_or_else(unverified_asset_commit)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unverified_asset_commit())?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(unverified_asset_commit());
    }
    Ok(bytes)
}

fn unverified_asset_commit() -> HiveError {
    HiveError::new(
        ErrorCode::AuthorityUnverified,
        "The prepared Git asset repository no longer matches its verified initial commit.",
    )
}

fn verify_descriptor_bytes(descriptor: &File, expected: &[u8]) -> Result<(), HiveError> {
    let mut bytes = vec![0; expected.len()];
    if dir_metadata(descriptor)?.len() == expected.len() as u64
        && descriptor.read_exact_at(&mut bytes, 0).is_ok()
        && bytes == expected
    {
        return Ok(());
    }
    Err(HiveError::new(
        ErrorCode::ConcurrentChange,
        "The published workspace descriptor contents changed.",
    ))
}

fn create_publication_sentinel(target: &File, name: &str) -> Result<File, HiveError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(fd_path(target).join(name))
        .map_err(|error| {
            HiveError::new(
                if error.kind() == io::ErrorKind::AlreadyExists {
                    ErrorCode::ConcurrentChange
                } else {
                    ErrorCode::IoError
                },
                format!(
                    "Could not create the incomplete workspace publication sentinel ({error})."
                ),
            )
        })
}

fn verify_entry(target: &File, name: &str, expected: &File) -> Result<(), HiveError> {
    let changed = || {
        HiveError::new(
            ErrorCode::ConcurrentChange,
            "A published workspace entry was moved or replaced.",
        )
    };
    let actual = fs::symlink_metadata(fd_path(target).join(name)).map_err(|_| changed())?;
    if !same_inode(&dir_metadata(expected)?, &actual) {
        return Err(changed());
    }
    Ok(())
}

fn move_entry(source: &File, target: &File, name: &str) -> Result<(), HiveError> {
    let name = CString::new(name).expect("fixed workspace entry names contain no NUL");
    // SAFETY: The descriptors are pinned and the name is a single component.
    if unsafe {
        libc::renameat2(
            source.as_raw_fd(),
            name.as_ptr(),
            target.as_raw_fd(),
            name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    Err(HiveError::new(
        match error.kind() {
            io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound => ErrorCode::ConcurrentChange,
            _ => ErrorCode::IoError,
        },
        format!("Could not publish a verified workspace entry ({error})."),
    ))
}

fn inspect(target: &Path) -> Result<(File, File), HiveError> {
    let name = target
        .file_name()
        .ok_or_else(|| invalid_target("The workspace target must name an empty directory."))?;
    if config_v2::has_init_stage_component(target)
        || target.components().any(|component| {
            component == Component::ParentDir
                || matches!(component, Component::Normal(name) if name == ".dolt" || name == ".git")
        })
    {
        return Err(invalid_target(
            "The workspace target contains a disallowed path component.",
        ));
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
    let original = open_directory(&fd_path(&directory).join(name))?;
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
    Ok((directory, original))
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
    Ok(same_inode(&left, &right))
}

fn same_inode(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const FIXTURE_COMMIT: &str = "0123456789012345678901234567890123456789";
    const FIXTURE_DESCRIPTOR: &str = concat!(
        "version: 2\n",
        "workspace_id: 01900000-0000-7000-8000-000000000001\n",
        "schema_version: 1\n",
        "policy_revision: c66b3c50150e18426485d134de92c6b311dac125\n",
        "bootstrap_layout: dolt-native-v1\n",
    );

    struct Publication {
        root: PathBuf,
        parent: PathBuf,
        target: PathBuf,
        payload: PathBuf,
    }

    impl Publication {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("hive-publish-test-{}", new_uuid().unwrap()));
            let parent = root.join("parent");
            let target = parent.join("target");
            let payload = root.join("payload");
            for path in [&root, &parent, &target, &payload] {
                private_dir(path).unwrap();
            }
            for name in PAYLOAD_ENTRIES {
                private_dir(&payload.join(name)).unwrap();
            }
            fs::create_dir_all(payload.join("db/.dolt/noms")).unwrap();
            fs::write(payload.join("db/.dolt/repo_state.json"), b"fixture state").unwrap();
            fs::write(payload.join("db/.dolt/noms/manifest"), b"fixture manifest").unwrap();
            let git = payload.join("assets/.git");
            fs::create_dir_all(git.join("refs/heads")).unwrap();
            fs::create_dir_all(git.join("objects/01")).unwrap();
            fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
            fs::write(git.join("refs/heads/main"), format!("{FIXTURE_COMMIT}\n")).unwrap();
            fs::write(
                git.join("objects/01").join(&FIXTURE_COMMIT[2..]),
                b"fixture object",
            )
            .unwrap();
            fs::write(payload.join(config_v2::MARKER), FIXTURE_DESCRIPTOR).unwrap();
            Self {
                root,
                parent,
                target,
                payload,
            }
        }

        fn publish_with(&self, after_check: impl FnMut(usize)) -> Result<(), HiveError> {
            publish_fixture(&self.target, &self.payload, after_check)
        }
    }

    fn publish_fixture(
        path: &Path,
        source: &Path,
        mut after_check: impl FnMut(usize),
    ) -> Result<(), HiveError> {
        let (parent, target) = inspect(path)?;
        lock_target(&target)?;
        let payload = open_directory(source)?;
        let descriptor = OpenOptions::new()
            .read(true)
            .write(true)
            .open(source.join(config_v2::MARKER))
            .unwrap();
        let prepared = Prepared {
            db: open_directory(&source.join("db"))?,
            dolt: open_directory(&source.join("db/.dolt"))?,
            assets: open_directory(&source.join("assets"))?,
            git: open_directory(&source.join("assets/.git"))?,
            conversations: open_directory(&source.join("conversations"))?,
            asset_commit: FIXTURE_COMMIT.to_owned(),
            asset_object_digest: Sha256::digest(b"fixture object").into(),
        };
        let mut checks = 0;
        publish(
            &payload,
            &target,
            &prepared,
            &descriptor,
            FIXTURE_DESCRIPTOR.as_bytes(),
            || {
                check_target_path(path, &parent, &target)?;
                checks += 1;
                after_check(checks);
                Ok(())
            },
        )
    }

    impl Drop for Publication {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn target_replacement_after_last_prepublication_check_preserves_replacement() {
        let fixture = Publication::new();
        let moved = fixture.parent.join("moved");
        let error = fixture
            .publish_with(|check| {
                if check == 2 {
                    fs::rename(&fixture.target, &moved).unwrap();
                    private_dir(&fixture.target).unwrap();
                    fs::write(fixture.target.join(config_v2::MARKER), b"replacement").unwrap();
                }
            })
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::ConcurrentChange));
        assert_eq!(
            fs::read(fixture.target.join(config_v2::MARKER)).unwrap(),
            b"replacement"
        );
        assert_eq!(fs::read_dir(&fixture.target).unwrap().count(), 1);
        assert!(moved.join("db").is_dir());
        assert_unpublished(&moved);
    }

    #[test]
    fn parent_move_before_descriptor_check_leaves_staging_unpublished() {
        let fixture = Publication::new();
        let moved = fixture.root.join("moved");
        let error = fixture
            .publish_with(|check| {
                if check == 1 {
                    fs::rename(&fixture.parent, &moved).unwrap();
                    private_dir(&fixture.parent).unwrap();
                    private_dir(&fixture.target).unwrap();
                }
            })
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::ConcurrentChange));
        assert!(fs::read_dir(&fixture.target).unwrap().next().is_none());
        assert!(moved.join("target/db").is_dir());
        assert!(!moved.join("target").join(config_v2::MARKER).exists());
        assert_eq!(
            fs::read(fixture.payload.join(config_v2::MARKER)).unwrap(),
            FIXTURE_DESCRIPTOR.as_bytes()
        );
    }

    #[test]
    fn parent_replacement_after_last_prepublication_check_is_refused() {
        let fixture = Publication::new();
        let moved = fixture.root.join("moved");
        let error = fixture
            .publish_with(|check| {
                if check == 2 {
                    fs::rename(&fixture.parent, &moved).unwrap();
                    private_dir(&fixture.parent).unwrap();
                    private_dir(&fixture.target).unwrap();
                }
            })
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::ConcurrentChange));
        assert!(fs::read_dir(&fixture.target).unwrap().next().is_none());
        assert_unpublished(&moved.join("target"));
    }

    #[test]
    fn competing_descriptor_is_never_clobbered() {
        let fixture = Publication::new();
        let error = fixture
            .publish_with(|check| {
                if check == 5 {
                    fs::write(fixture.target.join(config_v2::MARKER), b"competing").unwrap();
                }
            })
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::ConcurrentChange));
        assert_eq!(
            fs::read(fixture.target.join(config_v2::MARKER)).unwrap(),
            b"competing"
        );
        assert_eq!(
            fs::read(fixture.payload.join(config_v2::MARKER)).unwrap(),
            FIXTURE_DESCRIPTOR.as_bytes()
        );
    }

    #[test]
    fn publication_sentinel_creation_never_follows_or_replaces_a_symlink() {
        let fixture = Publication::new();
        let sentinel_name = config_v2::INIT_PENDING.to_str().unwrap();
        let sentinel = fixture.target.join(sentinel_name);
        let other = fixture.root.join("keep");
        fs::write(&other, b"keep").unwrap();
        symlink(&other, &sentinel).unwrap();
        let target = open_directory(&fixture.target).unwrap();

        let error = create_publication_sentinel(&target, sentinel_name).unwrap_err();
        assert!(matches!(error.code, ErrorCode::ConcurrentChange));
        assert_eq!(fs::read_link(&sentinel).unwrap(), other);
        assert_eq!(fs::read(other).unwrap(), b"keep");
    }

    #[test]
    fn changed_prepared_directory_is_not_published() {
        let fixture = Publication::new();
        let staged_assets = fixture.payload.join("assets");
        let error = fixture
            .publish_with(|check| {
                if check == 1 {
                    fs::rename(&staged_assets, fixture.root.join("original-assets")).unwrap();
                    private_dir(&staged_assets).unwrap();
                    fs::write(staged_assets.join("keep"), b"replacement").unwrap();
                }
            })
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::ConcurrentChange));
        assert_eq!(
            fs::read(staged_assets.join("keep")).unwrap(),
            b"replacement"
        );
        assert!(!fixture.target.join("assets").exists());
        assert!(!fixture.target.join(config_v2::MARKER).exists());
    }

    #[test]
    fn missing_replaced_or_changed_git_repo_keeps_descriptor_unpublished() {
        for mode in ["missing", "replaced", "revision", "object"] {
            let fixture = Publication::new();
            let git = fixture.target.join("assets/.git");
            let error = fixture
                .publish_with(|check| {
                    if check == 3 {
                        if mode == "revision" {
                            fs::write(git.join("refs/heads/main"), b"unexpected revision").unwrap();
                        } else if mode == "object" {
                            fs::write(
                                git.join("objects/01").join(&FIXTURE_COMMIT[2..]),
                                b"replacement object",
                            )
                            .unwrap();
                        } else {
                            let original = fixture.root.join("original-git");
                            fs::rename(&git, &original).unwrap();
                            if mode == "replaced" {
                                private_dir(&git).unwrap();
                                fs::write(git.join("keep"), b"replacement").unwrap();
                            }
                        }
                    }
                })
                .unwrap_err();
            if matches!(mode, "revision" | "object") {
                assert!(matches!(error.code, ErrorCode::AuthorityUnverified));
            } else {
                assert!(matches!(error.code, ErrorCode::ConcurrentChange));
            }
            assert_unpublished(&fixture.target);
            assert_eq!(
                fs::read(fixture.payload.join(config_v2::MARKER)).unwrap(),
                FIXTURE_DESCRIPTOR.as_bytes()
            );
            if mode == "replaced" {
                assert_eq!(fs::read(git.join("keep")).unwrap(), b"replacement");
            }
        }
    }

    #[test]
    fn readers_refuse_paused_and_killed_publications() {
        for (pause, mode) in [
            (2, "complete"),
            (2, "killed"),
            (3, "complete"),
            (3, "moved"),
            (3, "replaced"),
            (3, "killed"),
            (5, "killed"),
        ] {
            let fixture = Publication::new();
            let ready = fixture.root.join("ready");
            let mut writer = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "workspace_init::linux::tests::publication_witness_child",
                    "--ignored",
                ])
                .env_clear()
                .env("HIVE_TEST_PUBLICATION_ROOT", &fixture.root)
                .env("HIVE_TEST_PUBLICATION_MODE", mode)
                .env("HIVE_TEST_PUBLICATION_PAUSE", pause.to_string())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            if !wait_for(&ready) {
                let _ = writer.kill();
                let output = writer.wait_with_output().unwrap();
                panic!("Writer never reached its final path check: {output:?}");
            }
            let moved = fixture.root.join("moved");
            let moved_target = moved.join("target");
            let observed = if mode == "moved" {
                fs::rename(&fixture.parent, &moved).unwrap();
                private_dir(&fixture.parent).unwrap();
                private_dir(&fixture.target).unwrap();
                fs::write(fixture.target.join("keep"), b"replacement parent").unwrap();
                &moved_target
            } else {
                &fixture.target
            };
            assert!(!observed.join(config_v2::MARKER).exists());
            let pending = observed.join(config_v2::INIT_PENDING.to_str().unwrap());
            if pause == 2 {
                assert!(pending.is_file());
                assert_incomplete(observed);
            } else {
                assert!(!pending.exists());
                assert_unpublished(observed);
            }

            if matches!(mode, "replaced" | "killed") {
                fs::rename(observed.join("db"), fixture.root.join("original-db")).unwrap();
                fs::create_dir(observed.join("db")).unwrap();
                fs::write(observed.join("db/keep"), b"replacement").unwrap();
            }
            if mode == "killed" {
                writer.kill().unwrap();
                assert!(!writer.wait().unwrap().success());
                assert!(!observed.join(config_v2::MARKER).exists());
                assert_eq!(fs::read(observed.join("db/keep")).unwrap(), b"replacement");
            } else {
                fs::write(fixture.root.join("release"), b"go").unwrap();
                let output = writer.wait_with_output().unwrap();
                assert!(output.status.success(), "{output:?}");
            }
            if mode == "complete" {
                let candidate = config_v2::read_explicit(observed).unwrap();
                assert_eq!(
                    candidate.workspace_id(),
                    "01900000-0000-7000-8000-000000000001"
                );
            } else if pause == 2 {
                assert!(pending.is_file());
                assert_incomplete(observed);
            } else {
                assert!(!pending.exists());
                assert_unpublished(observed);
            }
            if mode == "moved" {
                assert_eq!(
                    fs::read(fixture.target.join("keep")).unwrap(),
                    b"replacement parent"
                );
                assert_eq!(fs::read_dir(&fixture.target).unwrap().count(), 1);
            }
            if matches!(mode, "replaced" | "killed") {
                assert_eq!(fs::read(observed.join("db/keep")).unwrap(), b"replacement");
            }
        }
    }

    fn selection_error(path: &Path) -> HiveError {
        config_v2::read_explicit(path)
            .err()
            .expect("workspace must be hidden")
    }

    fn assert_unpublished(path: &Path) {
        assert!(!path.join(config_v2::MARKER).exists());
        assert!(matches!(selection_error(path).code, ErrorCode::NotFound));
    }

    fn assert_incomplete(path: &Path) {
        let error = selection_error(path);
        assert!(matches!(error.code, ErrorCode::InvalidConfig));
        assert_eq!(error.to_string(), "Workspace initialization is incomplete.");
    }

    fn wait_for(path: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    #[ignore = "subprocess witness invoked by the publication test"]
    fn publication_witness_child() {
        let Some(root) = std::env::var_os("HIVE_TEST_PUBLICATION_ROOT").map(PathBuf::from) else {
            return;
        };
        let pause = std::env::var("HIVE_TEST_PUBLICATION_PAUSE")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let result = publish_fixture(
            &root.join("parent/target"),
            &root.join("payload"),
            |check| {
                if check == pause {
                    fs::write(root.join("ready"), b"writer paused").unwrap();
                    assert!(
                        wait_for(&root.join("release")),
                        "publication witness timed out"
                    );
                }
            },
        );
        if matches!(
            std::env::var("HIVE_TEST_PUBLICATION_MODE")
                .unwrap()
                .as_str(),
            "moved" | "replaced"
        ) {
            assert!(matches!(
                result.unwrap_err().code,
                ErrorCode::ConcurrentChange
            ));
        } else {
            result.unwrap();
        }
    }

    #[test]
    fn initializers_cannot_lock_the_same_directory_twice() {
        let fixture = Publication::new();
        let first = open_directory(&fixture.target).unwrap();
        let second = open_directory(&fixture.target).unwrap();
        lock_target(&first).unwrap();
        assert!(matches!(
            lock_target(&second).unwrap_err().code,
            ErrorCode::ConcurrentChange
        ));
        drop(first);
        lock_target(&second).unwrap();
    }
}

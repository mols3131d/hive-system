use std::path::Path;

use serde_json::Value;

use crate::error::HiveError;

pub(crate) fn run(source: &Path, target: &Path) -> Result<Value, HiveError> {
    #[cfg(target_os = "linux")]
    {
        linux::restore(source, target)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, target);
        Err(HiveError::new(
            crate::error::ErrorCode::NotImplemented,
            "Backup restoration is supported only on Linux.",
        ))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};

    use rustix::fs::{RenameFlags, renameat_with};
    use serde::Deserialize;
    use serde_json::{Value, json};

    use crate::config::json_path;
    use crate::config_file::ScopedDirectory;
    use crate::config_git::{ASSET_BACKUP_REF, MAX_BLOB_BYTES, is_full_lowercase_oid};
    use crate::config_v2::{BOOTSTRAP_LAYOUT, MARKER, verify_bootstrap_layout};
    use crate::dolt::{
        DOLT_VERSION, STORAGE_SCHEMA_VERSION, is_commit_hash, is_uuid, validate_restored_records,
    };
    use crate::error::{ErrorCode, HiveError};
    use crate::workspace_backup::{
        DigestEntry, MAX_FILES, MAX_MANIFEST_BYTES, MAX_PAYLOAD_BYTES, digest_file, verify_db,
        visit_payload,
    };
    use crate::workspace_init::{
        Installed, fd_path, inspect_target_parent, lock_target, require_entries, restrict_db_modes,
        same_directory, sync_tree, verify_target_directory,
    };
    use crate::workspace_status::{require_inactive_target, require_local_filesystem};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Manifest {
        format_version: u32,
        workspace_id: String,
        storage_schema_version: u32,
        dolt_version: String,
        dolt_commit: String,
        asset_git_commit: String,
        policy_revision: String,
        #[serde(rename = "created_at_unix_seconds")]
        _created_at_unix_seconds: u64,
        consistency_state: String,
        conversations_included: bool,
        files: Vec<DigestEntry>,
    }

    pub(super) fn restore(source: &Path, target: &Path) -> Result<Value, HiveError> {
        let source = ScopedDirectory::open_absolute(&absolute_path(source)?)?;
        require_local_filesystem(&source)?;
        let target = absolute_path(target)?;
        if target.starts_with(&source.path) || source.path.starts_with(&target) {
            return Err(invalid("Select a target separate from the backup source."));
        }
        let target_json = json_path(&target)?;
        let parent = inspect_target_parent(&target)?;
        let name = target.file_name().expect("inspected target name");
        let target_entry = fd_path(&parent).join(name);
        let original = open_target(&target_entry)?;
        if let Some(original) = &original {
            verify_target_directory(&parent, original)?;
            lock_target(original)?;
            require_entries(original, &[])?;
        }

        let manifest = read_manifest(&source)?;
        require_inactive_target(&manifest.workspace_id, &target)?;
        let stage = tempfile::Builder::new()
            .prefix(crate::config_v2::INIT_STAGE_PREFIX)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(fd_path(&parent))
            .map_err(io_error)?;
        let stage_dir = open_target(stage.path())?.ok_or_else(changed_target)?;
        let stage_path = fd_path(&stage_dir);
        let snapshot = stage_path.join("snapshot");
        let scratch = stage_path.join("scratch");
        let payload = stage_path.join("workspace");
        for directory in [
            &snapshot,
            &snapshot.join("dolt"),
            &snapshot.join("dolt/oldgen"),
        ] {
            create_private_directory(directory)?;
        }
        copy_payload(&source, &snapshot, manifest.files)?;

        let scratch_dir = create_private_directory(&scratch)?;
        let payload_dir = create_private_directory(&payload)?;
        lock_target(&payload_dir)?;
        let assets = payload.join("assets");
        create_private_directory(&assets)?;
        create_private_directory(&payload.join("conversations"))?;
        let installed = Installed::check(&scratch_dir)?;
        installed.prepare_dolt_restore()?;
        // Use a fixed ASCII path to our verified copy, even when the caller's
        // pathname has spaces or characters with special meaning in URLs.
        let snapshot_path = format!(
            "/proc/{}/fd/{}/snapshot",
            std::process::id(),
            stage_dir.as_raw_fd()
        );
        installed.dolt(
            &payload,
            &[
                "backup",
                "restore",
                &format!("file://{snapshot_path}/dolt"),
                "db",
            ],
            None,
        )?;
        let db = payload.join("db");
        // Pinned Dolt fsck walks the commit DAG and verifies each historical
        // root and its referenced objects as well as the store's chunks.
        installed.dolt(&db, &["fsck", "--quiet"], None)?;
        if verify_db(&installed, &db, &manifest.workspace_id)? != manifest.dolt_commit {
            return Err(unverified(
                "The restored Dolt revision differs from the manifest.",
            ));
        }
        installed.dolt(
            &db,
            &["constraints", "verify", "--all", "--output-only"],
            None,
        )?;
        restrict_db_modes(&db)?;

        restore_assets(
            &installed,
            &assets,
            &format!("{snapshot_path}/assets.bundle"),
            &manifest.asset_git_commit,
        )?;
        validate_restored_records(
            &installed,
            &db,
            &manifest.dolt_commit,
            &manifest.workspace_id,
            |path| {
                let object = format!("{}:{path}", manifest.asset_git_commit);
                let size = installed.git(&assets, &["cat-file", "-s", &object])?;
                if !size
                    .trim()
                    .parse::<usize>()
                    .is_ok_and(|size| size <= MAX_BLOB_BYTES)
                {
                    return Err(unverified("A restored Soul exceeds the asset blob limit."));
                }
                installed.git(&assets, &["cat-file", "blob", &object])
            },
        )?;
        verify_bootstrap_layout(&payload_dir)?;
        let mut descriptor = private_file(&payload.join(MARKER))?;
        write!(
            descriptor,
            "version: 2\nworkspace_id: {}\nschema_version: {STORAGE_SCHEMA_VERSION}\npolicy_revision: {}\nbootstrap_layout: {BOOTSTRAP_LAYOUT}\n",
            manifest.workspace_id, manifest.policy_revision,
        ).map_err(io_error)?;
        sync_tree(&payload_dir)?;

        let current_parent = inspect_target_parent(&target)?;
        require_inactive_target(&manifest.workspace_id, &target)?;
        let staged_payload = open_target(&payload)?.ok_or_else(changed_target)?;
        if !same_directory(&parent, &current_parent)?
            || !same_directory(&payload_dir, &staged_payload)?
        {
            return Err(changed_target());
        }
        let flags = if let Some(original) = &original {
            let current = open_target(&target_entry)?.ok_or_else(changed_target)?;
            if !same_directory(original, &current)? {
                return Err(changed_target());
            }
            verify_target_directory(&parent, &current)?;
            require_entries(original, &[])?;
            // Directory rename may replace only an empty directory. If a
            // concurrent writer adds anything, the kernel refuses the move.
            RenameFlags::empty()
        } else {
            RenameFlags::NOREPLACE
        };
        renameat_with(&stage_dir, "workspace", &parent, name, flags).map_err(|error| {
            if matches!(
                error,
                rustix::io::Errno::EXIST | rustix::io::Errno::NOTEMPTY
            ) {
                HiveError::new(ErrorCode::AlreadyExists, "The restore target is occupied.")
            } else {
                io_error(error.into())
            }
        })?;
        let uncertain = || {
            HiveError::new(
                ErrorCode::UnknownWriteOutcome,
                "The restored workspace was published but its destination or durability could not be confirmed.",
            )
        };
        parent.sync_all().map_err(|_| uncertain())?;
        let current_parent = inspect_target_parent(&target).map_err(|_| uncertain())?;
        let published = open_target(&target_entry)
            .map_err(|_| uncertain())?
            .ok_or_else(uncertain)?;
        if !same_directory(&parent, &current_parent).map_err(|_| uncertain())?
            || !same_directory(&published, &payload_dir).map_err(|_| uncertain())?
        {
            return Err(uncertain());
        }

        Ok(json!({
            "workspace_root": target_json,
            "workspace_id": manifest.workspace_id,
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "policy_revision": manifest.policy_revision,
            "dolt_version": DOLT_VERSION,
            "db_commit": manifest.dolt_commit,
            "asset_commit": manifest.asset_git_commit,
            "active_root": "inactive",
            "conversations_restored": false,
        }))
    }

    fn absolute_path(path: &Path) -> Result<PathBuf, HiveError> {
        if path.as_os_str().is_empty() || path.components().any(|part| part == Component::ParentDir)
        {
            return Err(invalid(
                "Supply explicit local paths without parent traversal.",
            ));
        }
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_err(io_error)?.join(path)
        };
        Ok(path.components().collect())
    }

    fn open_target(path: &Path) -> Result<Option<File>, HiveError> {
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)
        {
            Ok(dir) => Ok(Some(dir)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
                Err(invalid("The restore target must be a real directory."))
            }
            Err(error) => Err(io_error(error)),
        }
    }

    fn create_private_directory(path: &Path) -> Result<File, HiveError> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(io_error)?;
        open_target(path)?.ok_or_else(changed_target)
    }

    fn private_file(path: &Path) -> Result<File, HiveError> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(io_error)
    }

    fn read_manifest(source: &ScopedDirectory) -> Result<Manifest, HiveError> {
        let rejected = || unverified("The backup manifest is invalid or unsupported.");
        let file = source
            .open_regular(OsStr::new("manifest.json"))?
            .ok_or_else(rejected)?;
        let meta = file.metadata().map_err(io_error)?;
        if meta.nlink() != 1 || meta.len() > MAX_MANIFEST_BYTES as u64 {
            return Err(rejected());
        }
        let mut bytes = Vec::new();
        file.take(MAX_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() as u64 != meta.len() {
            return Err(rejected());
        }
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| rejected())?;
        if manifest.format_version != 1
            || manifest.storage_schema_version != STORAGE_SCHEMA_VERSION
            || manifest.dolt_version != DOLT_VERSION
            || manifest.consistency_state != "verified_stopped_committed"
            || manifest.conversations_included
            || !is_uuid(&manifest.workspace_id)
            || !is_commit_hash(&manifest.dolt_commit)
            || !is_full_lowercase_oid(&manifest.asset_git_commit)
            || manifest.policy_revision.len() != 40
            || !is_full_lowercase_oid(&manifest.policy_revision)
            || manifest.files.len() > MAX_FILES
        {
            return Err(rejected());
        }
        Ok(manifest)
    }

    fn copy_payload(
        source: &ScopedDirectory,
        snapshot: &Path,
        files: Vec<DigestEntry>,
    ) -> Result<(), HiveError> {
        let rejected =
            || unverified("Backup files do not match the manifest or exceed the payload limits.");
        let mut expected = BTreeMap::new();
        let mut remaining = MAX_PAYLOAD_BYTES;
        for entry in files {
            remaining = remaining.checked_sub(entry.bytes).ok_or_else(rejected)?;
            if entry.sha256.len() != 64
                || !is_full_lowercase_oid(&entry.sha256)
                || expected.insert(entry.path.clone(), entry).is_some()
            {
                return Err(rejected());
            }
        }

        visit_payload(source, |directory, name, path| {
            // Only the enumerated, native-layout paths reach filesystem APIs.
            let entry = expected.remove(&path).ok_or_else(rejected)?;
            let output = private_file(&snapshot.join(&path))?;
            if digest_file(directory, name, path, entry.bytes, output)? != entry {
                return Err(rejected());
            }
            Ok(())
        })?;
        if !expected.is_empty() {
            return Err(rejected());
        }
        Ok(())
    }

    fn restore_assets(
        installed: &Installed<'_>,
        assets: &Path,
        bundle: &str,
        revision: &str,
    ) -> Result<(), HiveError> {
        let object_format = if revision.len() == 64 {
            "sha256"
        } else {
            "sha1"
        };
        let git = |args: &[&str]| installed.git(assets, args);
        git(&[
            "init",
            "--initial-branch=main",
            "--template=",
            &format!("--object-format={object_format}"),
        ])?;
        git(&["bundle", "verify", bundle])?;
        if git(&["bundle", "unbundle", bundle])? != format!("{revision} {ASSET_BACKUP_REF}\n") {
            return Err(unverified(
                "The asset bundle revision differs from the manifest.",
            ));
        }
        git(&["update-ref", "refs/heads/main", revision])?;
        git(&["fsck", "--full", "--no-reflogs", revision])?;
        if git(&["rev-parse", "--verify", "HEAD^{commit}"])?.trim() != revision {
            return Err(unverified(
                "The restored asset revision differs from the manifest.",
            ));
        }
        // Validate modes before checkout so the visible workspace contains
        // only ordinary files and directories. Historical objects stay intact.
        let modes = git(&["ls-tree", "-r", "--format=%(objectmode)", revision])?;
        if modes
            .lines()
            .any(|mode| !matches!(mode, "100644" | "100755"))
        {
            return Err(unverified(
                "The asset tree contains an unsupported entry type.",
            ));
        }
        git(&["reset", "--hard", revision])?;
        if !git(&["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
            return Err(unverified("The restored asset worktree is not clean."));
        }
        Ok(())
    }

    fn changed_target() -> HiveError {
        HiveError::new(
            ErrorCode::ConcurrentChange,
            "The restore target or its parent changed.",
        )
    }

    fn invalid(message: &'static str) -> HiveError {
        HiveError::new(ErrorCode::InvalidArgument, message)
    }

    fn unverified(message: &'static str) -> HiveError {
        HiveError::new(ErrorCode::AuthorityUnverified, message)
    }

    fn io_error(error: io::Error) -> HiveError {
        HiveError::new(
            ErrorCode::IoError,
            format!("Could not access local restore files ({error})."),
        )
    }
}

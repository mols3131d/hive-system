use std::path::Path;

use serde_json::Value;

use crate::error::HiveError;

pub(crate) fn run(
    source: &Path,
    target: &Path,
    include_conversations: bool,
) -> Result<Value, HiveError> {
    #[cfg(target_os = "linux")]
    {
        linux::restore(source, target, include_conversations)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, target, include_conversations);
        Err(HiveError::new(
            crate::error::ErrorCode::NotImplemented,
            "Backup restoration is supported only on Linux.",
        ))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::collections::HashSet;
    use std::ffi::OsStr;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};

    use rustix::fs::{RenameFlags, renameat_with};
    use serde::{Deserialize, Deserializer};
    use serde_json::{Value, json, value::RawValue};

    use crate::config::json_path;
    use crate::config_file::ScopedDirectory;
    use crate::config_git::{ASSET_BACKUP_REF, is_full_lowercase_oid, verify_checkout_tree};
    use crate::config_v2::{BOOTSTRAP_LAYOUT, MARKER, verify_bootstrap_layout};
    use crate::dolt::{
        DOLT_VERSION, STORAGE_SCHEMA_VERSION, conversation_archive, is_commit_hash, is_uuid,
        validate_restored_records,
    };
    use crate::error::{ErrorCode, HiveError};
    use crate::workspace_backup::{
        DigestEntry, MAX_FILES, MAX_MANIFEST_BYTES, MAX_PAYLOAD_BYTES, digest_file, verify_db,
        visit_payload,
    };
    use crate::workspace_init::{
        Installed, fd_path, inspect_target_parent, lock_target, require_entries, restrict_db_modes,
        same_directory, sync_tree_with_links, verify_target_directory,
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
        #[serde(default, deserialize_with = "present_raw")]
        conversations: Option<Box<RawValue>>,
        files: Vec<DigestEntry>,
    }

    pub(super) fn restore(
        source: &Path,
        target: &Path,
        include_conversations: bool,
    ) -> Result<Value, HiveError> {
        let source = ScopedDirectory::open_absolute(&absolute_path(source)?)?;
        require_local_filesystem(&source)?;
        let (target, target_json) = InactiveTarget::inspect(&source.path, target)?;

        let manifest = read_manifest(&source)?;
        let conversations = if include_conversations {
            let section = manifest
                .conversations
                .as_ref()
                .ok_or_else(|| unverified("This backup has no conversation archive section."))?;
            Some(
                serde_json::from_str::<conversation_archive::Conversations>(section.get())
                    .map_err(|_| unverified("The conversation archive section is invalid."))?,
            )
        } else {
            None
        };
        let (_stage, stage_dir) = target.stage(&manifest.workspace_id)?;
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
        copy_payload(
            &source,
            &snapshot,
            manifest.files,
            manifest.conversations_included,
        )?;

        let scratch_dir = create_private_directory(&scratch)?;
        let payload_dir = create_private_directory(&payload)?;
        lock_target(&payload_dir)?;
        let assets = payload.join("assets");
        create_private_directory(&assets)?;
        let conversation_dir = create_private_directory(&payload.join("conversations"))?;
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
        verify_staged_db(
            &installed,
            &db,
            &manifest.workspace_id,
            &manifest.dolt_commit,
        )?;

        let links = restore_assets(
            &installed,
            &assets,
            &format!("{snapshot_path}/assets.bundle"),
            &manifest.asset_git_commit,
        )?;
        let queens = validate_restored_records(
            &installed,
            &db,
            &manifest.dolt_commit,
            &manifest.workspace_id,
            |path| installed.read_committed_soul(&assets, &manifest.asset_git_commit, path),
        )?;
        if let Some(conversations) = conversations {
            conversations.restore(&source, &conversation_dir, &manifest.workspace_id, &queens)?;
        }
        verify_bootstrap_layout(&payload_dir)?;
        let mut descriptor = private_file(&payload.join(MARKER))?;
        write!(
            descriptor,
            "version: 2\nworkspace_id: {}\nschema_version: {STORAGE_SCHEMA_VERSION}\npolicy_revision: {}\nbootstrap_layout: {BOOTSTRAP_LAYOUT}\n",
            manifest.workspace_id, manifest.policy_revision,
        ).map_err(io_error)?;
        sync_tree_with_links(&payload_dir, Path::new(""), &links)?;

        target.publish(&manifest.workspace_id, &stage_dir, &payload_dir)?;

        Ok(json!({
            "workspace_root": target_json,
            "workspace_id": manifest.workspace_id,
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "policy_revision": manifest.policy_revision,
            "dolt_version": DOLT_VERSION,
            "db_commit": manifest.dolt_commit,
            "asset_commit": manifest.asset_git_commit,
            "active_root": "inactive",
            "conversations_restored": include_conversations,
        }))
    }

    // Hold the original target lock from admission through verification and publication.
    struct InactiveTarget {
        path: PathBuf,
        parent: File,
        original: Option<File>,
    }

    impl InactiveTarget {
        fn inspect(source: &Path, target: &Path) -> Result<(Self, String), HiveError> {
            let path = absolute_path(target)?;
            if path.starts_with(source) || source.starts_with(&path) {
                return Err(invalid("Select a target separate from the backup source."));
            }
            let target_json = json_path(&path)?.to_owned();
            let parent = inspect_target_parent(&path)?;
            let name = path.file_name().expect("inspected target name");
            let original = open_target(&fd_path(&parent).join(name))?;
            if let Some(original) = &original {
                verify_target_directory(&parent, original)?;
                lock_target(original)?;
                require_entries(original, &[])?;
            }
            Ok((
                Self {
                    path,
                    parent,
                    original,
                },
                target_json,
            ))
        }

        fn verify_inactive(&self, workspace_id: &str) -> Result<(), HiveError> {
            require_inactive_target(workspace_id, &self.path)
        }

        fn stage(&self, workspace_id: &str) -> Result<(tempfile::TempDir, File), HiveError> {
            self.verify_inactive(workspace_id)?;
            let stage = tempfile::Builder::new()
                .prefix(crate::config_v2::INIT_STAGE_PREFIX)
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(fd_path(&self.parent))
                .map_err(io_error)?;
            let directory = open_target(stage.path())?.ok_or_else(changed_target)?;
            directory
                .set_permissions(fs::Permissions::from_mode(0o700))
                .map_err(io_error)?;
            Ok((stage, directory))
        }

        // The caller verifies and syncs the payload before making it selectable.
        fn publish(
            &self,
            workspace_id: &str,
            stage: &File,
            payload: &File,
        ) -> Result<(), HiveError> {
            let current_parent = inspect_target_parent(&self.path)?;
            self.verify_inactive(workspace_id)?;
            let name = self.path.file_name().expect("inspected target name");
            let target_entry = fd_path(&self.parent).join(name);
            let staged_payload =
                open_target(&fd_path(stage).join("workspace"))?.ok_or_else(changed_target)?;
            if !same_directory(&self.parent, &current_parent)?
                || !same_directory(payload, &staged_payload)?
            {
                return Err(changed_target());
            }
            let flags = if let Some(original) = &self.original {
                let current = open_target(&target_entry)?.ok_or_else(changed_target)?;
                if !same_directory(original, &current)? {
                    return Err(changed_target());
                }
                verify_target_directory(&self.parent, &current)?;
                require_entries(original, &[])?;
                // Directory rename replaces only an empty directory.
                RenameFlags::empty()
            } else {
                RenameFlags::NOREPLACE
            };
            renameat_with(stage, "workspace", &self.parent, name, flags).map_err(|error| {
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
            self.parent.sync_all().map_err(|_| uncertain())?;
            let current_parent = inspect_target_parent(&self.path).map_err(|_| uncertain())?;
            let published = open_target(&target_entry)
                .map_err(|_| uncertain())?
                .ok_or_else(uncertain)?;
            if !same_directory(&self.parent, &current_parent).map_err(|_| uncertain())?
                || !same_directory(&published, payload).map_err(|_| uncertain())?
            {
                return Err(uncertain());
            }
            Ok(())
        }
    }

    fn verify_staged_db(
        installed: &Installed<'_>,
        db: &Path,
        workspace_id: &str,
        revision: &str,
    ) -> Result<(), HiveError> {
        // Pinned Dolt fsck walks the commit DAG and verifies each historical
        // root and its referenced objects as well as the store's chunks.
        installed.dolt(db, &["fsck", "--quiet"], None)?;
        if verify_db(installed, db, workspace_id)? != revision {
            return Err(unverified(
                "The restored Dolt revision differs from the manifest.",
            ));
        }
        installed.dolt(
            db,
            &["constraints", "verify", "--all", "--output-only"],
            None,
        )?;
        restrict_db_modes(db)
    }

    #[cfg(test)]
    mod migration_staging_tests;

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

    // Recognize the format envelope without decoding unselected conversation entries.
    fn present_raw<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Box<RawValue>>, D::Error> {
        Box::<RawValue>::deserialize(deserializer).map(Some)
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
        if !matches!(
            (
                manifest.format_version,
                manifest.conversations_included,
                &manifest.conversations
            ),
            (1, false, None) | (conversation_archive::FORMAT_VERSION, true, Some(_))
        ) || manifest
            .conversations
            .as_ref()
            .is_some_and(|section| !section.get().starts_with('{'))
            || manifest.storage_schema_version != STORAGE_SCHEMA_VERSION
            || manifest.dolt_version != DOLT_VERSION
            || manifest.consistency_state != "verified_stopped_committed"
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
        has_conversations: bool,
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

        visit_payload(source, has_conversations, |directory, name, path| {
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
    ) -> Result<HashSet<PathBuf>, HiveError> {
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
        let links = verify_checkout_tree(|| installed.git_command(assets), revision)?;
        git(&["-c", "core.symlinks=true", "reset", "--hard", revision])?;
        if !git(&["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
            return Err(unverified("The restored asset worktree is not clean."));
        }
        Ok(links
            .into_iter()
            .map(|link| Path::new("assets").join(link))
            .collect())
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

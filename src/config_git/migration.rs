use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::time::SystemTime;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::cli::is_slug;
use crate::config_file::{Coordination, ScopedDirectory};

mod artifact_archive;
mod directive_artifact_intake;
mod directive_history;
mod issue_resources;
mod json_object;
pub(crate) mod prepared_records;
mod queen_assets;
mod queen_preparation;
mod reviewed_records;
mod task_graph;
mod task_intake;
mod task_provenance;
mod work_artifact;
pub(crate) use queen_preparation::PreparedQueen;
pub(crate) use task_intake::StagingTask;
mod scope_plan;
pub(crate) use issue_resources::IssueResourceOutcome;
pub(crate) use scope_plan::{
    Area, PreparedResources, ResourceHold, ReviewedScopes, plan, with_scope_mapping,
};

const MAX_INVENTORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_TREE_BYTES: usize = 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXCLUDE_BYTES: usize = 64 * 1024;
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn is_portable_bot_memory_locator(bot: &str, memory_scope: Option<&str>) -> bool {
    fn portable_slug(slug: &str) -> bool {
        slug.len() <= 100
            && is_slug(slug)
            && !matches!(slug, "con" | "prn" | "aux" | "nul")
            && !slug
                .strip_prefix("com")
                .or_else(|| slug.strip_prefix("lpt"))
                .is_some_and(|suffix| suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_digit())
    }

    portable_slug(bot)
        && memory_scope.is_none_or(|scope| {
            matches!(scope, "_general" | "_hive-userdata") || portable_slug(scope)
        })
}

pub(crate) fn preflight(source: &Path, revision: &str) -> Result<Value, HiveError> {
    verified_source(source, revision, |source, inventory, _| {
        Ok(json!({
            "mode": "source_preflight",
            "source": source,
            "inventory": inventory.summary,
        }))
    })
}

#[derive(Serialize)]
struct SourceSnapshot<'a> {
    source_commit: &'a str,
    source_clean: bool,
    policy_pin: &'static str,
    hive_yml: Value,
    tree_oid: String,
    root_tree_sha256: String,
}

struct Inventory {
    entries: BTreeMap<String, InventoryEntry>,
    summary: Value,
}

struct InventoryEntry {
    mode: &'static str,
    oid: String,
    size: Option<usize>,
}

impl InventoryEntry {
    fn is_directory(&self) -> bool {
        self.size.is_none()
    }

    fn digest_fields(&self) -> String {
        let size = self
            .size
            .map_or_else(|| "-".into(), |size| size.to_string());
        format!("{}\0{}\0{size}\0", self.mode, self.oid)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    AmbiguousScopeOrCollision,
    ExternalBackendOwned,
    UnsplitLegacyScope,
    ConflictingTaskReview,
    DuplicateLegacyWorkId,
    ExecutionNeedsReconciliation,
    InvalidLegacyState,
    MissingTaskEntrypoint,
    MissingTypedSubmissionEvidence,
    TaskReviewBindingMismatch,
    UnresolvedCompletionObligation,
    UnresolvedTaskRelations,
    UnverifiedAcceptanceCriteria,
    UnverifiedTaskLabels,
    UnsupportedTaskMetadata,
    NavigationOnly,
    DirectiveBundleSupport,
    UnsupportedDirectiveShape,
    DuplicateLegacyDirective,
    UnsupportedArtifactShape,
    UnresolvedArtifactBundleMember,
    UnresolvedBinaryOrExternalOwner,
    RecordMarkdownTooLarge,
    UnsupportedDirectiveMetadata,
    UnsupportedArtifactMetadata,
    InvalidDirectiveStatus,
    UnclassifiedArtifactKind,
    InvalidArtifactStatus,
    MissingOrConflictingTitle,
    IncompleteHistoricalProvenance,
    MissingDirectiveApproval,
    UnverifiedDirectiveValidity,
    UnresolvedSupersessionGraph,
    InvalidSupersessionChain,
    UnverifiedArtifactAuthor,
    UnverifiedArtifactRetention,
    UnresolvedParentTask,
    MissingParentTaskEntrypoint,
    ParentTaskQuarantined,
    UnsupportedReviewedRecord,
    UnresolvedArtifactLinks,
}

fn verified_source<T>(
    source: &Path,
    revision: &str,
    inspect: impl FnOnce(
        &SourceSnapshot<'_>,
        &Inventory,
        &AssetObjectReader<'_>,
    ) -> Result<T, HiveError>,
) -> Result<T, HiveError> {
    if !source.is_absolute() || !is_full_lowercase_oid(revision) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Select an absolute local --source path and a full lowercase --source-commit.",
        ));
    }
    let select = || AssetRepository::select_worktree(ScopedDirectory::open_absolute(source)?);
    let repository = select()?;
    let verify_layout = || {
        repository.verify_layout().map_err(|error| {
            HiveError::new(
                error.code,
                "The v1 source must use a dedicated local Git object store.",
            )
        })
    };
    verify_layout()?;
    let exclude = read_local_exclude(&repository.git_dir)?;
    let source_index = read_local_index(&repository.git_dir)?;
    let config = repository
        .git_dir
        .read(OsStr::new("config"))?
        .unwrap_or_default();
    let reader = AssetObjectReader::new(&repository.objects, &config, revision.len())?;
    let git = || reader.command();
    let index_path = reader.repository.path().join("index");
    let unsupported_index = || {
        HiveError::new(
            ErrorCode::AuthorityUnverified,
            "The v1 source must have a readable standalone Git index.",
        )
    };
    let mut copy = fs::File::create(&index_path).map_err(|_| unsupported_index())?;
    let modified = source_index.stat.modified;
    // Git compares this timestamp to cached worktree mtimes to detect racy entries.
    copy.write_all(&source_index.bytes)
        .and_then(|()| copy.set_modified(modified))
        .map_err(|_| unsupported_index())?;
    if copy
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        != Some(modified)
    {
        return Err(unsupported_index());
    }
    drop(copy);
    // Decode the copy with no source Git directory or shared index available.
    // A split index must fail here before status can refresh source metadata.
    let index_probe = execute_asset(
        git()?.args(["ls-files", "--stage", "-z"]),
        4 * 1024 * 1024,
        None,
    )?;
    if !index_probe.status.success() {
        return Err(unsupported_index());
    }
    let check_tracked = || {
        repository
            .check_clean(
                revision,
                CleanWorktree::MigrationSource { index: &index_path },
            )
            .map_err(|error| {
                let message = match error.code {
                    ErrorCode::StaleRevision => {
                        "The v1 source HEAD differs from the selected commit."
                    }
                    ErrorCode::DirtyConflict => {
                        "The v1 source has staged, modified, or untracked entries."
                    }
                    _ => "Could not establish a clean v1 source worktree.",
                };
                HiveError::new(error.code, message)
            })
    };
    let index = check_tracked()?;
    let check_untracked = prepare_untracked_check(&repository, &reader, exclude.as_deref())?;
    check_untracked()?;
    let tree = committed_asset_tree(&git, revision)?;
    let raw_tree = snapshot_object(&git, "tree", &tree, MAX_TREE_BYTES)?;
    let inventory = inventory(&git, &tree, &raw_tree, &index)?;
    let (policy_pin, coordination) = crate::config::parse_workspace_sources(|path| {
        read_committed_asset_blob(&git, revision, path)
    })?;
    let hive_yml = match coordination {
        Some(Coordination::Local) => json!({"version": 1, "mode": "local"}),
        Some(Coordination::Remote(_)) => json!({"version": 1, "mode": "remote"}),
        None => Value::Null,
    };
    let snapshot = SourceSnapshot {
        source_commit: revision,
        source_clean: true,
        policy_pin,
        hive_yml,
        tree_oid: tree,
        root_tree_sha256: format!("{:x}", Sha256::digest(raw_tree)),
    };
    let result = inspect(&snapshot, &inventory, &reader);

    let changed = || {
        HiveError::new(
            ErrorCode::ConcurrentChange,
            "The v1 source repository changed during inspection.",
        )
    };
    if check_tracked()? != index {
        return Err(changed());
    }
    check_untracked()?;
    if read_local_exclude(&repository.git_dir)? != exclude
        || read_local_index(&repository.git_dir)? != source_index
    {
        return Err(changed());
    }
    verify_layout()?;
    if !repository.same_worktree(&select().map_err(|_| changed())?)? {
        return Err(changed());
    }
    result
}

#[derive(PartialEq, Eq)]
struct IndexSnapshot {
    bytes: Vec<u8>,
    stat: IndexStat,
}

#[derive(PartialEq, Eq)]
struct IndexStat {
    identity: (u64, u64),
    modified: SystemTime,
    changed: (i64, i64),
    len: u64,
}

impl IndexStat {
    fn read(file: &fs::File) -> std::io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            identity: (metadata.dev(), metadata.ino()),
            modified: metadata.modified()?,
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            len: metadata.len(),
        })
    }
}

fn read_local_index(git_dir: &ScopedDirectory) -> Result<IndexSnapshot, HiveError> {
    let file = git_dir
        .open_regular(OsStr::new("index"))
        .map_err(|error| HiveError::new(error.code, "Could not open the v1 source Git index."))?
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::AuthorityUnverified,
                "The v1 source Git index is missing.",
            )
        })?;
    let unreadable = |_| {
        HiveError::new(
            ErrorCode::IoError,
            "Could not read the v1 source Git index.",
        )
    };
    let stat = IndexStat::read(&file).map_err(unreadable)?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_INDEX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "The v1 source Git index exceeds 16 MiB.",
        ));
    }
    if bytes.len() as u64 != stat.len || IndexStat::read(&file).map_err(unreadable)? != stat {
        return Err(HiveError::new(
            ErrorCode::ConcurrentChange,
            "The v1 source Git index changed while it was read.",
        ));
    }
    Ok(IndexSnapshot { bytes, stat })
}

fn read_local_exclude(git_dir: &ScopedDirectory) -> Result<Option<Vec<u8>>, HiveError> {
    let invalid_exclude = |error: HiveError| {
        HiveError::new(
            error.code,
            "Could not open the v1 source Git info directory and exclude file as local paths.",
        )
    };
    let Some(info) = git_dir
        .open_child_directory(OsStr::new("info"))
        .map_err(invalid_exclude)?
    else {
        return Ok(None);
    };
    let Some(file) = info
        .open_regular(OsStr::new("exclude"))
        .map_err(invalid_exclude)?
    else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.take(MAX_EXCLUDE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            HiveError::new(
                ErrorCode::IoError,
                "Could not read the v1 source Git exclude file.",
            )
        })?;
    if bytes.len() > MAX_EXCLUDE_BYTES {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "The v1 source Git exclude file exceeds 64 KiB.",
        ));
    }
    Ok(Some(bytes))
}

fn prepare_untracked_check<'a>(
    repository: &'a AssetRepository,
    reader: &'a AssetObjectReader<'_>,
    exclude: Option<&[u8]>,
) -> Result<impl Fn() -> Result<(), HiveError> + 'a, HiveError> {
    let unverified = || {
        HiveError::new(
            ErrorCode::AuthorityUnverified,
            "Could not verify untracked entries in the v1 source worktree.",
        )
    };
    if let Some(exclude) = exclude {
        // Keep Git's standard exclude precedence under worktree .gitignore.
        let info = reader.repository.path().join("info");
        fs::create_dir(&info)
            .and_then(|()| fs::write(info.join("exclude"), exclude))
            .map_err(|_| unverified())?;
    }
    let setting = execute_asset(
        repository
            .status_command()?
            .args(["config", "--bool", "--get", "core.ignoreCase"]),
        6,
        None,
    )?;
    let ignore_case = match (setting.status.code(), setting.stdout.as_slice()) {
        (Some(0), b"true\n") => "core.ignoreCase=true",
        (Some(0), b"false\n") | (Some(1), b"") => "core.ignoreCase=false",
        _ => return Err(unverified()),
    };
    Ok(move || {
        let mut command = reader.command()?;
        // SAFETY: The selected worktree descriptor stays open until the child is reaped.
        let worktree = unsafe { repository.assets.inherit_directory(&mut command)? };
        let result = execute_asset(
            command.env("GIT_WORK_TREE", worktree).args([
                "-c",
                "core.excludesFile=/dev/null",
                "-c",
                "core.untrackedCache=false",
                "-c",
                ignore_case,
                "ls-files",
                "--others",
                "--exclude-standard",
                "--directory",
                "--no-empty-directory",
                "-z",
            ]),
            4 * 1024 * 1024,
            None,
        )?;
        if !result.status.success() {
            return Err(unverified());
        }
        if !result.stdout.is_empty() {
            return Err(HiveError::new(
                ErrorCode::DirtyConflict,
                "The v1 source has untracked entries.",
            ));
        }
        Ok(())
    })
}

fn snapshot_object(
    git: &impl Fn() -> Result<Command, HiveError>,
    kind: &str,
    oid: &str,
    limit: usize,
) -> Result<Vec<u8>, HiveError> {
    verified_asset_object(git, kind, oid, limit).map_err(|error| {
        let message = if matches!(error.code, ErrorCode::InvalidConfig) {
            "The v1 source exceeds its Git object verification size limit."
        } else {
            "Could not read and verify a committed v1 source object locally."
        };
        HiveError::new(error.code, message)
    })
}

fn inventory(
    git: &impl Fn() -> Result<Command, HiveError>,
    root_oid: &str,
    root_tree: &[u8],
    index: &[u8],
) -> Result<Inventory, HiveError> {
    let invalid = || {
        HiveError::new(
            ErrorCode::InvalidRecord,
            "The committed v1 source contains an unsupported or ambiguous Git tree entry.",
        )
    };
    let mut entries = BTreeMap::new();
    let mut verified_blobs = HashMap::new();
    let mut directories = 0;
    let mut blob_bytes = 0_u64;
    let mut catalog_bytes = 0;
    let mut remaining = MAX_SNAPSHOT_BYTES - root_tree.len();
    let mut pending = vec![(String::new(), root_oid.to_owned())];
    while let Some((parent, oid)) = pending.pop() {
        let tree = if parent.is_empty() {
            Cow::Borrowed(root_tree)
        } else {
            let bytes = snapshot_object(git, "tree", &oid, remaining.min(MAX_TREE_BYTES))?;
            remaining -= bytes.len();
            Cow::Owned(bytes)
        };
        let mut raw = tree.as_ref();
        while !raw.is_empty() {
            if entries.len() == MAX_ENTRIES {
                return Err(HiveError::new(
                    ErrorCode::InvalidConfig,
                    "The v1 source inventory exceeds 10,000 entries.",
                ));
            }
            let entry =
                take_asset_tree_entry(&mut raw, root_oid.len() / 2).map_err(|_| invalid())?;
            let name = std::str::from_utf8(entry.name).map_err(|_| invalid())?;
            if matches!(name, "" | "." | "..")
                || name.eq_ignore_ascii_case(".git")
                || name
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '/' | '\\'))
            {
                return Err(invalid());
            }
            let path = if parent.is_empty() {
                name.to_owned()
            } else {
                format!("{parent}/{name}")
            };
            let oid = entry.hex_oid();
            let (mode, size) = match entry.mode {
                b"40000" => {
                    directories += 1;
                    pending.push((path.clone(), oid.clone()));
                    ("040000", None)
                }
                b"100644" | b"100755" => {
                    let size = if let Some(&size) = verified_blobs.get(&oid) {
                        size
                    } else {
                        let size = snapshot_object(git, "blob", &oid, remaining)?.len();
                        remaining -= size;
                        verified_blobs.insert(oid.clone(), size);
                        size
                    };
                    blob_bytes += size as u64;
                    let mode = if entry.mode == b"100644" {
                        "100644"
                    } else {
                        "100755"
                    };
                    (mode, Some(size))
                }
                _ => return Err(invalid()),
            };
            let entry = InventoryEntry { mode, oid, size };
            catalog_bytes += path.len() + 1 + entry.digest_fields().len();
            if catalog_bytes > MAX_INVENTORY_BYTES {
                return Err(HiveError::new(
                    ErrorCode::InvalidConfig,
                    "The v1 source inventory exceeds 4 MiB.",
                ));
            }
            if entries.insert(path, entry).is_some() {
                return Err(invalid());
            }
        }
    }
    let mut digest = Sha256::new();
    let inconsistent_index = || {
        HiveError::new(
            ErrorCode::AuthorityUnverified,
            "The v1 source index does not match the verified commit.",
        )
    };
    let mut index = index.split_inclusive(|byte| *byte == 0);
    for (path, entry) in &entries {
        digest.update(path);
        digest.update(b"\0");
        digest.update(entry.digest_fields());
        if !entry.is_directory() {
            let expected = format!("H {} {} 0\t{path}\0", entry.mode, entry.oid);
            if index.next() != Some(expected.as_bytes()) {
                return Err(inconsistent_index());
            }
        }
    }
    if index.next().is_some() {
        return Err(inconsistent_index());
    }
    let summary = json!({
        "directory_count": directories,
        "file_count": entries.len() - directories,
        "blob_bytes": blob_bytes,
        "catalog_sha256": format!("{:x}", digest.finalize()),
        "rulesync_directory": entries.get(".rulesync").is_some_and(InventoryEntry::is_directory),
        "rulesync_config": entries.get("rulesync.jsonc").is_some_and(|entry| !entry.is_directory()),
    });
    Ok(Inventory { entries, summary })
}

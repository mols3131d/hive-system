use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::config_file::{Coordination, ScopedDirectory};

mod scope_plan;
pub(crate) use scope_plan::plan;

const MAX_INVENTORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_TREE_BYTES: usize = 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

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

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    AmbiguousScopeOrCollision,
    ExternalBackendOwned,
    UnsplitLegacyScope,
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
    let verify_local_exclude = || -> Result<(), HiveError> {
        let invalid_exclude = |error: HiveError| {
            HiveError::new(
                error.code,
                "The v1 source Git info directory and exclude file must be local when present.",
            )
        };
        if let Some(info) = repository
            .git_dir
            .open_child_directory(OsStr::new("info"))
            .map_err(invalid_exclude)?
        {
            info.open_regular(OsStr::new("exclude"))
                .map_err(invalid_exclude)?;
        }
        Ok(())
    };
    let check_clean = || {
        // Git status follows links in the repository's info/exclude path.
        verify_local_exclude()?;
        repository
            .check_clean(revision, CleanWorktree::MigrationSource)
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
            })?;
        verify_local_exclude()
    };
    check_clean()?;

    let config = repository
        .git_dir
        .read(OsStr::new("config"))?
        .unwrap_or_default();
    let reader = AssetObjectReader::new(&repository.objects, &config, revision.len())?;
    let git = || reader.command();
    let tree = committed_asset_tree(&git, revision)?;
    let raw_tree = snapshot_object(&git, "tree", &tree, MAX_TREE_BYTES)?;
    let inventory = inventory(&git, &tree, &raw_tree)?;
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

    check_clean()?;
    verify_layout()?;
    let changed = || {
        HiveError::new(
            ErrorCode::ConcurrentChange,
            "The v1 source repository changed during inspection.",
        )
    };
    if !repository.same_worktree(&select().map_err(|_| changed())?)? {
        return Err(changed());
    }
    result
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
    for (path, entry) in &entries {
        digest.update(path);
        digest.update(b"\0");
        digest.update(entry.digest_fields());
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

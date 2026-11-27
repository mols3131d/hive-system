use std::collections::BTreeMap;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::config_file::{Coordination, ScopedDirectory};

const MAX_INVENTORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

pub(crate) fn preflight(source: &Path, revision: &str) -> Result<Value, HiveError> {
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
    let check_clean = || {
        repository
            .check_clean(revision, false, false)
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
    check_clean()?;

    let config = repository
        .git_dir
        .read(OsStr::new("config"))?
        .unwrap_or_default();
    let reader = AssetObjectReader::new(&repository.objects, &config, revision.len())?;
    let git = || reader.command();
    let tree = committed_asset_tree(&git, revision)?;
    let raw_tree = verified_asset_object(&git, "tree", &tree, 1024 * 1024)?;
    let inventory = inventory(&git, &tree)?;
    let (policy_pin, coordination) = crate::config::parse_workspace_sources(|path| {
        read_committed_asset_blob(&git, revision, path)
    })?;
    let hive_yml = match coordination {
        Some(Coordination::Local) => json!({"version": 1, "mode": "local"}),
        Some(Coordination::Remote(_)) => json!({"version": 1, "mode": "remote"}),
        None => Value::Null,
    };

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
    Ok(json!({
        "mode": "source_preflight",
        "source": {
            "source_commit": revision,
            "source_clean": true,
            "policy_pin": policy_pin,
            "hive_yml": hive_yml,
            "tree_oid": tree,
            "root_tree_sha256": format!("{:x}", Sha256::digest(raw_tree)),
        },
        "inventory": inventory,
    }))
}

fn inventory(
    git: &impl Fn() -> Result<Command, HiveError>,
    tree: &str,
) -> Result<Value, HiveError> {
    let invalid = || {
        HiveError::new(
            ErrorCode::InvalidRecord,
            "The committed v1 source contains an unsupported or ambiguous Git tree entry.",
        )
    };
    let unavailable = || {
        HiveError::new(
            ErrorCode::ObjectUnavailable,
            "The committed v1 source tree or blob metadata is unavailable locally.",
        )
    };
    let listing = execute_asset(
        git()?.args(["ls-tree", "-r", "-t", "-z", "-l", "--full-tree", tree]),
        MAX_INVENTORY_BYTES,
        None,
    )
    .map_err(|error| {
        if matches!(error.code, ErrorCode::InvalidConfig) {
            HiveError::new(
                ErrorCode::InvalidConfig,
                "The v1 source inventory exceeds 4 MiB.",
            )
        } else {
            unavailable()
        }
    })?;
    if !listing.status.success() {
        return Err(unavailable());
    }
    let mut entries = BTreeMap::new();
    let mut directories = 0;
    let mut blob_bytes = 0_u64;
    let mut records = listing.stdout.split(|byte| *byte == 0);
    if records.next_back() != Some(&[][..]) {
        return Err(invalid());
    }
    for entry in records {
        if entries.len() == MAX_ENTRIES {
            return Err(HiveError::new(
                ErrorCode::InvalidConfig,
                "The v1 source inventory exceeds 10,000 entries.",
            ));
        }
        let entry = std::str::from_utf8(entry).map_err(|_| invalid())?;
        let (fields, path) = entry.split_once('\t').ok_or_else(invalid)?;
        let fields: Vec<_> = fields.split_ascii_whitespace().collect();
        let [mode, kind, oid, size] = fields.as_slice() else {
            return Err(invalid());
        };
        if oid.len() != tree.len()
            || !is_full_lowercase_oid(oid)
            || path.chars().any(|c| c.is_control() || c == '\\')
            || path
                .split('/')
                .any(|part| matches!(part, "" | "." | "..") || part.eq_ignore_ascii_case(".git"))
        {
            return Err(invalid());
        }
        let size = match (*mode, *kind, *size) {
            ("040000", "tree", "-") => {
                directories += 1;
                None
            }
            ("100644" | "100755", "blob", size) => {
                let size = size.parse::<u64>().map_err(|_| unavailable())?;
                blob_bytes = blob_bytes.checked_add(size).ok_or_else(invalid)?;
                Some(size)
            }
            _ => return Err(invalid()),
        };
        if entries.insert(path, (*mode, *oid, size)).is_some() {
            return Err(invalid());
        }
    }
    let mut digest = Sha256::new();
    for (path, (mode, oid, size)) in &entries {
        // Use NUL delimiters and path order, independent of Git's display padding.
        digest.update(format!(
            "{path}\0{mode}\0{oid}\0{}\0",
            size.map_or_else(|| "-".into(), |n| n.to_string())
        ));
    }
    Ok(json!({
        "directory_count": directories,
        "file_count": entries.len() - directories,
        "blob_bytes": blob_bytes,
        "catalog_sha256": format!("{:x}", digest.finalize()),
        "rulesync_directory": entries.get(".rulesync").is_some_and(|entry| entry.2.is_none()),
        "rulesync_config": entries.get("rulesync.jsonc").is_some_and(|entry| entry.2.is_some()),
    }))
}

use std::path::Path;

use serde_json::Value;

use crate::error::HiveError;

#[cfg(target_os = "linux")]
#[path = "workspace_init/linux.rs"]
mod linux;

#[cfg(target_os = "linux")]
pub(crate) use linux::{Installed, fd_path, inspect_empty_target, probe_versions, sync_tree};
#[cfg(target_os = "linux")]
pub(crate) use linux::{
    inspect_target_parent, lock_target, require_entries, restrict_db_modes, same_directory,
    sync_tree_with_links, verify_target_directory,
};

pub(crate) fn run(target: &Path, register_runtime: bool) -> Result<Value, HiveError> {
    #[cfg(target_os = "linux")]
    {
        linux::run(target, register_runtime)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (target, register_runtime);
        Err(HiveError::new(
            crate::error::ErrorCode::NotImplemented,
            "Workspace initialization is supported only on Linux.",
        ))
    }
}

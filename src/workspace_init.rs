use std::path::Path;

use serde_json::Value;

use crate::error::HiveError;

#[cfg(target_os = "linux")]
#[path = "workspace_init/linux.rs"]
mod linux;

#[cfg(target_os = "linux")]
pub(crate) use linux::{inspect_empty_target, probe_versions};

pub(crate) fn run(target: &Path) -> Result<Value, HiveError> {
    #[cfg(target_os = "linux")]
    {
        linux::run(target)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = target;
        Err(HiveError::new(
            crate::error::ErrorCode::NotImplemented,
            "Workspace initialization is supported only on Linux.",
        ))
    }
}

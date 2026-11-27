use std::path::Path;

use serde_json::Value;

use crate::error::HiveError;

#[cfg(target_os = "linux")]
#[path = "workspace_init/linux.rs"]
mod linux;

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

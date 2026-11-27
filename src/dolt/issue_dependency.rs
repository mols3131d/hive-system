//! Guarded planning edits to the prerequisite graph.

use crate::cli::{CommonOptions, IssueDependencyArgs};

use super::{DoltFailure, DoltSuccess};

pub(crate) fn write(
    common: &CommonOptions,
    args: &IssueDependencyArgs,
    add: bool,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::write(common, args, add).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args, add);
        Err(DoltFailure::without_context(crate::error::HiveError::new(
            crate::error::ErrorCode::NotImplemented,
            "Issue dependency edits require a verified Linux workspace runtime.",
        )))
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::{ScopedDependency, check_receipt};

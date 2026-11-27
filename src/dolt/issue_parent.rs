//! Guarded planning edits to the Issue parent hierarchy.

use crate::cli::{CommonOptions, IssueParentArgs};

use super::{DoltFailure, DoltSuccess};

pub(crate) fn write(
    common: &CommonOptions,
    args: &IssueParentArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::write(common, args).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args);
        Err(DoltFailure::without_context(crate::error::HiveError::new(
            crate::error::ErrorCode::NotImplemented,
            "Issue parent edits require a verified Linux workspace runtime.",
        )))
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::{ScopedParent, check_receipt};

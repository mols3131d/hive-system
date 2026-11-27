//! Scoped edits to the ordinary Issue fields.

use crate::cli::{CommonOptions, IssueUpdateArgs};

use super::{DoltFailure, DoltSuccess};

pub(crate) fn write(
    common: &CommonOptions,
    args: &IssueUpdateArgs,
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
            "Issue update requires a verified Linux workspace runtime.",
        )))
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::{KIND, ScopedUpdate, check_scope, verify_effect};

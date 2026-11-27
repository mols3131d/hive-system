//! Issue creation and proof of its committed counter allocation.

use crate::cli::{CommonOptions, IssueCreateArgs};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess};

pub(crate) fn write(
    common: &CommonOptions,
    args: &IssueCreateArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::write(common, args).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args);
        Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            "Issue create requires a verified Linux workspace runtime.",
        )))
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::{CreateResult, KIND, check_scope, verify_effect};

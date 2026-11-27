//! A holder-scoped progress note with a durable local commit intent.

use crate::cli::{CommonOptions, IssueNoteArgs};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess};

pub(crate) fn write(
    common: &CommonOptions,
    args: &IssueNoteArgs,
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
            "Issue note requires a verified Linux workspace runtime.",
        )))
    }
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub(crate) use linux::Pending;

#[cfg(target_os = "linux")]
pub(crate) fn uncertain() -> HiveError {
    HiveError::new(
        ErrorCode::CommitUncertain,
        "The pending Issue note outcome could not be established.",
    )
}

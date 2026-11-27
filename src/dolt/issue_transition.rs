//! Guarded Issue lifecycle transitions.

use crate::cli::{
    CommonOptions, IssueReleaseArgs, IssueReviewArgs, IssueSubmitArgs, IssueSubmitEpicArgs,
    IssueTransitionArgs,
};

use super::{DoltFailure, DoltSuccess};

pub(crate) fn write(
    common: &CommonOptions,
    args: &IssueTransitionArgs,
    holder: Option<&str>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::write(common, args, holder).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args, holder);
        unavailable()
    }
}

pub(crate) fn release(
    common: &CommonOptions,
    args: &IssueReleaseArgs,
    recovery: bool,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::release(common, args, recovery).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args, recovery);
        unavailable()
    }
}

pub(crate) fn submit(
    common: &CommonOptions,
    args: &IssueSubmitArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::submit(common, args).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args);
        unavailable()
    }
}

pub(crate) fn submit_epic(
    common: &CommonOptions,
    args: &IssueSubmitEpicArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::submit_epic(common, args).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args);
        unavailable()
    }
}

pub(crate) fn review(
    common: &CommonOptions,
    args: &IssueReviewArgs,
    acknowledged: Option<&[String]>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::review(common, args, acknowledged).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args, acknowledged);
        unavailable()
    }
}

#[cfg(not(target_os = "linux"))]
fn unavailable() -> Result<DoltSuccess, Box<DoltFailure>> {
    Err(DoltFailure::without_context(crate::error::HiveError::new(
        crate::error::ErrorCode::NotImplemented,
        "Issue transitions require a verified Linux workspace runtime.",
    )))
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use super::issue_readiness::Blocker;
#[cfg(target_os = "linux")]
pub(super) use linux::{Claim, Operation, Record, check_scope, verify_effect};

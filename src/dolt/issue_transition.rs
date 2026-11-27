//! Guarded Issue lifecycle transitions.

use crate::cli::{CommonOptions, IssueTransitionArgs};

use super::{DoltFailure, DoltSuccess};

pub(crate) enum Input<'a> {
    Select,
    Claim(&'a str),
    Cancel(&'a str),
    Reopen(&'a str),
}

pub(crate) fn write(
    common: &CommonOptions,
    args: &IssueTransitionArgs,
    input: Input<'_>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    #[cfg(target_os = "linux")]
    {
        linux::write(common, args, input).map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (common, args, input);
        Err(DoltFailure::without_context(crate::error::HiveError::new(
            crate::error::ErrorCode::NotImplemented,
            "Issue transitions require a verified Linux workspace runtime.",
        )))
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use super::issue_readiness::Blocker;
#[cfg(target_os = "linux")]
pub(super) use linux::{Operation, Record, check_scope, verify_effect};

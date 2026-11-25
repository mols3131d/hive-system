use serde::Serialize;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(
    dead_code,
    reason = "The v1 contract defines codes before their operations are implemented"
)]
pub(crate) enum ErrorCode {
    InternalError,
    InvalidArgument,
    InvalidConfig,
    AmbiguousScope,
    ContextLimit,
    NotFound,
    InvalidRecord,
    InvalidTransition,
    IncompatiblePolicy,
    AlreadyExists,
    AlreadyClaimed,
    ClaimMismatch,
    StaleRevision,
    ConcurrentChange,
    DirtyConflict,
    Unauthorized,
    AuthorityUnavailable,
    ObjectUnavailable,
    PolicyUnavailable,
    IoError,
    NotImplemented,
    UnknownWriteOutcome,
}

impl ErrorCode {
    pub(crate) const fn exit_status(self) -> u8 {
        match self {
            Self::InternalError => 1,
            Self::InvalidArgument
            | Self::InvalidConfig
            | Self::AmbiguousScope
            | Self::ContextLimit => 2,
            Self::NotFound => 3,
            Self::InvalidRecord | Self::InvalidTransition | Self::IncompatiblePolicy => 4,
            Self::AlreadyExists
            | Self::AlreadyClaimed
            | Self::ClaimMismatch
            | Self::StaleRevision
            | Self::ConcurrentChange
            | Self::DirtyConflict => 5,
            Self::Unauthorized => 6,
            Self::AuthorityUnavailable
            | Self::ObjectUnavailable
            | Self::PolicyUnavailable
            | Self::IoError
            | Self::NotImplemented => 7,
            Self::UnknownWriteOutcome => 8,
        }
    }
}

#[derive(Debug, Error, Serialize)]
#[error("{message}")]
pub(crate) struct HiveError {
    pub(crate) code: ErrorCode,
    message: String,
}

impl HiveError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    #[test]
    fn v1_error_codes_match_their_wire_names_and_exit_statuses() {
        use ErrorCode::*;

        for (code, name, status) in [
            (InternalError, "INTERNAL_ERROR", 1),
            (InvalidArgument, "INVALID_ARGUMENT", 2),
            (InvalidConfig, "INVALID_CONFIG", 2),
            (AmbiguousScope, "AMBIGUOUS_SCOPE", 2),
            (ContextLimit, "CONTEXT_LIMIT", 2),
            (NotFound, "NOT_FOUND", 3),
            (InvalidRecord, "INVALID_RECORD", 4),
            (InvalidTransition, "INVALID_TRANSITION", 4),
            (IncompatiblePolicy, "INCOMPATIBLE_POLICY", 4),
            (AlreadyExists, "ALREADY_EXISTS", 5),
            (AlreadyClaimed, "ALREADY_CLAIMED", 5),
            (ClaimMismatch, "CLAIM_MISMATCH", 5),
            (StaleRevision, "STALE_REVISION", 5),
            (ConcurrentChange, "CONCURRENT_CHANGE", 5),
            (DirtyConflict, "DIRTY_CONFLICT", 5),
            (Unauthorized, "UNAUTHORIZED", 6),
            (AuthorityUnavailable, "AUTHORITY_UNAVAILABLE", 7),
            (ObjectUnavailable, "OBJECT_UNAVAILABLE", 7),
            (PolicyUnavailable, "POLICY_UNAVAILABLE", 7),
            (IoError, "IO_ERROR", 7),
            (NotImplemented, "NOT_IMPLEMENTED", 7),
            (UnknownWriteOutcome, "UNKNOWN_WRITE_OUTCOME", 8),
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), name);
            assert_eq!(code.exit_status(), status, "{name}");
        }
    }
}

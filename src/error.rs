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
    SchemaIncompatible,
    ConstraintViolation,
    AlreadyExists,
    AlreadyClaimed,
    ClaimMismatch,
    StaleRevision,
    StaleCursor,
    ConcurrentChange,
    DirtyConflict,
    Unauthorized,
    AuthorityUnavailable,
    AuthorityUnverified,
    RuntimeUnavailable,
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
            Self::InvalidRecord
            | Self::InvalidTransition
            | Self::IncompatiblePolicy
            | Self::SchemaIncompatible
            | Self::ConstraintViolation => 4,
            Self::AlreadyExists
            | Self::AlreadyClaimed
            | Self::ClaimMismatch
            | Self::StaleRevision
            | Self::StaleCursor
            | Self::ConcurrentChange
            | Self::DirtyConflict => 5,
            Self::Unauthorized => 6,
            Self::AuthorityUnavailable
            | Self::AuthorityUnverified
            | Self::RuntimeUnavailable
            | Self::ObjectUnavailable
            | Self::PolicyUnavailable
            | Self::IoError
            | Self::NotImplemented => 7,
            Self::UnknownWriteOutcome => 8,
        }
    }
}

#[derive(Clone, Debug, Error, Serialize)]
#[error("{message}")]
pub(crate) struct HiveError {
    pub(crate) code: ErrorCode,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<ErrorDetails>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum ErrorDetails {
    Legacy {
        diagnostics: [Diagnostic; 1],
    },
    Records {
        diagnostics: Vec<RecordDiagnostic>,
        truncated: bool,
    },
}

#[derive(Clone, Debug, Serialize)]
struct Diagnostic {
    code: ErrorCode,
    severity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    message: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RecordDiagnostic {
    pub(crate) code: &'static str,
    pub(crate) severity: &'static str,
    pub(crate) table: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) record_ids: Vec<String>,
    pub(crate) message: &'static str,
}

impl HiveError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn diagnostic(mut self, path: Option<&str>) -> Self {
        if self.details.is_none() {
            self.details = Some(ErrorDetails::Legacy {
                diagnostics: [Diagnostic {
                    code: self.code,
                    severity: "error",
                    path: path.map(str::to_owned),
                    message: self.message.clone(),
                }],
            });
        }
        self
    }

    pub(crate) fn record_violations(diagnostics: Vec<RecordDiagnostic>, truncated: bool) -> Self {
        Self {
            code: ErrorCode::ConstraintViolation,
            message: "Record validation failed at the selected Dolt commit.".to_owned(),
            details: Some(ErrorDetails::Records {
                diagnostics,
                truncated,
            }),
        }
    }

    pub(crate) fn record_diagnostics(&self) -> Option<(&[RecordDiagnostic], bool)> {
        match &self.details {
            Some(ErrorDetails::Records {
                diagnostics,
                truncated,
            }) => Some((diagnostics, *truncated)),
            _ => None,
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

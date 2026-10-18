//! Typed error types for the Meld domain layer.

use thiserror::Error;

/// Top-level error type for domain-level failures.
#[derive(Debug, Error, Clone)]
pub enum MeldError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unsupported operation: {0}")]
    Unsupported(String),

    #[error("unsupported feature: {0}")]
    UnsupportedFeature(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("integrity check failed: {0}")]
    Integrity(String),

    #[error("incompatible engine kinds: expected {expected:?}, got {got:?}")]
    IncompatibleEngine {
        expected: crate::state::EngineKind,
        got: crate::state::EngineKind,
    },

    #[error("incompatible snapshot scope: {0}")]
    IncompatibleScope(String),

    #[error("incompatible format version: expected {expected}, got {got}")]
    IncompatibleFormatVersion { expected: u32, got: u32 },

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("commit graph error: {0}")]
    CommitGraph(String),

    #[error("reference error: {0}")]
    Reference(String),

    #[error("verification failed: {0}")]
    VerificationFailed(String),

    #[error("plan validation failed: {0}")]
    PlanValidation(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("configuration error: {0}")]
    Config(String),
}

impl MeldError {
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            MeldError::InvalidInput(_)
                | MeldError::NotFound(_)
                | MeldError::Unsupported(_)
                | MeldError::UnsupportedFeature(_)
                | MeldError::Conflict(_)
                | MeldError::PlanValidation(_)
                | MeldError::VerificationFailed(_)
        )
    }
}

#[derive(Debug, Error, Clone)]
#[error("{message} (severity: {severity:?})")]
pub struct DomainError {
    pub message: String,
    pub severity: ErrorSeverity,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

impl std::fmt::Display for ErrorSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorSeverity::Info => write!(f, "info"),
            ErrorSeverity::Warning => write!(f, "warning"),
            ErrorSeverity::Error => write!(f, "error"),
            ErrorSeverity::Critical => write!(f, "critical"),
        }
    }
}

//! Application-level error types for the Meld engine.
//!
//! The engine orchestrates domain logic, the local store, and database
//! adapters. Errors here are workflow-level: snapshot, restore, merge,
//! branch, and verification failures.

use meld_core::error::MeldError;
use thiserror::Error;

/// Engine-level error. Wraps domain errors and adds workflow context.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("session error: {0}")]
    Session(String),
    #[error("snapshot error: {0}")]
    Snapshot(String),
    #[error("restore error: {0}")]
    Restore(String),
    #[error("checkout error: {0}")]
    Checkout(String),
    #[error("merge error: {0}")]
    Merge(String),
    #[error("verification error: {0}")]
    Verification(String),
    #[error("branch error: {0}")]
    Branch(String),
    #[error("drift error: {0}")]
    Drift(String),
    #[error("diff error: {0}")]
    Diff(String),
    #[error("incompatible snapshot scope: {0}")]
    IncompatibleScope(String),
    #[error("domain error: {0}")]
    Domain(#[from] MeldError),
    #[error("io error: {0}")]
    Io(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("incompatible engine kinds: expected {expected:?}, got {got:?}")]
    IncompatibleEngine {
        expected: meld_core::state::EngineKind,
        got: meld_core::state::EngineKind,
    },
}

impl EngineError {
    pub fn is_recoverable(&self) -> bool {
        !matches!(
            self,
            EngineError::Internal(_) | EngineError::Domain(MeldError::Integrity(_))
        )
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::Io(e.to_string())
    }
}

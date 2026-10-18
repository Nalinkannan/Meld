//! Diff between two commits (placeholder stub).

use meld_core::ids::CommitHash;

use crate::error::EngineError;

/// Diff between two commit states.
pub async fn diff(
    _store: &meld_store::Store,
    _from: &CommitHash,
    _to: &CommitHash,
) -> Result<meld_core::diff::SnapshotDiff, EngineError> {
    Err(EngineError::Unsupported("diff not yet implemented".into()))
}

/// Diff between a commit and the live database (drift).
pub async fn drift(
    _adapter: &dyn meld_adapter::DatabaseAdapter,
    _commit: &CommitHash,
) -> Result<meld_core::diff::SnapshotDiff, EngineError> {
    Err(EngineError::Unsupported("drift not yet implemented".into()))
}

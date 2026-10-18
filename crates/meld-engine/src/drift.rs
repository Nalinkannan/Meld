//! Drift detection against the current HEAD (placeholder stub).

use meld_core::ids::CommitHash;

use crate::error::EngineError;

/// Detect drift between HEAD and the live database.
pub async fn drift(
    _adapter: &dyn meld_adapter::DatabaseAdapter,
    _head: &CommitHash,
) -> Result<meld_core::diff::SnapshotDiff, EngineError> {
    Err(EngineError::Unsupported("drift not yet implemented".into()))
}

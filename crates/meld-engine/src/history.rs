//! Commit history traversal (placeholder stub).

use crate::error::EngineError;

/// List commits reachable from HEAD (newest first).
pub async fn history(
    _store: &meld_store::Store,
    _limit: Option<usize>,
) -> Result<Vec<meld_core::commit::Commit>, EngineError> {
    Err(EngineError::Unsupported(
        "history not yet implemented".into(),
    ))
}

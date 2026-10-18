//! Three-way merge (placeholder stub).

use meld_core::ids::CommitHash;

use crate::error::EngineError;

/// Input to a three-way merge.
pub struct MergeInput {
    pub ancestor: CommitHash,
    pub left: CommitHash,
    pub right: CommitHash,
}

/// Result of merge analysis.
pub struct MergeAnalysis {
    pub ancestor: CommitHash,
    pub left: CommitHash,
    pub right: CommitHash,
    pub engine_kind: meld_core::state::EngineKind,
    pub conflicts: Vec<meld_core::conflict::ConflictReport>,
}

pub async fn analyze_merge(
    _store: &meld_store::Store,
    _input: MergeInput,
) -> Result<MergeAnalysis, EngineError> {
    Err(EngineError::Unsupported("merge not yet implemented".into()))
}

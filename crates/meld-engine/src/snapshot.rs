//! Snapshot creation workflow.
//!
//! Captures the live database state, canonicalizes it, diffs it against the
//! current branch HEAD, persists an immutable commit, and advances the branch
//! and HEAD references atomically.

use meld_core::ids::{CommitHash, ObjectHash};

use crate::error::EngineError;
use crate::session::Session;

/// Result of creating a snapshot.
pub struct SnapshotResult {
    pub commit: meld_core::commit::Commit,
    pub commit_hash: CommitHash,
    pub root_hash: ObjectHash,
    pub change_summary: meld_core::transition::ChangeSummary,
    pub consistency: meld_core::state::ConsistencyLevel,
    pub warnings: Vec<String>,
}

/// Capture a snapshot of the live database and persist a commit on the
/// current branch.
pub async fn snapshot(
    session: &mut Session,
    message: &str,
    author: &str,
) -> Result<SnapshotResult, EngineError> {
    let adapter = session.adapter_mut()?;

    // 1. Introspect the live database (fresh, read-only capture).
    let state = adapter
        .introspect()
        .await
        .map_err(|e| EngineError::Snapshot(e.to_string()))?;

    // 2. Root state hash (content address of the canonical state).
    let root_hash = ObjectHash::from_bytes(meld_core::canonical::hash_state(&state).as_bytes());

    // 3. Semantic diff against the current branch HEAD, if any.
    let change_summary = match session.head()? {
        Some(head) => {
            let head_commit = session
                .store
                .get_commit(&head)
                .map_err(|e| EngineError::Snapshot(e.to_string()))?;
            let diff = meld_core::diff::diff_states(&head_commit.state, &state)
                .map_err(|e| EngineError::Snapshot(e.to_string()))?;
            diff.summary
        }
        None => meld_core::transition::ChangeSummary::default(),
    };

    // 4. Build the commit (parent = HEAD). The commit JSON carries the full
    //    state, so the commit is self-contained and content-addressed.
    let parents = match session.head()? {
        Some(h) => vec![h],
        None => vec![],
    };
    let commit = meld_core::commit::Commit::new(
        parents,
        author,
        message,
        state.engine_kind(),
        state.scope(),
        root_hash,
        change_summary.clone(),
        state,
    );

    // 5. Persist the commit (integrity-checked: stored hash == content hash).
    session
        .store
        .put_commit(&commit)
        .map_err(|e| EngineError::Snapshot(e.to_string()))?;

    // 6. Advance the current branch and HEAD atomically in a single store
    //    transaction: a failure leaves neither reference changed.
    let branch = session.current_branch.clone();
    let old_head = session
        .store
        .get_branch(&branch)
        .map_err(|e| EngineError::Snapshot(e.to_string()))?;
    session
        .store
        .compare_and_swap_branch_and_head(&branch, old_head.as_ref(), &commit.hash)
        .map_err(|e| EngineError::Snapshot(e.to_string()))?;

    let commit_hash = commit.hash;
    let (consistency, warnings) = match &commit.state {
        meld_core::state::EngineState::PostgreSql(s) => {
            (s.capture_info.consistency, s.capture_info.warnings.clone())
        }
        meld_core::state::EngineState::MongoDb(s) => {
            (s.capture_info.consistency, s.capture_info.warnings.clone())
        }
    };

    Ok(SnapshotResult {
        commit,
        commit_hash,
        root_hash,
        change_summary,
        consistency,
        warnings,
    })
}

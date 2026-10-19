//! Checkout and restore workflow.
//!
//! Restores the live database to the state captured at a target commit. The
//! flow is:
//!   1. Resolve the target commit and load its captured state.
//!   2. Build a dependency-aware plan by diffing the target against the live
//!      database.
//!   3. Validate the plan (dependency order, no missing deps, no cycles).
//!   4. Execute via the adapter (inside a transaction where supported).
//!   5. Independently re-inspect the live database and verify it matches the
//!      target. A restore is NOT reported as successful unless verification
//!      passes.

#[allow(unused_imports)]
use meld_adapter::{DatabaseAdapter, ExecutionResult};
use meld_core::ids::CommitHash;
use meld_core::plan::validate_plan;

use crate::error::EngineError;
use crate::restore::build_restore_plan;
use crate::session::Session;

/// Restore the live database to the state captured at `commit`.
///
/// Performs a full staged restore: plan, confirm-by-execution, and
/// independent verification. Returns an error if verification fails, so a
/// restore is never reported as successful unless the postconditions hold.
///
/// Before executing the restore, a safety snapshot of the current live state
/// is captured and recorded. If the restore fails, HEAD is left untouched,
/// the database is rolled back (the adapter executes inside a transaction),
/// and the error includes the safety-snapshot commit id so the operator can
/// recover.
pub async fn checkout(
    session: &mut Session,
    commit: &CommitHash,
) -> Result<ExecutionResult, EngineError> {
    let target_commit = session
        .store
        .get_commit(commit)
        .map_err(|e| EngineError::Restore(format!("failed to load commit: {e}")))?;
    let adapter = session.adapter()?;

    // Engine/scope must match the active connection.
    if target_commit.state.engine_kind() != adapter.engine_kind() {
        return Err(EngineError::Internal(format!(
            "engine/scope mismatch: expected {:?}, got {:?}",
            adapter.engine_kind(),
            target_commit.state.engine_kind()
        )));
    }

    // 0. Capture a safety snapshot of the current live state before any
    // destructive restore operations. This snapshot can be used to recover
    // if the restore is interrupted or fails.
    let safety_state = adapter
        .introspect()
        .await
        .map_err(|e| EngineError::Restore(format!("failed to capture safety snapshot: {e}")))?;
    let safety_root = meld_core::canonical::hash_state(&safety_state);
    let parents = match session.head()? {
        Some(h) => vec![h],
        None => vec![],
    };
    let safety_commit = meld_core::commit::Commit::new(
        parents,
        "meld",
        "safety snapshot before restore",
        safety_state.engine_kind(),
        safety_state.scope(),
        meld_core::ids::ObjectHash::from_bytes(safety_root.as_bytes()),
        meld_core::transition::ChangeSummary::default(),
        safety_state,
    );
    let safety_hash = safety_commit.hash;
    session
        .store
        .put_commit(&safety_commit)
        .map_err(|e| EngineError::Restore(format!("failed to persist safety snapshot: {e}")))?;
    // Record the restore-in-progress marker with the safety snapshot id.
    session
        .store
        .set_restore_marker(&safety_hash)
        .map_err(|e| EngineError::Restore(format!("failed to record restore marker: {e}")))?;

    let safety_snapshot_id = safety_hash.short(12);

    // 1. Build the restore plan by diffing target vs live.
    let plan = build_restore_plan(adapter, &target_commit).await?;

    // 2. Validate the plan before any destructive execution.
    validate_plan(&plan).map_err(|e| EngineError::Restore(e.to_string()))?;

    // 3. Execute. An `Ok` result with failed operations is still a failure:
    // the plan did not transform the live database into the target state.
    let result = adapter
        .execute_restore_plan(&plan)
        .await
        .map_err(|e| EngineError::Restore(e.to_string()))?;
    if result.operations_failed > 0 {
        // Roll back the restore marker so recovery can use the safety snapshot.
        let _ = session.store.clear_restore_marker();
        return Err(EngineError::Restore(format!(
            "restore plan execution failed: {} of {} operation(s) did not succeed; messages: {}; \
             safety snapshot id: {} (restore with this commit to recover)",
            result.operations_failed,
            result.operations_executed,
            result.messages.join("; "),
            safety_snapshot_id
        )));
    }
    if !result.recovered {
        let _ = session.store.clear_restore_marker();
        return Err(EngineError::Restore(format!(
            "restore plan execution was not fully recovered; messages: {}; \
             safety snapshot id: {} (restore with this commit to recover)",
            result.messages.join("; "),
            safety_snapshot_id
        )));
    }

    // 4. Independently verify against the target state.
    let report = adapter
        .verify(&target_commit.state)
        .await
        .map_err(|e| EngineError::Verification(e.to_string()))?;
    if !report.passed {
        let _ = session.store.clear_restore_marker();
        return Err(EngineError::Verification(format!(
            "restore verification failed: {:?}; safety snapshot id: {} (restore with this commit to recover)",
            report.mismatches, safety_snapshot_id
        )));
    }

    // 5. Advance HEAD to the restored commit and clear the restore marker.
    session
        .store
        .set_head(commit)
        .map_err(|e| EngineError::Restore(e.to_string()))?;
    session
        .store
        .clear_restore_marker()
        .map_err(|e| EngineError::Restore(format!("failed to clear restore marker: {e}")))?;

    Ok(result)
}

/// Build (but do not execute) the restore plan for `commit`, suitable for
/// preview/confirmation in the UI.
pub async fn restore_plan(
    session: &mut Session,
    commit: &CommitHash,
) -> Result<meld_core::plan::RestorePlan, EngineError> {
    let target_commit = session
        .store
        .get_commit(commit)
        .map_err(|e| EngineError::Restore(format!("failed to load commit: {e}")))?;
    let adapter = session.adapter()?;
    build_restore_plan(adapter, &target_commit).await
}

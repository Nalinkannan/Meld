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
pub async fn checkout(
    session: &mut Session,
    commit: &CommitHash,
) -> Result<ExecutionResult, EngineError> {
    let target_commit = session
        .store
        .get_commit(commit)
        .map_err(|e| EngineError::Restore(format!("failed to load commit: {e}")))?;
    let adapter = session.adapter_mut()?;

    // Engine/scope must match the active connection.
    if target_commit.state.engine_kind() != adapter.engine_kind() {
        return Err(EngineError::Internal(format!(
            "engine/scope mismatch: expected {:?}, got {:?}",
            adapter.engine_kind(),
            target_commit.state.engine_kind()
        )));
    }

    // 1. Build the restore plan by diffing target vs live.
    let plan = build_restore_plan(adapter, &target_commit).await?;

    // 2. Validate the plan before any destructive execution.
    validate_plan(&plan).map_err(|e| EngineError::Restore(e.to_string()))?;

    // 3. Execute.
    let result = adapter
        .execute_restore_plan(&plan)
        .await
        .map_err(|e| EngineError::Restore(e.to_string()))?;

    // 4. Independently verify against the target state.
    let report = adapter
        .verify(&target_commit.state)
        .await
        .map_err(|e| EngineError::Verification(e.to_string()))?;
    if !report.passed {
        return Err(EngineError::Verification(format!(
            "restore verification failed: {:?}",
            report.mismatches
        )));
    }

    // 5. Advance HEAD to the restored commit.
    session
        .store
        .set_head(commit)
        .map_err(|e| EngineError::Restore(e.to_string()))?;

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
    let adapter = session.adapter_mut()?;
    build_restore_plan(adapter, &target_commit).await
}

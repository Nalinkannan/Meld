//! Independent verification (verify a commit against the live database).

use meld_adapter::DatabaseAdapter;
use meld_core::ids::CommitHash;
use meld_core::verify::VerificationReport;

use crate::error::EngineError;
use crate::session::Session;

/// Independently re-inspect the live database and compare it against the
/// state captured at `commit`. Returns the verification report; the caller
/// must treat `report.passed == false` as a failed restore/merge.
pub async fn verify(
    adapter: &dyn DatabaseAdapter,
    store: &meld_store::Store,
    commit: &CommitHash,
) -> Result<VerificationReport, EngineError> {
    let target = store
        .get_commit(commit)
        .map_err(|e| EngineError::Restore(e.to_string()))?;
    let report = adapter
        .verify(&target.state)
        .await
        .map_err(|e| EngineError::Verification(e.to_string()))?;
    Ok(report)
}

/// Verify the currently checked-out (HEAD) commit.
pub async fn verify_head(session: &Session) -> Result<VerificationReport, EngineError> {
    let adapter = session.adapter()?;
    let head = session
        .head()?
        .ok_or_else(|| EngineError::Verification("HEAD is empty".into()))?;
    verify(adapter, &session.store, &head).await
}

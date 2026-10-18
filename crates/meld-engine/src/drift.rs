//! Drift detection against the current HEAD.

use crate::error::EngineError;
use crate::session::Session;
use meld_core::diff::diff_states;
use meld_core::ids::CommitHash;

/// Detect drift between HEAD and the live database.
///
/// Returns a semantic diff showing what has changed in the live database
/// since the given commit was created.
pub async fn drift(
    session: &Session,
    commit: &CommitHash,
) -> Result<meld_core::diff::SnapshotDiff, EngineError> {
    let adapter = session.adapter()?;
    let store = &session.store;

    let target_commit = store
        .get_commit(commit)
        .map_err(|e| EngineError::Drift(e.to_string()))?;

    // Introspect the live database state.
    let live_state = adapter
        .introspect()
        .await
        .map_err(|e| EngineError::Drift(e.to_string()))?;

    // Diff the target commit state against the live state.
    let diff = diff_states(&target_commit.state, &live_state)
        .map_err(|e| EngineError::Drift(e.to_string()))?;

    Ok(diff)
}

/// Detect drift between HEAD and the live database (convenience wrapper).
pub async fn drift_head(session: &Session) -> Result<meld_core::diff::SnapshotDiff, EngineError> {
    let head = session
        .head()?
        .ok_or_else(|| EngineError::Drift("HEAD is empty".into()))?;
    drift(session, &head).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use indexmap::IndexMap;
    use meld_core::ids::ObjectHash;
    use meld_core::state::{EngineKind, SnapshotScope};
    use meld_core::transition::ChangeSummary;
    use meld_store::Store;
    use tempfile::TempDir;

    fn sample_commit(
        parents: Vec<CommitHash>,
        msg: &str,
        root: [u8; 32],
    ) -> meld_core::commit::Commit {
        meld_core::commit::Commit::new(
            parents,
            "test",
            msg,
            EngineKind::PostgreSql,
            SnapshotScope::postgres("db"),
            ObjectHash::from_bytes(root),
            ChangeSummary::default(),
            meld_core::state::EngineState::PostgreSql(meld_core::state::PostgresState {
                engine_version: "18".into(),
                databases: IndexMap::new(),
                warnings: vec![],
                capture_info: meld_core::state::CaptureInfo {
                    consistency: meld_core::state::ConsistencyLevel::Snapshot,
                    strategy: "test".into(),
                    captured_at: Utc::now(),
                    engine_version: "18".into(),
                    scope: "db".into(),
                    warnings: vec![],
                },
            }),
        )
    }

    #[test]
    fn drift_requires_connection() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(meld_store::StoreConfig {
            repo_path: dir.path().join("meld.edb"),
        })
        .unwrap();

        let c1 = sample_commit(vec![], "first", [1; 32]);
        let h1 = c1.hash;

        let session = crate::session::Session {
            store,
            adapter: None,
            connection: None,
            current_branch: "main".to_string(),
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(drift(&session, &h1));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no database connection")
        );
    }
}

//! Diff between two commits or between a commit and the live database.

use crate::error::EngineError;
use crate::session::Session;
use meld_core::diff::diff_states;
use meld_core::ids::CommitHash;

/// Diff between two commit states.
pub async fn diff(
    store: &meld_store::Store,
    from: &CommitHash,
    to: &CommitHash,
) -> Result<meld_core::diff::SnapshotDiff, EngineError> {
    let from_commit = store
        .get_commit(from)
        .map_err(|e| EngineError::Diff(e.to_string()))?;
    let to_commit = store
        .get_commit(to)
        .map_err(|e| EngineError::Diff(e.to_string()))?;

    // Ensure both commits are for the same engine and scope.
    if from_commit.state.engine_kind() != to_commit.state.engine_kind() {
        return Err(EngineError::IncompatibleEngine {
            expected: from_commit.state.engine_kind(),
            got: to_commit.state.engine_kind(),
        });
    }
    if !from_commit.scope.compatible_with(&to_commit.scope) {
        return Err(EngineError::IncompatibleScope(format!(
            "scope mismatch: {:?} vs {:?}",
            from_commit.scope, to_commit.scope
        )));
    }

    let diff = diff_states(&from_commit.state, &to_commit.state)
        .map_err(|e| EngineError::Diff(e.to_string()))?;

    Ok(diff)
}

/// Diff between a commit and the live database (drift).
pub async fn drift(
    session: &Session,
    commit: &CommitHash,
) -> Result<meld_core::diff::SnapshotDiff, EngineError> {
    crate::drift::drift(session, commit).await
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
    fn diff_identical_commits_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(meld_store::StoreConfig {
            repo_path: dir.path().join("meld.edb"),
        })
        .unwrap();

        let c1 = sample_commit(vec![], "first", [1; 32]);
        let h1 = c1.hash;
        let c2 = sample_commit(vec![], "second", [1; 32]); // same state
        let h2 = c2.hash;

        store.put_commit(&c1).unwrap();
        store.put_commit(&c2).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let diff = rt.block_on(diff(&store, &h1, &h2)).unwrap();
        assert!(diff.is_empty());
        assert_eq!(diff.summary.total_changes(), 0);
    }

    #[test]
    fn diff_rejects_incompatible_engines() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(meld_store::StoreConfig {
            repo_path: dir.path().join("meld.edb"),
        })
        .unwrap();

        let pg = sample_commit(vec![], "pg", [1; 32]);
        let h1 = pg.hash;

        let mongo = meld_core::commit::Commit::new(
            vec![],
            "test",
            "mongo",
            EngineKind::MongoDb,
            SnapshotScope::mongodb("db"),
            ObjectHash::from_bytes([2; 32]),
            ChangeSummary::default(),
            meld_core::state::EngineState::MongoDb(meld_core::state::MongoState {
                engine_version: "8".into(),
                database_name: "db".into(),
                collections: IndexMap::new(),
                warnings: vec![],
                capture_info: meld_core::state::CaptureInfo {
                    consistency: meld_core::state::ConsistencyLevel::Snapshot,
                    strategy: "test".into(),
                    captured_at: Utc::now(),
                    engine_version: "8".into(),
                    scope: "db".into(),
                    warnings: vec![],
                },
            }),
        );
        let h2 = mongo.hash;

        store.put_commit(&pg).unwrap();
        store.put_commit(&mongo).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(diff(&store, &h1, &h2));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("incompatible engine")
        );
    }
}

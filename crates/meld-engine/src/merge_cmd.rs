//! Three-way merge orchestration.

use crate::error::EngineError;
use meld_core::ids::CommitHash;
use meld_core::merge::{MergeInput as CoreMergeInput, analyze_merge as core_analyze_merge};

/// Input to a three-way merge.
pub struct MergeInput {
    pub ancestor: CommitHash,
    pub left: CommitHash,
    pub right: CommitHash,
}

/// Result of merge analysis.
#[derive(Debug, Clone)]
pub struct MergeAnalysis {
    pub ancestor: CommitHash,
    pub left: CommitHash,
    pub right: CommitHash,
    pub engine_kind: meld_core::state::EngineKind,
    pub conflicts: Vec<meld_core::conflict::ConflictReport>,
    pub auto_mergeable: Vec<meld_core::transition::ChangeOp>,
    pub left_changes: Vec<meld_core::transition::ChangeOp>,
    pub right_changes: Vec<meld_core::transition::ChangeOp>,
}

/// Analyze a three-way merge between commits.
///
/// This loads the three commits from the store, computes the semantic diffs
/// from the ancestor to each side, and classifies conflicts.
pub async fn analyze_merge(
    store: &meld_store::Store,
    input: MergeInput,
) -> Result<MergeAnalysis, EngineError> {
    let ancestor_commit = store
        .get_commit(&input.ancestor)
        .map_err(|e| EngineError::Merge(e.to_string()))?;
    let left_commit = store
        .get_commit(&input.left)
        .map_err(|e| EngineError::Merge(e.to_string()))?;
    let right_commit = store
        .get_commit(&input.right)
        .map_err(|e| EngineError::Merge(e.to_string()))?;

    // Verify all three commits are for the same engine and scope.
    let engine_kind = ancestor_commit.state.engine_kind();
    if left_commit.state.engine_kind() != engine_kind
        || right_commit.state.engine_kind() != engine_kind
    {
        return Err(EngineError::IncompatibleEngine {
            expected: engine_kind,
            got: left_commit.state.engine_kind(),
        });
    }
    let scope = ancestor_commit.scope;
    if !left_commit.scope.compatible_with(&scope) || !right_commit.scope.compatible_with(&scope) {
        return Err(EngineError::IncompatibleScope(
            "merge commits have incompatible scopes".into(),
        ));
    }

    // Perform merge analysis using the core domain logic.
    let core_input = CoreMergeInput {
        ancestor: input.ancestor,
        left: input.left,
        right: input.right,
        ancestor_state: ancestor_commit.state.clone(),
        left_state: left_commit.state.clone(),
        right_state: right_commit.state.clone(),
    };

    let analysis = core_analyze_merge(&core_input);

    Ok(MergeAnalysis {
        ancestor: analysis.ancestor,
        left: analysis.left,
        right: analysis.right,
        engine_kind: analysis.engine_kind,
        conflicts: analysis.conflicts,
        auto_mergeable: analysis.auto_mergeable,
        left_changes: analysis.left_changes,
        right_changes: analysis.right_changes,
    })
}

/// Check if all conflicts in an analysis have been resolved.
pub fn all_conflicts_resolved(analysis: &MergeAnalysis) -> bool {
    analysis.conflicts.iter().all(|c| c.is_resolved())
}

/// Get a summary of the merge analysis.
pub fn merge_summary(analysis: &MergeAnalysis) -> MergeSummary {
    let total_conflicts = analysis.conflicts.len();
    let resolved_conflicts = analysis
        .conflicts
        .iter()
        .filter(|c| c.is_resolved())
        .count();
    let auto_mergeable = analysis.auto_mergeable.len();

    MergeSummary {
        total_conflicts,
        resolved_conflicts,
        auto_mergeable,
        engine_kind: analysis.engine_kind,
    }
}

/// Summary of a merge analysis for display purposes.
#[derive(Debug, Clone)]
pub struct MergeSummary {
    pub total_conflicts: usize,
    pub resolved_conflicts: usize,
    pub auto_mergeable: usize,
    pub engine_kind: meld_core::state::EngineKind,
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
    fn merge_analysis_requires_common_ancestor() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(meld_store::StoreConfig {
            repo_path: dir.path().join("meld.edb"),
        })
        .unwrap();

        // Three commits with no common ancestor.
        let c1 = sample_commit(vec![], "first", [1; 32]);
        let h1 = c1.hash;
        let c2 = sample_commit(vec![], "second", [2; 32]);
        let h2 = c2.hash;
        let c3 = sample_commit(vec![], "third", [3; 32]);
        let h3 = c3.hash;

        store.put_commit(&c1).unwrap();
        store.put_commit(&c2).unwrap();
        store.put_commit(&c3).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(analyze_merge(
            &store,
            MergeInput {
                ancestor: h1,
                left: h2,
                right: h3,
            },
        ));
        // Should succeed (analysis runs even if no common ancestor found in graph)
        // but conflicts will be detected based on state differences.
        assert!(result.is_ok());
    }

    #[test]
    fn merge_rejects_cross_engine() {
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

        let pg2 = sample_commit(vec![], "pg2", [3; 32]);
        let h3 = pg2.hash;

        store.put_commit(&pg).unwrap();
        store.put_commit(&mongo).unwrap();
        store.put_commit(&pg2).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(analyze_merge(
            &store,
            MergeInput {
                ancestor: h1,
                left: h2,
                right: h3,
            },
        ));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("incompatible engine")
        );
    }

    #[test]
    fn merge_summary_counts() {
        let analysis = MergeAnalysis {
            ancestor: CommitHash::ZERO,
            left: CommitHash::ZERO,
            right: CommitHash::ZERO,
            engine_kind: EngineKind::PostgreSql,
            conflicts: vec![],
            auto_mergeable: vec![],
            left_changes: vec![],
            right_changes: vec![],
        };
        let summary = merge_summary(&analysis);
        assert_eq!(summary.total_conflicts, 0);
        assert_eq!(summary.resolved_conflicts, 0);
        assert_eq!(summary.auto_mergeable, 0);
    }
}

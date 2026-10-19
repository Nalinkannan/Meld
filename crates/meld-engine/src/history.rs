//! Commit history traversal.

use crate::error::EngineError;
use meld_core::graph::CommitGraph;
use meld_core::ids::CommitHash;

/// List commits reachable from HEAD (newest first).
pub async fn history(
    store: &meld_store::Store,
    limit: Option<usize>,
) -> Result<Vec<meld_core::commit::Commit>, EngineError> {
    let head = store
        .get_head()
        .map_err(|e| EngineError::Session(e.to_string()))?;

    let Some(head_hash) = head else {
        return Ok(Vec::new());
    };

    // Build the commit graph by loading all commits.
    // For efficiency, we walk from HEAD backwards using parent references.
    let mut commits = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![head_hash];

    while let Some(hash) = stack.pop() {
        if !visited.insert(hash) {
            continue;
        }
        if let Some(max) = limit
            && commits.len() >= max
        {
            break;
        }

        let commit = store
            .get_commit(&hash)
            .map_err(|e| EngineError::Session(e.to_string()))?;

        // Push parents to stack for DFS (will give us reverse chronological order)
        for parent in &commit.parents {
            if !parent.is_zero() {
                stack.push(*parent);
            }
        }

        commits.push(commit);
    }

    // The DFS gives us commits in an order where children come before parents.
    // Since we want newest first (HEAD first), the DFS order is already correct.
    Ok(commits)
}

/// Build a full commit graph from the store.
pub fn build_commit_graph(store: &meld_store::Store) -> Result<CommitGraph, EngineError> {
    let mut graph = CommitGraph::new();
    let all_hashes = store
        .list_all_commit_hashes()
        .map_err(|e| EngineError::Session(e.to_string()))?;

    // Load all commits and add to graph in topological order.
    // We need to sort by topological order (parents before children).
    let mut commits = Vec::new();
    for hash in all_hashes {
        let commit = store
            .get_commit(&hash)
            .map_err(|e| EngineError::Session(e.to_string()))?;
        commits.push(commit);
    }

    // Sort by timestamp to approximate topological order (older commits first).
    commits.sort_by_key(|c| c.timestamp);

    for commit in commits {
        graph
            .add_commit(&commit)
            .map_err(|e| EngineError::Session(e.to_string()))?;
    }

    Ok(graph)
}

/// Get the commit graph's common ancestor of two commits.
pub fn common_ancestor(
    graph: &CommitGraph,
    left: &CommitHash,
    right: &CommitHash,
) -> Option<CommitHash> {
    graph.common_ancestor(left, right)
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
    fn history_walk_returns_commits_newest_first() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(meld_store::StoreConfig {
            repo_path: dir.path().join("meld.edb"),
        })
        .unwrap();

        let c1 = sample_commit(vec![], "first", [1; 32]);
        let h1 = c1.hash;
        let c2 = sample_commit(vec![h1], "second", [2; 32]);
        let h2 = c2.hash;
        let c3 = sample_commit(vec![h2], "third", [3; 32]);
        let h3 = c3.hash;

        store.put_commit(&c1).unwrap();
        store.put_commit(&c2).unwrap();
        store.put_commit(&c3).unwrap();
        store.set_head(&h3).unwrap();
        store.create_branch("main", &h3).unwrap();

        let history = futures::executor::block_on(history(&store, Some(10))).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].hash, h3); // newest first
        assert_eq!(history[1].hash, h2);
        assert_eq!(history[2].hash, h1);
    }

    #[test]
    fn history_limit_works() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(meld_store::StoreConfig {
            repo_path: dir.path().join("meld.edb"),
        })
        .unwrap();

        let c1 = sample_commit(vec![], "first", [1; 32]);
        let h1 = c1.hash;
        let c2 = sample_commit(vec![h1], "second", [2; 32]);
        let h2 = c2.hash;
        let c3 = sample_commit(vec![h2], "third", [3; 32]);
        let h3 = c3.hash;

        store.put_commit(&c1).unwrap();
        store.put_commit(&c2).unwrap();
        store.put_commit(&c3).unwrap();
        store.set_head(&h3).unwrap();

        let history = futures::executor::block_on(history(&store, Some(2))).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].hash, h3);
        assert_eq!(history[1].hash, h2);
    }

    #[test]
    fn history_empty_when_no_head() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(meld_store::StoreConfig {
            repo_path: dir.path().join("meld.edb"),
        })
        .unwrap();

        let history = futures::executor::block_on(history(&store, None)).unwrap();
        assert!(history.is_empty());
    }
}

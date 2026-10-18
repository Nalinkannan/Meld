//! Commit DAG and dependency graph types.
//!
//! Uses `petgraph` for the commit DAG. Branch and HEAD references are stored
//! separately (see `meld-store`); this crate owns the graph algorithms only.

use crate::MeldError;
use crate::ids::CommitHash;
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};

/// A directed acyclic graph of commits.
#[derive(Debug, Clone, Default)]
pub struct CommitGraph {
    graph: DiGraph<CommitHash, ()>,
    index_by_hash: HashMap<CommitHash, NodeIndex>,
}

impl CommitGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a commit. Its parents must already exist (unless empty or zero).
    pub fn add_commit(&mut self, commit: &crate::commit::Commit) -> Result<(), MeldError> {
        if self.index_by_hash.contains_key(&commit.hash) {
            return Err(MeldError::CommitGraph(format!(
                "commit {} already exists",
                commit.hash
            )));
        }
        for parent in &commit.parents {
            if parent.is_zero() {
                continue;
            }
            if !self.index_by_hash.contains_key(parent) {
                return Err(MeldError::CommitGraph(format!(
                    "unknown parent commit {parent}"
                )));
            }
        }

        let node = self.graph.add_node(commit.hash);
        for parent in &commit.parents {
            if parent.is_zero() {
                continue;
            }
            let pnode = self.index_by_hash[parent];
            self.graph.add_edge(pnode, node, ());
        }

        // A newly added node is a sink (no outgoing edges yet), so it cannot
        // itself introduce a cycle. Verify the whole graph remains acyclic for
        // safety.
        if self.has_cycle() {
            return Err(MeldError::CommitGraph(format!(
                "adding commit {} would create a cycle",
                commit.hash
            )));
        }

        self.index_by_hash.insert(commit.hash, node);
        Ok(())
    }

    pub fn parents_of(&self, hash: &CommitHash) -> Vec<CommitHash> {
        let Some(node) = self.index_by_hash.get(hash) else {
            return vec![];
        };
        self.graph
            .edges_directed(*node, petgraph::Direction::Incoming)
            .map(|e| self.graph[e.source()])
            .collect()
    }

    pub fn children_of(&self, hash: &CommitHash) -> Vec<CommitHash> {
        let Some(node) = self.index_by_hash.get(hash) else {
            return vec![];
        };
        self.graph
            .edges_directed(*node, petgraph::Direction::Outgoing)
            .map(|e| self.graph[e.target()])
            .collect()
    }

    pub fn contains(&self, hash: &CommitHash) -> bool {
        self.index_by_hash.contains_key(hash)
    }

    /// All ancestor commits (parents, grandparents, ...) of `hash`, not
    /// including `hash` itself.
    pub fn ancestors(&self, hash: &CommitHash) -> Vec<CommitHash> {
        let Some(&start) = self.index_by_hash.get(hash) else {
            return vec![];
        };
        let mut visited = HashSet::new();
        let mut stack = vec![start];
        let mut result = Vec::new();
        while let Some(node) = stack.pop() {
            for e in self
                .graph
                .edges_directed(node, petgraph::Direction::Incoming)
            {
                let parent = self.graph[e.source()];
                if visited.insert(parent) {
                    result.push(parent);
                    stack.push(self.index_by_hash[&parent]);
                }
            }
        }
        result
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Return true if the graph contains a cycle.
    pub fn has_cycle(&self) -> bool {
        toposort(&self.graph, None).is_err()
    }

    /// Topological order of commits (parents before children).
    pub fn topological_order(&self) -> Vec<CommitHash> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        for node in self.graph.node_indices() {
            self.visit_topo(node, &mut visited, &mut result);
        }
        result
    }

    fn visit_topo(
        &self,
        node: NodeIndex,
        visited: &mut HashSet<NodeIndex>,
        result: &mut Vec<CommitHash>,
    ) {
        if !visited.insert(node) {
            return;
        }
        // Visit parents first (incoming edges).
        for e in self
            .graph
            .edges_directed(node, petgraph::Direction::Incoming)
        {
            self.visit_topo(e.source(), visited, result);
        }
        result.push(self.graph[node]);
    }

    /// Find a common ancestor of two commits. Returns the nearest common
    /// ancestor found by walking the left side's ancestors first.
    pub fn common_ancestor(&self, left: &CommitHash, right: &CommitHash) -> Option<CommitHash> {
        let left_set = self.ancestor_set(left);
        for a in self.ancestors(right) {
            if left_set.contains(&a) {
                return Some(a);
            }
        }
        // Also consider the commits themselves as possible ancestors.
        if left_set.contains(right) {
            return Some(*right);
        }
        if self.ancestors(left).contains(right) {
            return Some(*right);
        }
        None
    }

    fn ancestor_set(&self, hash: &CommitHash) -> HashSet<CommitHash> {
        let mut set = HashSet::new();
        let mut stack = vec![*hash];
        while let Some(h) = stack.pop() {
            for p in self.parents_of(&h) {
                if set.insert(p) {
                    stack.push(p);
                }
            }
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::Commit;
    use crate::ids::ObjectHash;
    use crate::state::{
        CaptureInfo, ConsistencyLevel, EngineKind, EngineState, PostgresState, SnapshotScope,
    };
    use crate::transition::ChangeSummary;
    use indexmap::IndexMap;

    fn commit(root: [u8; 32], parents: &[CommitHash]) -> Commit {
        Commit::new(
            parents.to_vec(),
            "test",
            "msg",
            EngineKind::PostgreSql,
            SnapshotScope::postgres("db"),
            ObjectHash::from_bytes(root),
            ChangeSummary::default(),
            sample_state(),
        )
    }

    fn sample_state() -> EngineState {
        EngineState::PostgreSql(PostgresState {
            engine_version: "18".into(),
            databases: IndexMap::new(),
            warnings: vec![],
            capture_info: CaptureInfo {
                consistency: ConsistencyLevel::Snapshot,
                strategy: "test".into(),
                captured_at: chrono::Utc::now(),
                engine_version: "18".into(),
                scope: "db".into(),
                warnings: vec![],
            },
        })
    }

    #[test]
    fn add_and_traverse_parents() {
        let mut g = CommitGraph::new();
        let c1 = commit([1; 32], &[]);
        let h1 = c1.hash;
        let c2 = commit([2; 32], &[h1]);
        g.add_commit(&c1).unwrap();
        g.add_commit(&c2).unwrap();
        let parents = g.parents_of(&c2.hash);
        assert_eq!(parents, vec![h1]);
        assert!(g.contains(&h1));
        assert_eq!(g.node_count(), 2);
    }

    #[test]
    fn unknown_parent_rejected() {
        let mut g = CommitGraph::new();
        let c = commit([9; 32], &[CommitHash::from_bytes([7; 32])]);
        let err = g.add_commit(&c).unwrap_err();
        assert!(matches!(err, MeldError::CommitGraph { .. }));
    }

    #[test]
    fn ancestors_walk() {
        let mut g = CommitGraph::new();
        let c1 = commit([1; 32], &[]);
        let h1 = c1.hash;
        let c2 = commit([2; 32], &[h1]);
        let h2 = c2.hash;
        let c3 = commit([3; 32], &[h2, h1]);
        g.add_commit(&c1).unwrap();
        g.add_commit(&c2).unwrap();
        g.add_commit(&c3).unwrap();
        let ancs = g.ancestors(&c3.hash);
        assert!(ancs.contains(&h1));
        assert!(ancs.contains(&h2));
        assert_eq!(ancs.len(), 2);
    }

    #[test]
    fn common_ancestor_found() {
        let mut g = CommitGraph::new();
        let c1 = commit([1; 32], &[]);
        let h1 = c1.hash;
        let c2 = commit([2; 32], &[h1]);
        let h2 = c2.hash;
        let c3 = commit([3; 32], &[h1]);
        g.add_commit(&c1).unwrap();
        g.add_commit(&c2).unwrap();
        g.add_commit(&c3).unwrap();
        let ca = g.common_ancestor(&h2, &c3.hash);
        assert_eq!(ca, Some(h1));
    }
}

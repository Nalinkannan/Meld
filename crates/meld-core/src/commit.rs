//! Immutable commits and the commit identifier.

use crate::canonical::Canonicalize;
use crate::ids::{CommitHash, Hash, ObjectHash};
use crate::state::{EngineKind, SnapshotScope};
use crate::transition::ChangeSummary;
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Current commit format version.
pub const COMMIT_FORMAT_VERSION: u32 = 1;

/// An immutable commit representing a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub hash: CommitHash,
    pub parents: Vec<CommitHash>,
    pub author: String,
    pub timestamp: chrono::DateTime<Utc>,
    pub message: String,
    pub engine_kind: EngineKind,
    pub scope: SnapshotScope,
    pub root_hash: ObjectHash,
    pub change_summary: ChangeSummary,
    pub format_version: u32,
    /// The full captured state. Stored so the commit is self-contained and a
    /// historical state can be reconstructed without re-introspection.
    pub state: crate::state::EngineState,
}

impl Commit {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        parents: Vec<CommitHash>,
        author: impl Into<String>,
        message: impl Into<String>,
        engine_kind: crate::state::EngineKind,
        scope: crate::state::SnapshotScope,
        root_hash: ObjectHash,
        change_summary: ChangeSummary,
        state: crate::state::EngineState,
    ) -> Self {
        let timestamp = Utc::now();
        let mut commit = Commit {
            hash: CommitHash::ZERO,
            parents,
            author: author.into(),
            timestamp,
            message: message.into(),
            engine_kind,
            scope,
            root_hash,
            change_summary,
            format_version: COMMIT_FORMAT_VERSION,
            state,
        };
        commit.hash = CommitHash(commit.content_hash());
        commit
    }

    /// Content hash of the commit (excluding the `hash` field itself).
    /// The `hash` field is zeroed before serialization so that the address is
    /// a pure function of the commit's parents, content, and metadata.
    pub fn content_hash(&self) -> Hash {
        let mut c = self.clone();
        c.hash = CommitHash::ZERO;
        let json = serde_json::to_vec(&c).expect("commit is always serializable");
        crate::ids::hash_state_bytes(crate::canonical::DOMAIN_COMMIT, &json)
    }

    pub fn is_merge(&self) -> bool {
        self.parents.len() >= 2
    }

    pub fn is_initial(&self) -> bool {
        self.parents.is_empty()
    }

    pub fn short_hash(&self) -> crate::ids::ShortHash {
        self.hash.short(12)
    }
}

impl Canonicalize for Commit {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        use crate::canonical::{write_bytes, write_str, write_u32, write_u64};
        out.push(0xc1);
        write_bytes(out, &self.hash.0.as_bytes());
        write_u64(out, self.parents.len() as u64);
        for p in &self.parents {
            write_bytes(out, &p.0.as_bytes());
        }
        write_str(out, &self.author);
        self.timestamp.canonicalize(out);
        write_str(out, &self.message);
        write_u32(out, self.engine_kind as u32);
        self.scope.canonicalize(out);
        write_bytes(out, &self.root_hash.0.as_bytes());
        self.change_summary.canonicalize(out);
        write_u32(out, self.format_version);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        CaptureInfo, ConsistencyLevel, EngineKind, EngineState, PostgresState, SnapshotScope,
    };
    use crate::transition::ChangeSummary;
    use indexmap::IndexMap;

    fn sample_scope() -> SnapshotScope {
        SnapshotScope::postgres("meld_test")
    }

    #[test]
    fn commit_content_hash_is_deterministic() {
        let summary = ChangeSummary::default();
        let root = ObjectHash::from_bytes([1u8; 32]);
        let state = sample_state();
        let c1 = Commit::new(
            vec![],
            "alice",
            "initial snapshot",
            EngineKind::PostgreSql,
            sample_scope(),
            root,
            summary.clone(),
            state.clone(),
        );
        // A clone preserves the hash exactly.
        let c2 = c1.clone();
        assert_eq!(c1.hash, c2.hash);
        assert_eq!(c1.content_hash(), c2.content_hash());
        // The stored hash equals the content address of the commit with its
        // hash field zeroed.
        assert_eq!(c1.hash, CommitHash(c1.content_hash()));
        assert!(!c1.is_merge());
        assert!(c1.is_initial());
        // The content hash ignores the stored `hash` field (zeroed before hashing).
        let mut zeroed = c1.clone();
        zeroed.hash = CommitHash::ZERO;
        assert_eq!(c1.content_hash(), zeroed.content_hash());
        // A different message yields a different commit hash.
        let c3 = Commit::new(
            vec![],
            "alice",
            "different message",
            EngineKind::PostgreSql,
            sample_scope(),
            root,
            ChangeSummary::default(),
            state,
        );
        assert_ne!(c1.hash, c3.hash);
    }

    #[test]
    fn merge_commit_detected() {
        let summary = ChangeSummary::default();
        let c = Commit::new(
            vec![CommitHash::ZERO, CommitHash::ZERO],
            "bob",
            "merge",
            EngineKind::MongoDb,
            SnapshotScope::mongodb("db"),
            ObjectHash::from_bytes([2u8; 32]),
            summary,
            sample_state(),
        );
        assert!(c.is_merge());
        assert!(!c.is_initial());
    }

    fn sample_state() -> EngineState {
        EngineState::PostgreSql(PostgresState {
            engine_version: "18".into(),
            databases: IndexMap::new(),
            warnings: vec![],
            capture_info: CaptureInfo {
                consistency: ConsistencyLevel::Snapshot,
                strategy: "test".into(),
                captured_at: Utc::now(),
                engine_version: "18".into(),
                scope: "meld_test".into(),
                warnings: vec![],
            },
        })
    }
}

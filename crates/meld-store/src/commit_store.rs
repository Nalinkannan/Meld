//! Commit persistence.
//!
//! Commits are immutable and stored as JSON, content-addressed by their hash.

use crate::schema::TABLE_COMMITS;
use meld_core::MeldError;
use meld_core::commit::Commit;
use meld_core::ids::{CommitHash, Hash};
use redb::TableDefinition;
use serde_json;

pub(crate) static COMMITS: TableDefinition<String, Vec<u8>> = TableDefinition::new(TABLE_COMMITS);

pub fn put_commit(db: &redb::Database, commit: &Commit) -> Result<CommitHash, MeldError> {
    // The commit hash is the content address of the commit with its hash
    // field zeroed; verify the stored field matches before persisting.
    if commit.hash != CommitHash(commit.content_hash()) {
        return Err(MeldError::Integrity(
            "commit hash field does not match content address".into(),
        ));
    }
    let bytes = serde_json::to_vec(commit).map_err(|e| MeldError::Serialization(format!("{e}")))?;
    let key = commit.hash.hex();
    let txn = db
        .begin_write()
        .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
    {
        let mut table = txn
            .open_table(COMMITS)
            .map_err(|e| MeldError::Io(format!("open commits table: {e}")))?;
        table
            .insert(key, bytes)
            .map_err(|e| MeldError::Io(format!("insert commit: {e}")))?;
    }
    txn.commit()
        .map_err(|e| MeldError::Io(format!("commit commit: {e}")))?;
    Ok(commit.hash)
}

pub fn get_commit(db: &redb::Database, hash: &CommitHash) -> Result<Commit, MeldError> {
    let key = hash.hex();
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let commit = {
        let table = match txn.open_table(COMMITS) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                return Err(MeldError::NotFound(format!("commit {hash} not found")));
            }
            Err(e) => {
                return Err(MeldError::Io(format!("open commits table: {e}")));
            }
        };
        let Some(guard) = table
            .get(&key)
            .map_err(|e| MeldError::Io(format!("get commit: {e}")))?
        else {
            return Err(MeldError::NotFound(format!("commit {hash} not found")));
        };
        let bytes: Vec<u8> = guard.value();
        serde_json::from_slice::<Commit>(&bytes)
            .map_err(|e| MeldError::Serialization(format!("deserialize commit {hash}: {e}")))?
    };
    drop(txn);
    if CommitHash(commit.content_hash()) != *hash {
        return Err(MeldError::Integrity(format!(
            "commit {hash} content hash mismatch: got {}",
            commit.content_hash()
        )));
    }
    Ok(commit)
}

pub fn contains_commit(db: &redb::Database, hash: &CommitHash) -> Result<bool, MeldError> {
    let key = hash.hex();
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let exists = {
        let table = match txn.open_table(COMMITS) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                return Ok(false);
            }
            Err(e) => {
                return Err(MeldError::Io(format!("open commits table: {e}")));
            }
        };
        table
            .get(&key)
            .map_err(|e| MeldError::Io(format!("get commit: {e}")))?
            .is_some()
    };
    drop(txn);
    Ok(exists)
}

/// List all commit hashes in the store.
pub fn list_all_commit_hashes(db: &redb::Database) -> Result<Vec<CommitHash>, MeldError> {
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let hashes = {
        let table = match txn.open_table(COMMITS) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                return Ok(Vec::new());
            }
            Err(e) => {
                return Err(MeldError::Io(format!("open commits table: {e}")));
            }
        };
        let mut hashes = Vec::new();
        for result in table
            .range::<String>(..)
            .map_err(|e| MeldError::Io(format!("iterate commits: {e}")))?
        {
            let (key, _value) =
                result.map_err(|e| MeldError::Io(format!("iterate commits: {e}")))?;
            let hash_str: String = key.value();
            if let Ok(h) = Hash::from_hex(&hash_str) {
                hashes.push(CommitHash(h));
            }
        }
        hashes
    };
    drop(txn);
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use meld_core::ids::ObjectHash;
    use meld_core::state::{EngineKind, SnapshotScope};
    use meld_core::transition::ChangeSummary;
    use tempfile::TempDir;

    fn sample_commit() -> Commit {
        Commit::new(
            vec![],
            "alice",
            "initial",
            EngineKind::PostgreSql,
            SnapshotScope::postgres("db"),
            ObjectHash::from_bytes([7u8; 32]),
            ChangeSummary::default(),
            meld_core::state::EngineState::PostgreSql(meld_core::state::PostgresState {
                engine_version: "18".into(),
                databases: indexmap::IndexMap::new(),
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
    fn put_get_commit_round_trip() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let c = sample_commit();
        let hash = c.hash;
        put_commit(&db, &c).unwrap();
        let got = get_commit(&db, &hash).unwrap();
        assert_eq!(got.hash, hash);
        assert_eq!(got.message, "initial");
    }

    #[test]
    fn commit_with_wrong_hash_field_rejected() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let mut c = sample_commit();
        c.hash = CommitHash::ZERO;
        let err = put_commit(&db, &c).unwrap_err();
        assert!(matches!(err, MeldError::Integrity { .. }));
    }
}

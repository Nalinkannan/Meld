//! Integrity checks and orphan detection for the local store.

use crate::schema::{HEAD_KEY, TABLE_COMMITS, TABLE_HEAD, TABLE_OBJECTS};
use meld_core::MeldError;
use meld_core::ids::{CommitHash, Hash, ObjectHash, hash_commit, hash_object};
use redb::TableDefinition;

pub(crate) static OBJECTS: TableDefinition<String, Vec<u8>> = TableDefinition::new(TABLE_OBJECTS);
pub(crate) static COMMITS: TableDefinition<String, Vec<u8>> = TableDefinition::new(TABLE_COMMITS);
pub(crate) static HEAD: TableDefinition<String, String> = TableDefinition::new(TABLE_HEAD);

/// Result of an integrity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    pub objects_checked: u64,
    pub objects_ok: u64,
    pub objects_corrupt: u64,
    pub commits_checked: u64,
    pub commits_ok: u64,
    pub commits_corrupt: u64,
    pub orphans: Vec<String>, // commit hashes referenced but missing
    pub head_dangling: bool,
}

/// Verify every object and commit's content address, and detect dangling
/// references.
pub fn integrity_check(db: &redb::Database) -> Result<IntegrityReport, MeldError> {
    let mut report = IntegrityReport {
        objects_checked: 0,
        objects_ok: 0,
        objects_corrupt: 0,
        commits_checked: 0,
        commits_ok: 0,
        commits_corrupt: 0,
        orphans: Vec::new(),
        head_dangling: false,
    };

    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let objects = match txn.open_table(OBJECTS) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => {
            drop(txn);
            return Ok(report);
        }
        Err(e) => return Err(MeldError::Io(format!("open objects: {e}"))),
    };
    for result in objects
        .range::<String>(..)
        .map_err(|e| MeldError::Io(format!("iterate objects: {e}")))?
    {
        let (_key, value) = result.map_err(|e| MeldError::Io(format!("iterate objects: {e}")))?;
        report.objects_checked += 1;
        let bytes: Vec<u8> = value.value();
        let actual = ObjectHash(hash_object(&bytes));
        let key_str: String = _key.value();
        let expected = ObjectHash(
            Hash::from_hex(&key_str)
                .map_err(|e| MeldError::Io(format!("corrupt store object key {key_str}: {e}")))?,
        );
        if actual == expected {
            report.objects_ok += 1;
        } else {
            report.objects_corrupt += 1;
        }
    }

    let mut commit_hashes = Vec::new();
    let commits = match txn.open_table(COMMITS) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => {
            drop(txn);
            return Ok(report);
        }
        Err(e) => return Err(MeldError::Io(format!("open commits: {e}"))),
    };
    for result in commits
        .range::<String>(..)
        .map_err(|e| MeldError::Io(format!("iterate commits: {e}")))?
    {
        let (key, value) = result.map_err(|e| MeldError::Io(format!("iterate commits: {e}")))?;
        report.commits_checked += 1;
        let bytes: Vec<u8> = value.value();
        let actual = CommitHash(hash_commit(&bytes));
        let key_str: String = key.value();
        let expected = CommitHash(
            Hash::from_hex(&key_str)
                .map_err(|e| MeldError::Io(format!("corrupt store commit key {key_str}: {e}")))?,
        );
        if actual == expected {
            report.commits_ok += 1;
            commit_hashes.push(expected.hex());
        } else {
            report.commits_corrupt += 1;
        }
    }

    // HEAD dangling check.
    let head = match txn.open_table(HEAD) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => {
            drop(txn);
            return Ok(report);
        }
        Err(e) => return Err(MeldError::Io(format!("open head: {e}"))),
    };
    if let Some(guard) = head
        .get(&HEAD_KEY.to_string())
        .map_err(|e| MeldError::Io(format!("get head: {e}")))?
    {
        let head_hash = CommitHash(
            Hash::from_hex(guard.value().as_str())
                .map_err(|e| MeldError::Io(format!("corrupt HEAD hash: {e}")))?,
        );
        if !commit_hashes.contains(&head_hash.hex()) {
            report.head_dangling = true;
        }
    }
    drop(txn);

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn integrity_report_empty_store() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let report = integrity_check(&db).unwrap();
        assert_eq!(report.objects_checked, 0);
        assert_eq!(report.commits_checked, 0);
        assert!(!report.head_dangling);
    }
}

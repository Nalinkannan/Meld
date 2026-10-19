//! Branch and HEAD references.
//!
//! References are updated atomically inside redb write transactions with
//! compare-and-swap semantics to prevent lost updates when concurrent
//! operations modify the same reference.

use crate::schema::{HEAD_KEY, TABLE_BRANCHES, TABLE_HEAD};
use meld_core::MeldError;
use meld_core::ids::{CommitHash, Hash};
use redb::{ReadableTable, TableDefinition};

pub(crate) static BRANCHES: TableDefinition<String, String> = TableDefinition::new(TABLE_BRANCHES);
pub(crate) static HEAD: TableDefinition<String, String> = TableDefinition::new(TABLE_HEAD);

/// List all branch names.
pub fn list_branches(db: &redb::Database) -> Result<Vec<String>, MeldError> {
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let names = {
        let table = match txn.open_table(BRANCHES) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                drop(txn);
                return Ok(vec![]);
            }
            Err(e) => {
                drop(txn);
                return Err(MeldError::Io(format!("open branches table: {e}")));
            }
        };
        let mut names = Vec::new();
        for result in table
            .range::<String>(..)
            .map_err(|e| MeldError::Io(format!("iterate branches: {e}")))?
        {
            let (key, _value) =
                result.map_err(|e| MeldError::Io(format!("iterate branches: {e}")))?;
            names.push(key.value());
        }
        names
    };
    drop(txn);
    Ok(names)
}

/// Read a branch's target commit. Returns None if the branch does not exist.
pub fn get_branch(db: &redb::Database, branch: &str) -> Result<Option<CommitHash>, MeldError> {
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let current = {
        let table = match txn.open_table(BRANCHES) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                drop(txn);
                return Ok(None);
            }
            Err(e) => {
                drop(txn);
                return Err(MeldError::Io(format!("open branches table: {e}")));
            }
        };
        table
            .get(&branch.to_string())
            .map_err(|e| MeldError::Io(format!("get branch {branch}: {e}")))?
            .map(|g| {
                Hash::from_hex(g.value().as_str())
                    .map(CommitHash)
                    .map_err(|e| MeldError::Io(format!("corrupt branch hash for {branch}: {e}")))
            })
            .transpose()
    };
    drop(txn);
    current
}

/// Read HEAD's target commit.
pub fn get_head(db: &redb::Database) -> Result<Option<CommitHash>, MeldError> {
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let current = {
        let table = match txn.open_table(HEAD) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                drop(txn);
                return Ok(None);
            }
            Err(e) => {
                drop(txn);
                return Err(MeldError::Io(format!("open head table: {e}")));
            }
        };
        table
            .get(&HEAD_KEY.to_string())
            .map_err(|e| MeldError::Io(format!("get head: {e}")))?
            .map(|g| {
                Hash::from_hex(g.value().as_str())
                    .map(CommitHash)
                    .map_err(|e| MeldError::Io(format!("corrupt head hash: {e}")))
            })
            .transpose()
    };
    drop(txn);
    current
}

/// Atomically set a branch to `target`, failing if the branch's current value
/// is not `expected` (None means the branch must not exist).
pub fn compare_and_swap_branch(
    db: &redb::Database,
    branch: &str,
    expected: Option<&CommitHash>,
    target: &CommitHash,
) -> Result<(), MeldError> {
    let txn = db
        .begin_write()
        .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
    {
        let mut table = txn
            .open_table(BRANCHES)
            .map_err(|e| MeldError::Io(format!("open branches table: {e}")))?;
        let current_hex: Option<String> = table
            .get(&branch.to_string())
            .map_err(|e| MeldError::Io(format!("get branch {branch}: {e}")))?
            .map(|g| g.value());
        let current: Option<CommitHash> =
            match current_hex.as_deref() {
                Some(s) => Some(Hash::from_hex(s).map(CommitHash).map_err(|e| {
                    MeldError::Io(format!("corrupt branch hash for {branch}: {e}"))
                })?),
                None => None,
            };

        match (expected, current) {
            (None, Some(_)) => {
                return Err(MeldError::Reference(format!(
                    "branch {branch} already exists (would overwrite)"
                )));
            }
            (Some(e), None) => {
                return Err(MeldError::Reference(format!(
                    "branch {branch} does not exist (expected {e})"
                )));
            }
            (Some(e), Some(c)) if e != &c => {
                return Err(MeldError::Reference(format!(
                    "branch {branch} changed concurrently (expected {e}, found {c})"
                )));
            }
            _ => {}
        }

        table
            .insert(branch.to_string(), target.hex())
            .map_err(|e| MeldError::Io(format!("set branch {branch}: {e}")))?;
    }
    txn.commit()
        .map_err(|e| MeldError::Io(format!("commit branch {branch}: {e}")))?;
    Ok(())
}

/// Create a branch, failing if it already exists.
pub fn create_branch(
    db: &redb::Database,
    branch: &str,
    target: &CommitHash,
) -> Result<(), MeldError> {
    compare_and_swap_branch(db, branch, None, target)
}

/// Update a branch, failing unless it currently points at `expected`.
pub fn update_branch(
    db: &redb::Database,
    branch: &str,
    expected: &CommitHash,
    target: &CommitHash,
) -> Result<(), MeldError> {
    compare_and_swap_branch(db, branch, Some(expected), target)
}

/// Delete a branch, failing unless it currently points at `expected`.
pub fn delete_branch(
    db: &redb::Database,
    branch: &str,
    expected: &CommitHash,
) -> Result<(), MeldError> {
    let txn = db
        .begin_write()
        .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
    {
        let mut table = txn
            .open_table(BRANCHES)
            .map_err(|e| MeldError::Io(format!("open branches table: {e}")))?;
        let current_hex: Option<String> = table
            .get(&branch.to_string())
            .map_err(|e| MeldError::Io(format!("get branch {branch}: {e}")))?
            .map(|g| g.value());
        let Some(current) = current_hex
            .as_deref()
            .and_then(|s| Hash::from_hex(s).ok().map(CommitHash))
        else {
            return Err(MeldError::Reference(format!(
                "branch {branch} does not exist"
            )));
        };
        if current != *expected {
            return Err(MeldError::Reference(format!(
                "branch {branch} changed concurrently (expected {expected}, found {current})"
            )));
        }
        table
            .remove(&branch.to_string())
            .map_err(|e| MeldError::Io(format!("remove branch {branch}: {e}")))?;
    }
    txn.commit()
        .map_err(|e| MeldError::Io(format!("commit delete branch {branch}: {e}")))?;
    Ok(())
}

/// Atomically set HEAD to `target`.
pub fn set_head(db: &redb::Database, target: &CommitHash) -> Result<(), MeldError> {
    let txn = db
        .begin_write()
        .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
    {
        let mut table = txn
            .open_table(HEAD)
            .map_err(|e| MeldError::Io(format!("open head table: {e}")))?;
        table
            .insert(HEAD_KEY.to_string(), target.hex())
            .map_err(|e| MeldError::Io(format!("set head: {e}")))?;
    }
    txn.commit()
        .map_err(|e| MeldError::Io(format!("commit head: {e}")))?;
    Ok(())
}

/// Atomically update a branch (compare-and-swap) and set HEAD in a single
/// store transaction. Either both references change or neither does, so a
/// failure can never leave the active branch and HEAD inconsistent.
///
/// `expected` follows `compare_and_swap_branch` semantics: `None` requires
/// the branch not to exist; `Some(h)` requires the branch to point at `h`.
pub fn compare_and_swap_branch_and_head(
    db: &redb::Database,
    branch: &str,
    expected: Option<&CommitHash>,
    target: &CommitHash,
) -> Result<(), MeldError> {
    let txn = db
        .begin_write()
        .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
    {
        let mut table = txn
            .open_table(BRANCHES)
            .map_err(|e| MeldError::Io(format!("open branches table: {e}")))?;
        let current_hex: Option<String> = table
            .get(&branch.to_string())
            .map_err(|e| MeldError::Io(format!("get branch {branch}: {e}")))?
            .map(|g| g.value());
        let current: Option<CommitHash> =
            match current_hex.as_deref() {
                Some(s) => Some(Hash::from_hex(s).map(CommitHash).map_err(|e| {
                    MeldError::Io(format!("corrupt branch hash for {branch}: {e}"))
                })?),
                None => None,
            };
        match (expected, current) {
            (None, Some(_)) => {
                return Err(MeldError::Reference(format!(
                    "branch {branch} already exists (would overwrite)"
                )));
            }
            (Some(e), None) => {
                return Err(MeldError::Reference(format!(
                    "branch {branch} does not exist (expected {e})"
                )));
            }
            (Some(e), Some(c)) if e != &c => {
                return Err(MeldError::Reference(format!(
                    "branch {branch} changed concurrently (expected {e}, found {c})"
                )));
            }
            _ => {}
        }
        table
            .insert(branch.to_string(), target.hex())
            .map_err(|e| MeldError::Io(format!("set branch {branch}: {e}")))?;

        let mut head = txn
            .open_table(HEAD)
            .map_err(|e| MeldError::Io(format!("open head table: {e}")))?;
        head.insert(HEAD_KEY.to_string(), target.hex())
            .map_err(|e| MeldError::Io(format!("set head: {e}")))?;
    }
    txn.commit()
        .map_err(|e| MeldError::Io(format!("commit branch and head: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use meld_core::ids::ObjectHash;
    use meld_core::state::{EngineKind, SnapshotScope};
    use meld_core::transition::ChangeSummary;
    use tempfile::TempDir;

    fn commit_for(hash: [u8; 32]) -> meld_core::commit::Commit {
        meld_core::commit::Commit::new(
            vec![],
            "alice",
            "msg",
            EngineKind::PostgreSql,
            SnapshotScope::postgres("db"),
            ObjectHash::from_bytes(hash),
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
    fn branch_cas_prevents_lost_update() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let c1 = commit_for([1; 32]);
        let c2 = commit_for([2; 32]);
        create_branch(&db, "main", &c1.hash).unwrap();
        assert!(create_branch(&db, "main", &c2.hash).is_err());
        assert!(update_branch(&db, "main", &c2.hash, &c1.hash).is_err());
        update_branch(&db, "main", &c1.hash, &c2.hash).unwrap();
        assert_eq!(get_branch(&db, "main").unwrap(), Some(c2.hash));
        assert!(delete_branch(&db, "main", &c1.hash).is_err());
        delete_branch(&db, "main", &c2.hash).unwrap();
        assert_eq!(get_branch(&db, "main").unwrap(), None);
    }

    #[test]
    fn head_read_write() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        assert_eq!(get_head(&db).unwrap(), None);
        let c = commit_for([5; 32]);
        set_head(&db, &c.hash).unwrap();
        assert_eq!(get_head(&db).unwrap(), Some(c.hash));
    }
}

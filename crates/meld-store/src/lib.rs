//! Durable local version-history storage for Meld.
//!
//! Backed by `redb` (an embedded, single-writer key-value store) and BLAKE3
//! content addressing. Objects and commits are immutable and integrity-checked
//! on every read. Branch and HEAD references are updated atomically inside
//! redb write transactions with compare-and-swap to prevent lost updates.

pub mod commit_store;
pub mod connection_store;
pub mod integrity;
pub mod object_store;
pub mod refs;
pub mod schema;

use chrono::Utc;
use meld_core::ids::Hash;
use meld_core::{Commit, CommitHash, MeldError, ObjectHash};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use connection_store::ConnectionProfile;

/// Configuration for opening a Meld repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    pub repo_path: PathBuf,
}

/// Handle to an open Meld repository.
#[derive(Debug)]
pub struct Store {
    db: redb::Database,
    path: PathBuf,
    config: StoreConfig,
}

impl Store {
    /// Open (or create) the repository at `config.repo_path`.
    pub fn open(config: StoreConfig) -> Result<Self, MeldError> {
        let repo_path = &config.repo_path;
        if let Some(parent) = repo_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                MeldError::Io(format!(
                    "failed to create repo dir {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let db = if repo_path.exists() {
            redb::Database::open(repo_path).map_err(|e| {
                MeldError::Io(format!(
                    "failed to open store at {}: {e}",
                    repo_path.display()
                ))
            })?
        } else {
            redb::Database::create(repo_path).map_err(|e| {
                MeldError::Io(format!(
                    "failed to create store at {}: {e}",
                    repo_path.display()
                ))
            })?
        };

        let store = Self {
            db,
            path: repo_path.clone(),
            config,
        };
        store.initialize_schema()?;
        Ok(store)
    }

    fn initialize_schema(&self) -> Result<(), MeldError> {
        let existing: Option<String> = {
            let txn = self
                .db
                .begin_read()
                .map_err(|e| MeldError::Io(format!("failed to begin read transaction: {e}")))?;
            match txn.open_table(schema::STORE_META_TABLE) {
                Ok(meta) => redb::ReadableTable::get(&meta, &"schema".to_string())
                    .map_err(|e| MeldError::Io(format!("failed to read schema: {e}")))?
                    .map(|v| v.value().to_string()),
                Err(redb::TableError::TableDoesNotExist(_)) => None,
                Err(e) => return Err(MeldError::Io(format!("failed to open meta table: {e}"))),
            }
        };

        let needs_init = existing.is_none();
        if let Some(version) = existing.as_deref() {
            let parsed: u32 =
                version
                    .parse()
                    .map_err(|_| MeldError::IncompatibleFormatVersion {
                        expected: schema::STORE_SCHEMA_VERSION,
                        got: version.parse::<u32>().unwrap_or(0),
                    })?;
            if parsed != schema::STORE_SCHEMA_VERSION {
                return Err(MeldError::IncompatibleFormatVersion {
                    expected: schema::STORE_SCHEMA_VERSION,
                    got: parsed,
                });
            }
        }

        if needs_init {
            // Create all of Meld's tables. Opening a table on a write
            // transaction creates it if it does not yet exist.
            let txn = self
                .db
                .begin_write()
                .map_err(|e| MeldError::Io(format!("failed to begin write transaction: {e}")))?;
            {
                let _objects = txn
                    .open_table(schema::OBJECTS_TABLE)
                    .map_err(|e| MeldError::Io(format!("failed to create objects table: {e}")))?;
                let _commits = txn
                    .open_table(schema::COMMITS_TABLE)
                    .map_err(|e| MeldError::Io(format!("failed to create commits table: {e}")))?;
                let _branches = txn
                    .open_table(schema::BRANCHES_TABLE)
                    .map_err(|e| MeldError::Io(format!("failed to create branches table: {e}")))?;
                let _head = txn
                    .open_table(schema::HEAD_TABLE)
                    .map_err(|e| MeldError::Io(format!("failed to create head table: {e}")))?;
                let _connections = txn.open_table(schema::CONNECTIONS_TABLE).map_err(|e| {
                    MeldError::Io(format!("failed to create connections table: {e}"))
                })?;
                let mut meta = txn
                    .open_table(schema::STORE_META_TABLE)
                    .map_err(|e| MeldError::Io(format!("failed to open meta table: {e}")))?;
                meta.insert(
                    "schema".to_string(),
                    schema::STORE_SCHEMA_VERSION.to_string(),
                )
                .map_err(|e| MeldError::Io(format!("failed to write schema: {e}")))?;
            }
            txn.commit()
                .map_err(|e| MeldError::Io(format!("commit failed: {e}")))?;
        }
        Ok(())
    }

    /// Return the repository path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the store configuration.
    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    /// Return the underlying redb database (for catalog access).
    pub fn db(&self) -> &redb::Database {
        &self.db
    }

    /// Store an opaque byte object, returning its BLAKE3 content address.
    pub fn put_object(&self, bytes: &[u8]) -> Result<ObjectHash, MeldError> {
        object_store::put_object(&self.db, bytes)
    }

    /// Retrieve and integrity-check an object by its content address.
    pub fn get_object(&self, hash: &ObjectHash) -> Result<Vec<u8>, MeldError> {
        object_store::get_object(&self.db, hash)
    }

    /// Persist an immutable commit, content-addressed by its hash.
    pub fn put_commit(&self, commit: &Commit) -> Result<CommitHash, MeldError> {
        commit_store::put_commit(&self.db, commit)
    }

    /// Retrieve a commit by its hash (integrity-checked).
    pub fn get_commit(&self, hash: &CommitHash) -> Result<Commit, MeldError> {
        commit_store::get_commit(&self.db, hash)
    }

    /// Create a branch, failing if it already exists.
    pub fn create_branch(&self, branch: &str, target: &CommitHash) -> Result<(), MeldError> {
        refs::create_branch(&self.db, branch, target)
    }

    /// Atomically update `branch` (compare-and-swap against `expected`) and
    /// set HEAD to `target` in a single store transaction, so branch and HEAD
    /// can never be left inconsistent by a partial update.
    pub fn compare_and_swap_branch_and_head(
        &self,
        branch: &str,
        expected: Option<&CommitHash>,
        target: &CommitHash,
    ) -> Result<(), MeldError> {
        refs::compare_and_swap_branch_and_head(&self.db, branch, expected, target)
    }

    /// Update a branch, failing unless it currently points at `expected`.
    pub fn update_branch(
        &self,
        branch: &str,
        expected: &CommitHash,
        target: &CommitHash,
    ) -> Result<(), MeldError> {
        refs::update_branch(&self.db, branch, expected, target)
    }

    /// Delete a branch, failing unless it currently points at `expected`.
    pub fn delete_branch(&self, branch: &str, expected: &CommitHash) -> Result<(), MeldError> {
        refs::delete_branch(&self.db, branch, expected)
    }

    /// List all branch names.
    pub fn list_branches(&self) -> Result<Vec<String>, MeldError> {
        refs::list_branches(&self.db)
    }

    /// Read a branch's target commit.
    pub fn get_branch(&self, branch: &str) -> Result<Option<CommitHash>, MeldError> {
        refs::get_branch(&self.db, branch)
    }

    /// Atomically set HEAD to `target`.
    pub fn set_head(&self, target: &CommitHash) -> Result<(), MeldError> {
        refs::set_head(&self.db, target)
    }

    /// Read HEAD's target commit.
    pub fn get_head(&self) -> Result<Option<CommitHash>, MeldError> {
        refs::get_head(&self.db)
    }

    /// Save a connection profile (passwords are rejected).
    pub fn save_profile(&self, profile: &ConnectionProfile) -> Result<(), MeldError> {
        connection_store::save_profile(&self.db, profile)
    }

    /// Read a saved connection profile by name.
    pub fn get_profile(&self, name: &str) -> Result<Option<ConnectionProfile>, MeldError> {
        connection_store::get_profile(&self.db, name)
    }

    /// List all saved connection profiles.
    pub fn list_profiles(&self) -> Result<Vec<ConnectionProfile>, MeldError> {
        connection_store::list_profiles(&self.db)
    }

    /// Delete a saved connection profile.
    pub fn delete_profile(&self, name: &str) -> Result<(), MeldError> {
        connection_store::delete_profile(&self.db, name)
    }

    /// Run an integrity check over all objects and commits.
    pub fn integrity_check(&self) -> Result<integrity::IntegrityReport, MeldError> {
        integrity::integrity_check(&self.db)
    }

    /// List all commit hashes in the repository.
    pub fn list_all_commit_hashes(&self) -> Result<Vec<CommitHash>, MeldError> {
        commit_store::list_all_commit_hashes(&self.db)
    }

    /// Key used in the store-meta table for the restore-in-progress marker.
    const RESTORE_MARKER_KEY: &str = "restore_in_progress";

    /// Record that a restore is in progress, with the safety-snapshot commit
    /// hash that can be used to recover if the restore fails.
    pub fn set_restore_marker(&self, safety_snapshot: &CommitHash) -> Result<(), MeldError> {
        let json = serde_json::to_string(&safety_snapshot.hex())
            .map_err(|e| MeldError::Serialization(format!("{e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
        {
            let mut meta = txn
                .open_table(schema::STORE_META_TABLE)
                .map_err(|e| MeldError::Io(format!("open meta table: {e}")))?;
            meta.insert(Self::RESTORE_MARKER_KEY.to_string(), json)
                .map_err(|e| MeldError::Io(format!("set restore marker: {e}")))?;
        }
        txn.commit()
            .map_err(|e| MeldError::Io(format!("commit restore marker: {e}")))?;
        Ok(())
    }

    /// Clear the restore-in-progress marker after a successful restore.
    pub fn clear_restore_marker(&self) -> Result<(), MeldError> {
        let txn = self
            .db
            .begin_write()
            .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
        {
            let mut meta = txn
                .open_table(schema::STORE_META_TABLE)
                .map_err(|e| MeldError::Io(format!("open meta table: {e}")))?;
            meta.remove(Self::RESTORE_MARKER_KEY.to_string())
                .map_err(|e| MeldError::Io(format!("clear restore marker: {e}")))?;
        }
        txn.commit()
            .map_err(|e| MeldError::Io(format!("commit clear restore marker: {e}")))?;
        Ok(())
    }

    /// Return the safety-snapshot commit hash if a restore is in progress
    /// (i.e. a previous restore was interrupted). Returns None if no restore
    /// is in progress.
    pub fn get_restore_marker(&self) -> Result<Option<CommitHash>, MeldError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
        let marker = {
            let meta = txn
                .open_table(schema::STORE_META_TABLE)
                .map_err(|e| MeldError::Io(format!("open meta table: {e}")))?;
            meta.get(&Self::RESTORE_MARKER_KEY.to_string())
                .map_err(|e| MeldError::Io(format!("get restore marker: {e}")))?
                .map(|g| g.value())
        };
        drop(txn);
        let Some(hex_json) = marker else {
            return Ok(None);
        };
        let hex: String = serde_json::from_str(&hex_json)
            .map_err(|e| MeldError::Serialization(format!("{e}")))?;
        let hash = Hash::from_hex(&hex)
            .map(CommitHash)
            .map_err(|e| MeldError::InvalidInput(format!("corrupt restore marker hash: {e}")))?;
        Ok(Some(hash))
    }

    /// Record when this store was last touched (for diagnostics).
    pub fn touched_at(&self) -> chrono::DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn open_creates_and_reopens_repository() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("meld.edb");
        {
            let _store = Store::open(StoreConfig {
                repo_path: path.clone(),
            })
            .unwrap();
        }
        // Reopen: data persists.
        let _store = Store::open(StoreConfig {
            repo_path: path.clone(),
        })
        .unwrap();
    }

    #[test]
    fn incompatible_schema_version_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("meld.edb");
        {
            let store = Store::open(StoreConfig {
                repo_path: path.clone(),
            })
            .unwrap();
            let txn = store.db.begin_write().unwrap();
            {
                let mut meta = txn.open_table(schema::STORE_META_TABLE).unwrap();
                meta.insert("schema".to_string(), "999".to_string())
                    .unwrap();
            }
            txn.commit().unwrap();
        }
        let err = Store::open(StoreConfig { repo_path: path }).unwrap_err();
        assert!(matches!(err, MeldError::IncompatibleFormatVersion { .. }));
    }
}

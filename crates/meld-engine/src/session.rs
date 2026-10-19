//! Session management: connection + store + current branch.
//!
//! A `Session` ties together a `meld_store::Store` and a connected
//! `meld_adapter::DatabaseAdapter`, and tracks the current branch/HEAD.

use meld_adapter::{ConnectionConfig, ConnectionInfo, DatabaseAdapter, registry::AdapterRegistry};
use meld_core::ids::CommitHash;
use meld_core::state::EngineKind;
use meld_store::Store;

use crate::error::EngineError;

/// A user session: an open local store plus an optional active database
/// connection and the current branch.
#[derive(Debug)]
pub struct Session {
    pub store: Store,
    pub adapter: Option<Box<dyn DatabaseAdapter>>,
    pub connection: Option<ConnectionConfig>,
    pub current_branch: String,
}

impl Session {
    /// Open (or create) the local repository at `repo_path`.
    pub fn open(repo_path: impl Into<std::path::PathBuf>) -> Result<Self, EngineError> {
        let store = Store::open(meld_store::StoreConfig {
            repo_path: repo_path.into(),
        })
        .map_err(|e| EngineError::Session(e.to_string()))?;
        Ok(Self {
            store,
            adapter: None,
            connection: None,
            current_branch: "main".to_string(),
        })
    }

    /// Connect to a database using the given configuration and remember it.
    pub async fn connect(
        &mut self,
        config: ConnectionConfig,
    ) -> Result<ConnectionInfo, EngineError> {
        let engine_kind = match &config {
            ConnectionConfig::PostgreSql(_) => EngineKind::PostgreSql,
            ConnectionConfig::MongoDb(_) => EngineKind::MongoDb,
        };
        let mut adapter = AdapterRegistry::new().adapter_for(engine_kind);
        let info = adapter
            .connect(&config)
            .await
            .map_err(|e| EngineError::Session(e.to_string()))?;
        self.connection = Some(config);
        self.adapter = Some(adapter);
        Ok(info)
    }

    /// Disconnect from the database (keeping the local store open).
    pub async fn disconnect(&mut self) -> Result<(), EngineError> {
        if let Some(adapter) = self.adapter.as_mut() {
            adapter
                .disconnect()
                .await
                .map_err(|e| EngineError::Session(e.to_string()))?;
        }
        self.adapter = None;
        self.connection = None;
        Ok(())
    }

    /// Return the currently connected adapter (required for most operations).
    pub fn adapter(&self) -> Result<&dyn DatabaseAdapter, EngineError> {
        self.adapter
            .as_ref()
            .map(|a| a.as_ref())
            .ok_or_else(|| EngineError::Session("no database connection".into()))
    }

    pub fn adapter_mut(&mut self) -> Result<&mut dyn DatabaseAdapter, EngineError> {
        match &mut self.adapter {
            Some(a) => Ok(a.as_mut()),
            None => Err(EngineError::Session("no database connection".into())),
        }
    }

    /// Return the current branch name.
    pub fn branch(&self) -> &str {
        &self.current_branch
    }

    /// Return the current HEAD commit, if any.
    pub fn head(&self) -> Result<Option<CommitHash>, EngineError> {
        self.store
            .get_head()
            .map_err(|e| EngineError::Session(e.to_string()))
    }

    /// List all local branches.
    pub fn branches(&self) -> Result<Vec<String>, EngineError> {
        self.store
            .list_branches()
            .map_err(|e| EngineError::Session(e.to_string()))
    }

    /// Execute an ad-hoc query against the connected database through the
    /// active adapter. Read-only mode is enforced server-side where the
    /// engine supports it.
    pub async fn execute_query(
        &self,
        request: meld_adapter::adapter::QueryRequest,
    ) -> Result<meld_adapter::adapter::QueryResult, EngineError> {
        let adapter = self.adapter()?;
        adapter
            .execute_query(request)
            .await
            .map_err(|e| EngineError::Session(e.to_string()))
    }

    /// Recover from an interrupted restore, if one is detected.
    ///
    /// If a restore-marker exists in the store, a previous restore was
    /// interrupted. This restores the live database from the safety snapshot
    /// commit recorded in the marker, then clears the marker.
    pub async fn recover_interrupted_restore(&mut self) -> Result<Option<CommitHash>, EngineError> {
        let Some(safety_hash) = self
            .store
            .get_restore_marker()
            .map_err(|e| EngineError::Session(format!("failed to read restore marker: {e}")))?
        else {
            return Ok(None);
        };

        let safety_commit = self
            .store
            .get_commit(&safety_hash)
            .map_err(|e| EngineError::Session(format!("failed to load safety snapshot: {e}")))?;

        let adapter = self.adapter_mut()?;
        // Restore the live database to the safety snapshot state.
        let result = adapter
            .execute_restore_plan(&{
                // Build a plan from the safety snapshot vs live.
                use crate::restore::build_restore_plan;
                build_restore_plan(adapter, &safety_commit)
                    .await
                    .map_err(|e| {
                        EngineError::Session(format!("failed to build recovery plan: {e}"))
                    })?
            })
            .await
            .map_err(|e| EngineError::Session(format!("recovery restore failed: {e}")))?;

        if result.operations_failed > 0 {
            return Err(EngineError::Session(format!(
                "interrupted restore recovery failed: {}; safety snapshot id: {}",
                result.messages.join("; "),
                safety_hash.short(12)
            )));
        }

        // Verify the recovery.
        let report = adapter
            .verify(&safety_commit.state)
            .await
            .map_err(|e| EngineError::Session(format!("recovery verification failed: {e}")))?;
        if !report.passed {
            return Err(EngineError::Session(format!(
                "interrupted restore recovery verification failed; safety snapshot id: {}",
                safety_hash.short(12)
            )));
        }

        // Move HEAD to the safety snapshot and clear the marker.
        self.store.set_head(&safety_hash).map_err(|e| {
            EngineError::Session(format!("failed to advance HEAD after recovery: {e}"))
        })?;
        self.store.clear_restore_marker().map_err(|e| {
            EngineError::Session(format!(
                "failed to clear restore marker after recovery: {e}"
            ))
        })?;

        Ok(Some(safety_hash))
    }

    /// Create a new branch at `target` (defaults to HEAD if None).
    pub async fn create_branch(
        &mut self,
        name: &str,
        target: Option<CommitHash>,
    ) -> Result<(), EngineError> {
        let target = match target {
            Some(t) => t,
            None => self
                .head()?
                .ok_or_else(|| EngineError::Branch("cannot create branch: HEAD is empty".into()))?,
        };
        self.store
            .create_branch(name, &target)
            .map_err(|e| EngineError::Branch(e.to_string()))
    }

    /// Switch the current branch to `name` (HEAD moves to the branch tip).
    pub async fn switch_branch(&mut self, name: &str) -> Result<(), EngineError> {
        let target = self
            .store
            .get_branch(name)
            .map_err(|e| EngineError::Branch(e.to_string()))?
            .ok_or_else(|| EngineError::Branch(format!("branch {name} does not exist")))?;
        self.store
            .set_head(&target)
            .map_err(|e| EngineError::Branch(e.to_string()))?;
        self.current_branch = name.to_string();
        Ok(())
    }
}

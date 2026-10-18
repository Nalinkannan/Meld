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

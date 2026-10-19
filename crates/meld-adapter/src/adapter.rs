//! The database adapter abstraction.
//!
//! `DatabaseAdapter` is the interface through which the engine talks to
//! PostgreSQL and MongoDB. Driver-specific types never leak into `meld-core`.

use crate::capabilities::Capabilities;
use meld_core::{
    commit::Commit, error::MeldError, ids::CommitHash, state::CanonicalValue, state::EngineState,
};
use serde::{Deserialize, Serialize};

/// Connection configuration for a database.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConnectionConfig {
    PostgreSql(PostgresConfig),
    MongoDb(MongoConfig),
}

/// PostgreSQL connection configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PostgresConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Password is held only in memory; never persisted by the store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub database: String,
    /// Optional libpq-style connection URI (takes precedence when set).
    pub uri: Option<String>,
}

/// MongoDB connection configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MongoConfig {
    pub name: String,
    /// Connection URI (preferred) or host:port.
    pub uri: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub auth_source: Option<String>,
}

/// Information about an established connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub engine_kind: meld_core::state::EngineKind,
    pub version: String,
    pub database_name: String,
}

/// Result of executing a restore plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub operations_executed: u64,
    pub operations_succeeded: u64,
    pub operations_failed: u64,
    pub recovered: bool,
    pub final_state: Option<EngineState>,
    pub messages: Vec<String>,
}

/// Snapshot capture result returned by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotResult {
    pub commit: Commit,
    pub commit_hash: CommitHash,
    pub engine_kind: meld_core::state::EngineKind,
    pub change_summary: meld_core::transition::ChangeSummary,
    pub consistency: meld_core::state::ConsistencyLevel,
    pub warnings: Vec<String>,
}

/// The asynchronous database adapter trait.
#[async_trait::async_trait]
pub trait DatabaseAdapter: Send + Sync + std::fmt::Debug {
    fn engine_kind(&self) -> meld_core::state::EngineKind;
    fn capabilities(&self) -> Capabilities;
    fn is_connected(&self) -> bool;

    /// Validate a connection configuration without establishing a connection.
    async fn test_connection(&self, config: &ConnectionConfig)
    -> Result<ConnectionInfo, MeldError>;

    /// Establish and hold a connection for subsequent operations.
    async fn connect(&mut self, config: &ConnectionConfig) -> Result<ConnectionInfo, MeldError>;

    /// Release the held connection.
    async fn disconnect(&mut self) -> Result<(), MeldError>;

    /// Capture the current database state (a snapshot).
    async fn introspect(&self) -> Result<EngineState, MeldError>;

    /// Extract rows for a table (PostgreSQL). Used by the engine to build
    /// row-aware restore plans.
    async fn extract_table_rows(
        &self,
        table: &meld_core::state::PgTable,
    ) -> Result<Vec<meld_core::state::PgRow>, MeldError>;

    /// Independently compare the live database against a target state.
    async fn verify(
        &self,
        target: &EngineState,
    ) -> Result<meld_core::verify::VerificationReport, MeldError>;

    /// Execute a dependency-aware restore plan against the live database,
    /// transforming it into the plan's target state. The plan's operations
    /// carry their execution details; the adapter applies them in order.
    async fn execute_restore_plan(
        &self,
        plan: &meld_core::plan::RestorePlan,
    ) -> Result<ExecutionResult, MeldError>;

    /// Execute an ad-hoc query against the live database. This is separate
    /// from snapshot, diff, restore, and merge workflows. Read-only mode is
    /// enforced server-side where the engine supports it (e.g. PostgreSQL
    /// `SET TRANSACTION READ ONLY`).
    async fn execute_query(&self, request: QueryRequest) -> Result<QueryResult, MeldError>;

    async fn health_check(&self) -> Result<(), MeldError>;
}

/// A typed ad-hoc query request. Variants are engine-specific; driver types
/// never leak into `meld-core`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryRequest {
    /// PostgreSQL SQL.
    PostgreSql {
        sql: String,
        /// When true, the query runs in a read-only transaction and writes
        /// are rejected by the server.
        read_only: bool,
    },
    /// MongoDB structured query.
    MongoDb {
        collection: String,
        /// JSON filter document (default: match all).
        filter: Option<String>,
        /// JSON projection document.
        projection: Option<String>,
        /// JSON sort document.
        sort: Option<String>,
        /// Maximum number of documents to return.
        limit: Option<u64>,
    },
}

/// Typed result of an ad-hoc query execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    /// Row cells aligned with `columns`.
    pub rows: Vec<Vec<CanonicalValue>>,
    pub row_count: u64,
    /// True when the result was truncated to the server-side row cap.
    pub truncated: bool,
    pub duration_ms: u64,
}

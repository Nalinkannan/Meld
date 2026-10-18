//! Capability model for database adapters.
//!
//! Each adapter declares which operations it actually implements and tests.
//! Unsupported operations must return actionable errors rather than silently
//! succeeding.

use meld_core::state::ConsistencyLevel;
use serde::{Deserialize, Serialize};

/// Declared capabilities of a database adapter.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capabilities {
    pub engine_kind: meld_core::state::EngineKind,
    /// Schema/catalog introspection.
    pub schema_introspection: bool,
    /// Data extraction.
    pub data_extraction: bool,
    /// Full snapshot capture.
    pub snapshot_capture: bool,
    /// Semantic state comparison.
    pub state_comparison: bool,
    /// Restore planning and execution.
    pub restore_planning: bool,
    /// Data insertion/update/delete.
    pub data_write: bool,
    /// Index and constraint management.
    pub index_constraint_management: bool,
    /// Transaction support for captures/restores.
    pub transactions: bool,
    /// Independent verification.
    pub independent_verification: bool,
    /// Supported consistency guarantee for captures.
    pub consistency: ConsistencyLevel,
    /// Human-readable description of the supported restore scope.
    pub supported_scope: String,
    /// Capture warnings that apply to every snapshot from this adapter.
    pub capture_warnings: Vec<String>,
}

impl Capabilities {
    pub fn postgres() -> Self {
        Self {
            engine_kind: meld_core::state::EngineKind::PostgreSql,
            schema_introspection: true,
            data_extraction: true,
            snapshot_capture: true,
            state_comparison: true,
            restore_planning: true,
            data_write: true,
            index_constraint_management: true,
            transactions: true,
            independent_verification: true,
            consistency: ConsistencyLevel::Snapshot,
            supported_scope:
                "schemas, tables, columns, constraints, indexes, views, sequences, rows".into(),
            capture_warnings: vec![],
        }
    }

    pub fn mongodb() -> Self {
        Self {
            engine_kind: meld_core::state::EngineKind::MongoDb,
            schema_introspection: true,
            data_extraction: true,
            snapshot_capture: true,
            state_comparison: true,
            restore_planning: true,
            data_write: true,
            index_constraint_management: true,
            transactions: false,
            independent_verification: true,
            consistency: ConsistencyLevel::BestEffort,
            supported_scope: "database, collections, documents, indexes, validators, views".into(),
            capture_warnings: vec![
                "Multi-document transactions are only available on replica sets and sharded clusters with WiredTiger; \
                 single-node captures are best-effort and may be inconsistent under concurrent writes.".into(),
            ],
        }
    }
}

/// Server information returned by a connection test.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServerInfo {
    pub engine_kind: meld_core::state::EngineKind,
    pub version: String,
    pub host: String,
    pub port: Option<u16>,
    pub database_name: Option<String>,
}

/// Result of validating a connection configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionValidation {
    pub valid: bool,
    pub server_info: Option<ServerInfo>,
    pub errors: Vec<String>,
}

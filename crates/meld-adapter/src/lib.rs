//! Database abstraction layer for Meld.
//!
//! Provides the `DatabaseAdapter` trait, capability declarations, connection
//! configuration, and the PostgreSQL/MongoDB adapter implementations. Driver
//! types never leak into `meld-core`.

pub mod adapter;
pub mod capabilities;
pub mod mongodb;
pub mod postgres;
pub mod registry;

pub use adapter::{
    ConnectionConfig, ConnectionInfo, DatabaseAdapter, ExecutionResult, MongoConfig,
    PostgresConfig, QueryRequest, QueryResult, SnapshotResult,
};
pub use capabilities::{Capabilities, ConnectionValidation, ServerInfo};
pub use registry::AdapterRegistry;

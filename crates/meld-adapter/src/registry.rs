//! Adapter registry.
//!
//! Creates the correct adapter from the selected engine and configuration.

use crate::adapter::{ConnectionConfig, DatabaseAdapter};
use crate::mongodb::MongoAdapter;
use crate::postgres::PostgresAdapter;
use meld_core::error::MeldError;

/// Registry that maps engine kinds to adapter instances.
#[derive(Debug)]
pub struct AdapterRegistry;

impl AdapterRegistry {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterRegistry {
    /// Create a fresh adapter for the given engine kind.
    pub fn adapter_for(
        &self,
        engine_kind: meld_core::state::EngineKind,
    ) -> Box<dyn DatabaseAdapter> {
        match engine_kind {
            meld_core::state::EngineKind::PostgreSql => Box::new(PostgresAdapter::new()),
            meld_core::state::EngineKind::MongoDb => Box::new(MongoAdapter::new()),
        }
    }

    /// Create and connect an adapter for the given configuration.
    pub async fn connect(
        &self,
        config: &ConnectionConfig,
    ) -> Result<(Box<dyn DatabaseAdapter>, crate::adapter::ConnectionInfo), MeldError> {
        match config {
            ConnectionConfig::PostgreSql(_) => {
                let mut adapter = PostgresAdapter::new();
                let info = adapter.connect(config).await?;
                Ok((Box::new(adapter) as Box<dyn DatabaseAdapter>, info))
            }
            ConnectionConfig::MongoDb(_) => {
                let mut adapter = MongoAdapter::new();
                let info = adapter.connect(config).await?;
                Ok((Box::new(adapter) as Box<dyn DatabaseAdapter>, info))
            }
        }
    }

    /// Validate a connection configuration without connecting.
    pub async fn test_connection(
        &self,
        config: &ConnectionConfig,
    ) -> Result<crate::capabilities::ServerInfo, MeldError> {
        match config {
            ConnectionConfig::PostgreSql(pg) => {
                let adapter = PostgresAdapter::new();
                let info = adapter.test_connection(config).await?;
                Ok(crate::capabilities::ServerInfo {
                    engine_kind: info.engine_kind,
                    version: info.version,
                    host: pg.host.clone(),
                    port: Some(pg.port),
                    database_name: Some(info.database_name),
                })
            }
            ConnectionConfig::MongoDb(mongo) => {
                let adapter = MongoAdapter::new();
                let info = adapter.test_connection(config).await?;
                let host = mongo.host.as_deref().unwrap_or("localhost").to_string();
                let port = mongo.port;
                Ok(crate::capabilities::ServerInfo {
                    engine_kind: info.engine_kind,
                    version: info.version,
                    host,
                    port,
                    database_name: Some(info.database_name),
                })
            }
        }
    }
}

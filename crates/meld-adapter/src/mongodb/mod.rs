//! MongoDB adapter.
//!
//! Uses the official MongoDB Rust driver with BSON types. BSON type
//! distinctions are preserved: a string `_id` and an ObjectId `_id` are
//! never treated as the same identity.

use crate::adapter::{ConnectionInfo, DatabaseAdapter, ExecutionResult, MongoConfig};
use crate::capabilities::Capabilities;
use meld_core::error::MeldError;
use meld_core::state::{EngineKind, EngineState};
use meld_core::verify::VerificationReport;
use tracing::info;

pub mod apply;
pub mod extract;
pub mod introspect;
pub mod verify;

/// MongoDB database adapter.
#[derive(Debug)]
pub struct MongoAdapter {
    client: Option<mongodb::Client>,
    database_name: Option<String>,
}

impl MongoAdapter {
    pub fn new() -> Self {
        Self {
            client: None,
            database_name: None,
        }
    }
    fn client(&self) -> Result<&mongodb::Client, MeldError> {
        self.client
            .as_ref()
            .ok_or_else(|| MeldError::InvalidInput("not connected to a MongoDB server".into()))
    }

    fn db_name(&self) -> Result<&str, MeldError> {
        self.database_name
            .as_deref()
            .ok_or_else(|| MeldError::InvalidInput("no database selected".into()))
    }

    fn build_uri(config: &MongoConfig) -> Result<String, MeldError> {
        let uri = config.uri.as_deref().filter(|s| !s.is_empty());
        if let Some(uri) = uri {
            return Ok(uri.to_string());
        }
        let host = config.host.as_deref().unwrap_or("localhost");
        let port = config.port.unwrap_or(27017);
        match (&config.username, &config.password) {
            (Some(user), Some(pass)) => {
                let auth_source = config.auth_source.as_deref().unwrap_or("admin");
                Ok(format!(
                    "mongodb://{user}:{pass}@{host}:{port}/{}?authSource={auth_source}",
                    config.database
                ))
            }
            _ => Ok(format!("mongodb://{host}:{port}/{}", config.database)),
        }
    }
}

impl Default for MongoAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl DatabaseAdapter for MongoAdapter {
    fn engine_kind(&self) -> EngineKind {
        EngineKind::MongoDb
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::mongodb()
    }

    fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    async fn test_connection(
        &self,
        config: &crate::adapter::ConnectionConfig,
    ) -> Result<ConnectionInfo, MeldError> {
        match config {
            crate::adapter::ConnectionConfig::MongoDb(mongo) => {
                let uri = Self::build_uri(mongo)?;
                let client = mongodb::Client::with_uri_str(&uri)
                    .await
                    .map_err(|e| MeldError::Io(format!("MongoDB connection test failed: {e}")))?;
                let db = client.database(&mongo.database);
                let info = db
                    .run_command(bson::doc! {"buildInfo": 1}, None)
                    .await
                    .map_err(|e| MeldError::Io(format!("buildInfo failed: {e}")))?;
                let version = info
                    .get_str("version")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| "unknown".into());
                Ok(ConnectionInfo {
                    engine_kind: EngineKind::MongoDb,
                    version,
                    database_name: mongo.database.clone(),
                })
            }
            crate::adapter::ConnectionConfig::PostgreSql(_) => Err(MeldError::IncompatibleEngine {
                expected: EngineKind::MongoDb,
                got: EngineKind::PostgreSql,
            }),
        }
    }

    async fn connect(
        &mut self,
        config: &crate::adapter::ConnectionConfig,
    ) -> Result<ConnectionInfo, MeldError> {
        match config {
            crate::adapter::ConnectionConfig::MongoDb(mongo) => {
                let uri = Self::build_uri(mongo)?;
                let client = mongodb::Client::with_uri_str(&uri)
                    .await
                    .map_err(|e| MeldError::Io(format!("MongoDB connect failed: {e}")))?;
                let version = {
                    let db = client.database(&mongo.database);
                    let info = db
                        .run_command(bson::doc! {"buildInfo": 1}, None)
                        .await
                        .map_err(|e| MeldError::Io(format!("buildInfo failed: {e}")))?;
                    info.get_str("version")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|_| "unknown".into())
                };
                info!(mongodb.version = %version, "connected to mongodb");
                self.client = Some(client);
                self.database_name = Some(mongo.database.clone());
                Ok(ConnectionInfo {
                    engine_kind: EngineKind::MongoDb,
                    version,
                    database_name: mongo.database.clone(),
                })
            }
            crate::adapter::ConnectionConfig::PostgreSql(_) => Err(MeldError::IncompatibleEngine {
                expected: EngineKind::MongoDb,
                got: EngineKind::PostgreSql,
            }),
        }
    }

    async fn disconnect(&mut self) -> Result<(), MeldError> {
        self.client = None;
        Ok(())
    }

    async fn extract_table_rows(
        &self,
        _table: &meld_core::state::PgTable,
    ) -> Result<Vec<meld_core::state::PgRow>, MeldError> {
        Err(MeldError::Unsupported(
            "row extraction not implemented for MongoDB".into(),
        ))
    }

    async fn introspect(&self) -> Result<EngineState, MeldError> {
        introspect::introspect(self.client()?, self.db_name()?).await
    }

    async fn verify(&self, target: &EngineState) -> Result<VerificationReport, MeldError> {
        verify::verify(self.client()?, target).await
    }

    async fn execute_restore_plan(
        &self,
        plan: &meld_core::plan::RestorePlan,
    ) -> Result<ExecutionResult, MeldError> {
        let db_name = self.db_name()?;
        apply::execute(self.client()?, db_name, plan).await
    }

    async fn health_check(&self) -> Result<(), MeldError> {
        self.client()?
            .database("admin")
            .run_command(bson::doc! {"ping": 1}, None)
            .await
            .map_err(|e| MeldError::Io(format!("ping failed: {e}")))?;
        Ok(())
    }
}

impl MongoAdapter {
    #[cfg(test)]
    fn build_uri_for_test(config: &MongoConfig) -> Result<String, MeldError> {
        Self::build_uri(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_uri_prefers_uri() {
        let cfg = MongoConfig {
            name: "x".into(),
            uri: Some("mongodb://custom:27017/db".into()),
            host: None,
            port: None,
            database: "db".into(),
            username: None,
            password: None,
            auth_source: None,
        };
        assert_eq!(
            MongoAdapter::build_uri_for_test(&cfg).unwrap(),
            "mongodb://custom:27017/db"
        );
    }

    #[test]
    fn build_uri_from_fields() {
        let cfg = MongoConfig {
            name: "x".into(),
            uri: None,
            host: Some("localhost".into()),
            port: Some(27017),
            database: "mydb".into(),
            username: Some("u".into()),
            password: Some("p".into()),
            auth_source: Some("admin".into()),
        };
        let uri = MongoAdapter::build_uri_for_test(&cfg).unwrap();
        assert!(uri.starts_with("mongodb://"));
        assert!(uri.contains("localhost:27017/mydb"));
        assert!(uri.contains("authSource=admin"));
    }
}

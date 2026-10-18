//! PostgreSQL adapter.
//!
//! Uses `sqlx` with the `postgres` feature and `native-tls`. Identifier names
//! are quoted with a dedicated quoting routine; values are always bound as
//! parameters. Nothing is interpolated into executable SQL.

use crate::adapter::{ConnectionInfo, DatabaseAdapter, ExecutionResult, PostgresConfig};
use crate::capabilities::Capabilities;
use meld_core::error::MeldError;
use meld_core::state::{EngineKind, EngineState};
use meld_core::verify::VerificationReport;
use sqlx::Row;
use sqlx::postgres::PgPool;
use tracing::info;

pub mod apply;
pub mod extract;
pub mod introspect;
pub mod verify;

/// PostgreSQL database adapter.
#[derive(Debug)]
pub struct PostgresAdapter {
    pool: Option<PgPool>,
}

impl PostgresAdapter {
    pub fn new() -> Self {
        Self { pool: None }
    }

    fn pool(&self) -> Result<&PgPool, MeldError> {
        self.pool
            .as_ref()
            .ok_or_else(|| MeldError::InvalidInput("not connected to a PostgreSQL server".into()))
    }

    /// Build a libpq-style connection string. Passwords are held only in
    /// memory; the DSN is never written to disk or logs.
    fn dsn(config: &PostgresConfig) -> Result<String, MeldError> {
        let uri = config.uri.as_deref().filter(|s| !s.is_empty());
        if let Some(uri) = uri {
            return Ok(uri.to_string());
        }
        Ok(format!(
            "postgresql://{}:{}@{}:{}/{}",
            config.user,
            config.password.as_deref().unwrap_or(""),
            config.host,
            config.port,
            config.database
        ))
    }
}

impl Default for PostgresAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl DatabaseAdapter for PostgresAdapter {
    fn engine_kind(&self) -> EngineKind {
        EngineKind::PostgreSql
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::postgres()
    }

    fn is_connected(&self) -> bool {
        self.pool.is_some()
    }

    async fn test_connection(
        &self,
        config: &crate::adapter::ConnectionConfig,
    ) -> Result<ConnectionInfo, MeldError> {
        match config {
            crate::adapter::ConnectionConfig::PostgreSql(pg) => {
                let dsn = Self::dsn(pg)?;
                let pool = PgPool::connect(&dsn).await.map_err(|e| {
                    MeldError::Io(format!("PostgreSQL connection test failed: {e}"))
                })?;
                let row = sqlx::query("SELECT version()")
                    .fetch_one(&pool)
                    .await
                    .map_err(|e| MeldError::Io(format!("version query failed: {e}")))?;
                let version: String = row.try_get("version").unwrap_or_default();
                pool.close().await;
                Ok(ConnectionInfo {
                    engine_kind: EngineKind::PostgreSql,
                    version,
                    database_name: pg.database.clone(),
                })
            }
            crate::adapter::ConnectionConfig::MongoDb(_) => Err(MeldError::IncompatibleEngine {
                expected: EngineKind::PostgreSql,
                got: EngineKind::MongoDb,
            }),
        }
    }

    async fn connect(
        &mut self,
        config: &crate::adapter::ConnectionConfig,
    ) -> Result<ConnectionInfo, MeldError> {
        match config {
            crate::adapter::ConnectionConfig::PostgreSql(pg) => {
                let dsn = Self::dsn(pg)?;
                let pool = PgPool::connect(&dsn)
                    .await
                    .map_err(|e| MeldError::Io(format!("PostgreSQL connect failed: {e}")))?;
                let version = {
                    let row = sqlx::query("SELECT version()")
                        .fetch_one(&pool)
                        .await
                        .map_err(|e| MeldError::Io(format!("version query failed: {e}")))?;
                    row.try_get::<String, _>("version").unwrap_or_default()
                };
                info!(postgres.version = %version, "connected to postgresql");
                self.pool = Some(pool);
                Ok(ConnectionInfo {
                    engine_kind: EngineKind::PostgreSql,
                    version,
                    database_name: pg.database.clone(),
                })
            }
            crate::adapter::ConnectionConfig::MongoDb(_) => Err(MeldError::IncompatibleEngine {
                expected: EngineKind::PostgreSql,
                got: EngineKind::MongoDb,
            }),
        }
    }

    async fn disconnect(&mut self) -> Result<(), MeldError> {
        if let Some(pool) = self.pool.take() {
            pool.close().await;
        }
        Ok(())
    }

    async fn introspect(&self) -> Result<EngineState, MeldError> {
        introspect::introspect(self.pool()?).await
    }

    async fn verify(&self, target: &EngineState) -> Result<VerificationReport, MeldError> {
        verify::verify(self.pool()?, target).await
    }

    async fn execute_restore_plan(
        &self,
        plan: &meld_core::plan::RestorePlan,
    ) -> Result<ExecutionResult, MeldError> {
        apply::execute(self.pool()?, plan).await
    }

    async fn health_check(&self) -> Result<(), MeldError> {
        let row = sqlx::query("SELECT 1")
            .fetch_one(self.pool()?)
            .await
            .map_err(|e| MeldError::Io(format!("health check failed: {e}")))?;
        let _: i32 = row.try_get(0).unwrap_or(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsn_prefers_uri() {
        let cfg = PostgresConfig {
            name: "x".into(),
            host: "h".into(),
            port: 5432,
            user: "u".into(),
            password: Some("p".into()),
            database: "d".into(),
            uri: Some("postgresql://custom:5432/db".into()),
        };
        assert_eq!(
            PostgresAdapter::dsn(&cfg).unwrap(),
            "postgresql://custom:5432/db"
        );
    }

    #[test]
    fn dsn_builds_from_fields() {
        let cfg = PostgresConfig {
            name: "x".into(),
            host: "localhost".into(),
            port: 5432,
            user: "postgres".into(),
            password: Some("secret".into()),
            database: "mydb".into(),
            uri: None,
        };
        let dsn = PostgresAdapter::dsn(&cfg).unwrap();
        assert!(dsn.starts_with("postgresql://"));
        assert!(dsn.contains("localhost:5432/mydb"));
    }
}

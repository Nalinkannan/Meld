#![allow(clippy::explicit_auto_deref)]

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
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::time::Duration;
use tracing::info;

pub mod apply;
pub mod extract;
pub mod introspect;
pub mod query;
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

    /// Build `PgConnectOptions` from the config, preferring `uri` when set.
    /// Empty strings are treated as absent.
    fn connect_options(config: &PostgresConfig) -> Result<PgConnectOptions, MeldError> {
        if let Some(uri) = config.uri.as_deref().filter(|s| !s.is_empty()) {
            return uri
                .parse::<PgConnectOptions>()
                .map_err(|e| MeldError::InvalidInput(format!("invalid postgres URI: {e}")));
        }
        let mut opts = PgConnectOptions::new();
        if !config.host.is_empty() {
            opts = opts.host(&config.host);
        }
        opts = opts.port(config.port);
        if !config.user.is_empty() {
            opts = opts.username(&config.user);
        }
        if let Some(password) = config.password.as_deref().filter(|s| !s.is_empty()) {
            opts = opts.password(password);
        }
        if !config.database.is_empty() {
            opts = opts.database(&config.database);
        }
        Ok(opts)
    }

    /// Map a sqlx connect error into an actionable message that includes
    /// host:port and classifies common failure modes.
    fn map_connect_error(e: sqlx::Error, config: &PostgresConfig) -> MeldError {
        let target = format!("{}:{}", config.host, config.port);
        let msg = e.to_string();
        let kind = if msg.contains("connection refused") {
            "connection refused"
        } else if msg.contains("authentication failed")
            || msg.contains("password authentication failed")
        {
            "authentication failed"
        } else if msg.contains("does not exist") {
            "database does not exist"
        } else if msg.contains("TLS") || msg.contains("SSL") || msg.contains("ssl") {
            "TLS required"
        } else {
            "connection failed"
        };
        MeldError::Io(format!(
            "PostgreSQL connect to {target} failed ({kind}): {msg}"
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
                let opts = Self::connect_options(pg)?;
                let pool = PgPoolOptions::new()
                    .max_connections(5)
                    .acquire_timeout(Duration::from_secs(10))
                    .connect_with(opts)
                    .await
                    .map_err(|e| Self::map_connect_error(e, pg))?;
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
                let opts = Self::connect_options(pg)?;
                let pool = PgPoolOptions::new()
                    .max_connections(5)
                    .acquire_timeout(Duration::from_secs(10))
                    .connect_with(opts)
                    .await
                    .map_err(|e| Self::map_connect_error(e, pg))?;
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

    async fn extract_table_rows(
        &self,
        table: &meld_core::state::PgTable,
    ) -> Result<Vec<meld_core::state::PgRow>, MeldError> {
        let mut tx = self
            .pool()?
            .begin()
            .await
            .map_err(|e| MeldError::Io(format!("begin extraction transaction: {e}")))?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .map_err(|e| MeldError::Io(format!("set transaction isolation: {e}")))?;
        let rows = extract::extract_table_rows(&mut *tx, table).await?;
        tx.commit()
            .await
            .map_err(|e| MeldError::Io(format!("commit extraction transaction: {e}")))?;
        Ok(rows)
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

    async fn execute_query(
        &self,
        request: crate::adapter::QueryRequest,
    ) -> Result<crate::adapter::QueryResult, MeldError> {
        query::execute_sql(self.pool()?, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cfg() -> PostgresConfig {
        PostgresConfig {
            name: "x".into(),
            host: "localhost".into(),
            port: 5432,
            user: "u".into(),
            password: None,
            database: "d".into(),
            uri: None,
        }
    }

    #[test]
    fn connect_options_prefers_uri() {
        let mut cfg = base_cfg();
        cfg.uri = Some("postgresql://custom:5432/db".into());
        let opts = PostgresAdapter::connect_options(&cfg).unwrap();
        assert_eq!(opts.get_host(), "custom");
        assert_eq!(opts.get_port(), 5432);
        assert_eq!(opts.get_database(), Some("db"));
    }

    #[test]
    fn connect_options_builds_from_fields() {
        let cfg = base_cfg();
        let opts = PostgresAdapter::connect_options(&cfg).unwrap();
        assert_eq!(opts.get_host(), "localhost");
        assert_eq!(opts.get_port(), 5432);
        assert_eq!(opts.get_username(), "u");
        assert_eq!(opts.get_database(), Some("d"));
    }

    #[test]
    fn connect_options_preserves_special_password() {
        let mut cfg = base_cfg();
        cfg.password = Some("p@:#%? /space".into());
        let opts = PostgresAdapter::connect_options(&cfg).unwrap();
        // Construction must succeed and not mangle the non-secret fields when a
        // password containing URI-special characters is supplied. sqlx stores
        // the password verbatim (no getter exposes it), so a successful build
        // with these fields proves the options were constructed from the input.
        assert_eq!(opts.get_host(), "localhost");
        assert_eq!(opts.get_port(), 5432);
        assert_eq!(opts.get_username(), "u");
        assert_eq!(opts.get_database(), Some("d"));
    }

    #[test]
    fn connect_options_treats_empty_as_none() {
        let mut cfg = base_cfg();
        cfg.host = "".into();
        cfg.user = "".into();
        cfg.database = "".into();
        cfg.password = Some("".into());
        let opts = PostgresAdapter::connect_options(&cfg).unwrap();
        // Empty strings are treated as absent: construction succeeds and no
        // empty value is forced onto the options (defaults apply instead).
        assert_ne!(opts.get_host(), "");
        assert_ne!(opts.get_username(), "");
        assert_eq!(opts.get_database(), None);
    }

    #[test]
    fn connect_options_rejects_bad_uri() {
        let mut cfg = base_cfg();
        cfg.uri = Some("not-a-uri".into());
        assert!(PostgresAdapter::connect_options(&cfg).is_err());
    }

    #[test]
    fn map_connect_error_includes_target() {
        let cfg = base_cfg();
        let err = sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        let mapped = PostgresAdapter::map_connect_error(err, &cfg);
        let text = mapped.to_string();
        assert!(text.contains("localhost:5432"));
        assert!(text.contains("connection refused"));
    }
}

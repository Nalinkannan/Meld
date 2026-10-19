//! MongoDB adapter.
//!
//! Uses the official MongoDB Rust driver with BSON types. BSON type
//! distinctions are preserved: a string `_id` and an ObjectId `_id` are
//! never treated as the same identity.

use crate::adapter::{
    ConnectionInfo, DatabaseAdapter, ExecutionResult, MongoConfig, QueryRequest, QueryResult,
};
use crate::capabilities::Capabilities;
use meld_core::error::MeldError;
use meld_core::state::{CanonicalValue, EngineKind, EngineState};
use meld_core::verify::VerificationReport;
use mongodb::options::{ClientOptions, Credential};
use std::time::Duration;
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

    /// Return `Some` only when `s` is a non-empty string; treats `Some("")`
    /// as `None`.
    fn non_empty(s: Option<&str>) -> Option<&str> {
        match s {
            Some(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    /// Build driver `ClientOptions` without concatenating credentials into a
    /// URI. Credentials are applied through `ClientOptions::credential`.
    async fn client_options(config: &MongoConfig) -> Result<ClientOptions, MeldError> {
        let base_uri = match config.uri.as_deref().filter(|s| !s.is_empty()) {
            Some(uri) => uri.to_string(),
            None => {
                let host = Self::non_empty(config.host.as_deref()).unwrap_or("localhost");
                let port = config.port.unwrap_or(27017);
                format!("mongodb://{host}:{port}/")
            }
        };
        let mut opts = ClientOptions::parse(&base_uri)
            .await
            .map_err(|e| MeldError::Io(format!("invalid MongoDB URI: {e}")))?;

        if let Some(username) = Self::non_empty(config.username.as_deref()) {
            let builder = Credential::builder()
                .username(username.to_string())
                .source(Self::non_empty(config.auth_source.as_deref()).map(|s| s.to_string()))
                .password(Self::non_empty(config.password.as_deref()).map(|s| s.to_string()));
            opts.credential = Some(builder.build());
        }

        opts.server_selection_timeout = Some(Duration::from_secs(10));
        opts.connect_timeout = Some(Duration::from_secs(10));
        Ok(opts)
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
                let opts = Self::client_options(mongo).await?;
                let client = mongodb::Client::with_options(opts)
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
                let opts = Self::client_options(mongo).await?;
                let client = mongodb::Client::with_options(opts)
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
        self.database_name = None;
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

    async fn execute_query(
        &self,
        request: crate::adapter::QueryRequest,
    ) -> Result<crate::adapter::QueryResult, MeldError> {
        let QueryRequest::MongoDb {
            collection,
            filter,
            projection,
            sort,
            limit,
        } = request
        else {
            return Err(MeldError::Internal(
                "execute_query called with PostgreSQL request on MongoDB adapter".into(),
            ));
        };

        let start = std::time::Instant::now();
        let db = self.client()?.database(self.db_name()?);
        let coll = db.collection::<mongodb::bson::Document>(&collection);

        let filter_doc: mongodb::bson::Document = match filter {
            Some(s) => serde_json::from_str(&s)
                .map_err(|e| MeldError::InvalidInput(format!("invalid JSON filter: {e}")))?,
            None => mongodb::bson::doc! {},
        };

        let projection_doc: Option<mongodb::bson::Document> =
            match projection {
                Some(p) => Some(serde_json::from_str(&p).map_err(|e| {
                    MeldError::InvalidInput(format!("invalid JSON projection: {e}"))
                })?),
                None => None,
            };
        let sort_doc: Option<mongodb::bson::Document> = match sort {
            Some(s) => Some(
                serde_json::from_str(&s)
                    .map_err(|e| MeldError::InvalidInput(format!("invalid JSON sort: {e}")))?,
            ),
            None => None,
        };

        let options = mongodb::options::FindOptions::builder()
            .projection(projection_doc)
            .sort(sort_doc)
            .limit(limit.map(|l| l as i64))
            .build();

        let mut cursor = coll
            .find(filter_doc, options)
            .await
            .map_err(|e| MeldError::Io(format!("find documents: {e}")))?;

        // Collect all documents, then compute the sorted union of field names
        // as columns (stable, deterministic output).
        let mut docs: Vec<mongodb::bson::Document> = Vec::new();
        let mut columns: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        while cursor
            .advance()
            .await
            .map_err(|e| MeldError::Io(format!("document cursor: {e}")))?
        {
            let doc: mongodb::bson::Document = cursor
                .deserialize_current()
                .map_err(|e| MeldError::Io(format!("deserialize document: {e}")))?;
            for k in doc.keys() {
                columns.insert(k.clone());
            }
            docs.push(doc);
        }

        let columns: Vec<String> = columns.into_iter().collect();
        let mut rows: Vec<Vec<CanonicalValue>> = Vec::with_capacity(docs.len());
        for doc in &docs {
            let mut cells = Vec::with_capacity(columns.len());
            for col in &columns {
                cells.push(match doc.get(col) {
                    Some(v) => crate::mongodb::extract::bson_value_to_canonical(v)?,
                    None => CanonicalValue::Null,
                });
            }
            rows.push(cells);
        }

        let row_count = rows.len() as u64;
        Ok(QueryResult {
            columns,
            rows,
            row_count,
            truncated: false,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cfg() -> MongoConfig {
        MongoConfig {
            name: "x".into(),
            uri: None,
            host: Some("localhost".into()),
            port: Some(27017),
            database: "db".into(),
            username: None,
            password: None,
            auth_source: None,
        }
    }

    fn tcp_address(opts: &ClientOptions) -> (&str, Option<u16>) {
        match &opts.hosts[0] {
            mongodb::options::ServerAddress::Tcp { host, port } => (host.as_str(), *port),
            other => panic!("expected Tcp server address, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn client_options_prefers_uri() {
        let mut cfg = base_cfg();
        cfg.uri = Some("mongodb://custom:27017/db".into());
        let opts = MongoAdapter::client_options(&cfg).await.unwrap();
        let (host, port) = tcp_address(&opts);
        assert_eq!(host, "custom");
        assert_eq!(port, Some(27017));
    }

    #[tokio::test]
    async fn client_options_builds_from_fields() {
        let cfg = base_cfg();
        let opts = MongoAdapter::client_options(&cfg).await.unwrap();
        let (host, port) = tcp_address(&opts);
        assert_eq!(host, "localhost");
        assert_eq!(port, Some(27017));
        assert!(opts.credential.is_none());
        assert_eq!(opts.server_selection_timeout, Some(Duration::from_secs(10)));
        assert_eq!(opts.connect_timeout, Some(Duration::from_secs(10)));
    }

    #[tokio::test]
    async fn client_options_preserves_special_password() {
        let mut cfg = base_cfg();
        cfg.username = Some("user".into());
        cfg.password = Some("p@:#%? /space".into());
        cfg.auth_source = Some("admin".into());
        let opts = MongoAdapter::client_options(&cfg).await.unwrap();
        let cred = opts.credential.expect("credential should be set");
        assert_eq!(cred.username, Some("user".into()));
        assert_eq!(cred.password, Some("p@:#%? /space".into()));
        assert_eq!(cred.source, Some("admin".into()));
    }

    #[tokio::test]
    async fn client_options_treats_empty_as_none() {
        let mut cfg = base_cfg();
        cfg.host = Some("".into());
        cfg.username = Some("".into());
        cfg.password = Some("".into());
        cfg.auth_source = Some("".into());
        let opts = MongoAdapter::client_options(&cfg).await.unwrap();
        let (host, _port) = tcp_address(&opts);
        assert_eq!(host, "localhost");
        assert!(opts.credential.is_none());
    }

    #[tokio::test]
    async fn client_options_rejects_bad_uri() {
        let mut cfg = base_cfg();
        cfg.uri = Some("not-a-uri".into());
        assert!(MongoAdapter::client_options(&cfg).await.is_err());
    }
}

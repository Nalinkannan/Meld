//! Smoke test: a real connection must remain usable *after* `connect()`
//! returns. This reproduces the bug where the pool/client was created on a
//! throwaway runtime and died as soon as that runtime was dropped.
//!
//! Requires a local PostgreSQL (`MELD_TEST_PG_URL`) and MongoDB
//! (`MELD_TEST_MONGO_URI`). Ignored by default.

use meld_adapter::{ConnectionConfig, MongoConfig, PostgresConfig};
use meld_adapter::registry::AdapterRegistry;

fn pg_config() -> ConnectionConfig {
    let url = std::env::var("MELD_TEST_PG_URL").expect("MELD_TEST_PG_URL must be set");
    ConnectionConfig::PostgreSql(PostgresConfig {
        name: "smoke".into(),
        host: "localhost".into(),
        port: 5432,
        user: "meld_user".into(),
        password: None,
        database: "meld_test".into(),
        uri: Some(url),
    })
}

fn mongo_config() -> ConnectionConfig {
    let uri = std::env::var("MELD_TEST_MONGO_URI").expect("MELD_TEST_MONGO_URI must be set");
    ConnectionConfig::MongoDb(MongoConfig {
        name: "smoke".into(),
        uri: Some(uri),
        host: None,
        port: None,
        database: "admin".into(),
        username: None,
        password: None,
        auth_source: None,
    })
}

#[tokio::test]
#[ignore]
async fn postgres_connect_then_health_and_introspect() {
    let config = pg_config();
    let (mut adapter, info) = AdapterRegistry::new().connect(&config).await.unwrap();
    assert!(!info.database_name.is_empty());
    assert!(adapter.is_connected());
    adapter.health_check().await.unwrap();
    let _state = adapter.introspect().await.unwrap();
    adapter.disconnect().await.unwrap();
    assert!(!adapter.is_connected());
}

#[tokio::test]
#[ignore]
async fn mongodb_connect_then_health_and_introspect() {
    let config = mongo_config();
    let (mut adapter, info) = AdapterRegistry::new().connect(&config).await.unwrap();
    assert!(!info.database_name.is_empty());
    assert!(adapter.is_connected());
    adapter.health_check().await.unwrap();
    let _state = adapter.introspect().await.unwrap();
    adapter.disconnect().await.unwrap();
    assert!(!adapter.is_connected());
}

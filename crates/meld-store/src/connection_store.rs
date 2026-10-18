//! Saved connection metadata.
//!
//! Connection profiles are persisted WITHOUT plaintext passwords. The password
//! is never written to the local store; a reconnect workflow is used instead.

use crate::schema::TABLE_CONNECTIONS;
use meld_core::MeldError;
use meld_core::state::EngineKind;
use redb::TableDefinition;
use serde::{Deserialize, Serialize};

pub(crate) static CONNECTIONS: TableDefinition<String, String> =
    TableDefinition::new(TABLE_CONNECTIONS);

/// A saved connection profile. The `password` field is always `None` in
/// persisted profiles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub name: String,
    pub engine: EngineKind,
    /// Human-readable, non-secret connection parameters.
    pub params: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub fn save_profile(db: &redb::Database, profile: &ConnectionProfile) -> Result<(), MeldError> {
    if profile.password_present() {
        return Err(MeldError::Config(
            "refusing to persist a connection password".into(),
        ));
    }
    let json =
        serde_json::to_string(profile).map_err(|e| MeldError::Serialization(format!("{e}")))?;
    let txn = db
        .begin_write()
        .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
    {
        let mut table = txn
            .open_table(CONNECTIONS)
            .map_err(|e| MeldError::Io(format!("open connections table: {e}")))?;
        table
            .insert(profile.name.clone(), json)
            .map_err(|e| MeldError::Io(format!("save connection: {e}")))?;
    }
    txn.commit()
        .map_err(|e| MeldError::Io(format!("commit connection: {e}")))?;
    Ok(())
}

pub fn get_profile(
    db: &redb::Database,
    name: &str,
) -> Result<Option<ConnectionProfile>, MeldError> {
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let profile = {
        let table = txn
            .open_table(CONNECTIONS)
            .map_err(|e| MeldError::Io(format!("open connections table: {e}")))?;
        table
            .get(&name.to_string())
            .map_err(|e| MeldError::Io(format!("get connection {name}: {e}")))?
            .map(|g| serde_json::from_str(&g.value()).expect("stored connection is valid JSON"))
    };
    drop(txn);
    Ok(profile)
}

pub fn list_profiles(db: &redb::Database) -> Result<Vec<ConnectionProfile>, MeldError> {
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let out = {
        let table = txn
            .open_table(CONNECTIONS)
            .map_err(|e| MeldError::Io(format!("open connections table: {e}")))?;
        let mut out = Vec::new();
        for result in table
            .range::<String>(..)
            .map_err(|e| MeldError::Io(format!("iterate connections: {e}")))?
        {
            let (_key, value) =
                result.map_err(|e| MeldError::Io(format!("iterate connections: {e}")))?;
            out.push(
                serde_json::from_str(&value.value()).expect("stored connection is valid JSON"),
            );
        }
        out
    };
    drop(txn);
    Ok(out)
}

pub fn delete_profile(db: &redb::Database, name: &str) -> Result<(), MeldError> {
    let txn = db
        .begin_write()
        .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
    {
        let mut table = txn
            .open_table(CONNECTIONS)
            .map_err(|e| MeldError::Io(format!("open connections table: {e}")))?;
        table
            .remove(&name.to_string())
            .map_err(|e| MeldError::Io(format!("delete connection {name}: {e}")))?;
    }
    txn.commit()
        .map_err(|e| MeldError::Io(format!("commit delete connection: {e}")))?;
    Ok(())
}

impl ConnectionProfile {
    pub fn password_present(&self) -> bool {
        self.params
            .get("password")
            .map(|v| !v.is_null() && !v.as_str().map(|s| s.is_empty()).unwrap_or(true))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn sample_profile() -> ConnectionProfile {
        ConnectionProfile {
            name: "demo".into(),
            engine: EngineKind::PostgreSql,
            params: json!({"host": "localhost", "port": 5432, "user": "meld", "database": "meld"}),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn profile_round_trip_without_password() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let p = sample_profile();
        save_profile(&db, &p).unwrap();
        let got = get_profile(&db, "demo").unwrap().unwrap();
        assert_eq!(got.name, "demo");
        assert_eq!(got.params["host"], "localhost");
        assert!(got.params.get("password").is_none());
        let profiles = list_profiles(&db).unwrap();
        assert_eq!(profiles.len(), 1);
    }

    #[test]
    fn password_is_rejected() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let mut p = sample_profile();
        p.params = json!({"host": "localhost", "password": "secret"});
        let err = save_profile(&db, &p).unwrap_err();
        assert!(matches!(err, MeldError::Config { .. }));
    }
}

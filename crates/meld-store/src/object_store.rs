//! Content-addressed immutable object storage.
//!
//! Objects are keyed by their BLAKE3 content hash. On every read, the stored
//! bytes are re-hashed and compared to the address; corruption is reported as
//! a typed error rather than silently returned.

use crate::schema::TABLE_OBJECTS;
use meld_core::MeldError;
use meld_core::ids::{ObjectHash, hash_object};
use redb::TableDefinition;

pub(crate) static OBJECTS: TableDefinition<String, Vec<u8>> = TableDefinition::new(TABLE_OBJECTS);

/// Store an opaque byte object, returning its content address.
pub fn put_object(db: &redb::Database, bytes: &[u8]) -> Result<ObjectHash, MeldError> {
    let hash = ObjectHash(hash_object(bytes));
    let key = hash.hex();
    let txn = db
        .begin_write()
        .map_err(|e| MeldError::Io(format!("begin write: {e}")))?;
    {
        let mut table = txn
            .open_table(OBJECTS)
            .map_err(|e| MeldError::Io(format!("open objects table: {e}")))?;
        table
            .insert(key, bytes.to_vec())
            .map_err(|e| MeldError::Io(format!("insert object: {e}")))?;
    }
    txn.commit()
        .map_err(|e| MeldError::Io(format!("commit object: {e}")))?;
    Ok(hash)
}

/// Retrieve and integrity-check an object by its content address.
pub fn get_object(db: &redb::Database, hash: &ObjectHash) -> Result<Vec<u8>, MeldError> {
    let key = hash.hex();
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let bytes = {
        let table = match txn.open_table(OBJECTS) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                drop(txn);
                return Err(MeldError::NotFound(format!("object {hash} not found")));
            }
            Err(e) => {
                drop(txn);
                return Err(MeldError::Io(format!("open objects table: {e}")));
            }
        };
        let Some(guard) = table
            .get(&key)
            .map_err(|e| MeldError::Io(format!("get object: {e}")))?
        else {
            drop(txn);
            return Err(MeldError::NotFound(format!("object {hash} not found")));
        };
        guard.value()
    };
    drop(txn);
    // Integrity check: re-hash and compare to the address.
    let actual = ObjectHash(hash_object(&bytes));
    if actual.0 != hash.0 {
        return Err(MeldError::Integrity(format!(
            "object {hash} content hash mismatch: got {actual}"
        )));
    }
    Ok(bytes)
}

/// Return true if an object exists (without verifying its contents).
pub fn contains_object(db: &redb::Database, hash: &ObjectHash) -> Result<bool, MeldError> {
    let key = hash.hex();
    let txn = db
        .begin_read()
        .map_err(|e| MeldError::Io(format!("begin read: {e}")))?;
    let exists = {
        let table = match txn.open_table(OBJECTS) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                drop(txn);
                return Ok(false);
            }
            Err(e) => {
                drop(txn);
                return Err(MeldError::Io(format!("open objects table: {e}")));
            }
        };
        table
            .get(&key)
            .map_err(|e| MeldError::Io(format!("get object: {e}")))?
            .is_some()
    };
    drop(txn);
    Ok(exists)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn put_get_round_trip_and_integrity() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let bytes = b"hello meld object";
        let hash = put_object(&db, bytes).unwrap();
        let got = get_object(&db, &hash).unwrap();
        assert_eq!(&got[..], bytes);
        assert!(contains_object(&db, &hash).unwrap());
    }

    #[test]
    fn missing_object_reported() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let hash = ObjectHash::from_bytes([42u8; 32]);
        let err = get_object(&db, &hash).unwrap_err();
        assert!(matches!(err, MeldError::NotFound { .. }));
    }

    #[test]
    fn corrupted_object_detected() {
        let dir = TempDir::new().unwrap();
        let db = redb::Database::create(dir.path().join("test.edb")).unwrap();
        let bytes = b"original bytes";
        let hash = put_object(&db, bytes).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(OBJECTS).unwrap();
            let mut corrupted = bytes.to_vec();
            corrupted[0] ^= 0xff;
            table.insert(hash.hex(), corrupted).unwrap();
        }
        txn.commit().unwrap();
        let err = get_object(&db, &hash).unwrap_err();
        assert!(matches!(err, MeldError::Integrity { .. }));
    }

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(hash_object(b"same"), hash_object(b"same"));
        assert_ne!(hash_object(b"same"), hash_object(b"different"));
    }
}

//! Stable object identities for schema and data objects.
//!
//! Identities are engine-typed so that a PostgreSQL table and a MongoDB
//! collection can never be confused, and a string `_id` and an ObjectId
//! `_id` are always distinct.

use crate::canonical::Canonicalize;
use crate::state::CanonicalValue;
use serde::{Deserialize, Serialize};

/// A stable identity for a version-controlled object.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectIdentity {
    PostgreSql(PostgresObjectId),
    MongoDb(MongoObjectId),
}

impl ObjectIdentity {
    pub fn engine_kind(&self) -> crate::state::EngineKind {
        match self {
            ObjectIdentity::PostgreSql(_) => crate::state::EngineKind::PostgreSql,
            ObjectIdentity::MongoDb(_) => crate::state::EngineKind::MongoDb,
        }
    }
}

impl std::fmt::Display for ObjectIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjectIdentity::PostgreSql(id) => write!(f, "pg:{id}"),
            ObjectIdentity::MongoDb(id) => write!(f, "mongo:{id}"),
        }
    }
}

/// PostgreSQL object identities, fully qualified by schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PostgresObjectId {
    Schema {
        name: String,
    },
    Table {
        schema: String,
        table: String,
    },
    Column {
        schema: String,
        table: String,
        column: String,
    },
    Index {
        schema: String,
        table: String,
        index: String,
    },
    Constraint {
        schema: String,
        table: String,
        constraint: String,
    },
    View {
        schema: String,
        view: String,
    },
    Sequence {
        schema: String,
        sequence: String,
    },
    Row {
        schema: String,
        table: String,
        identity: Vec<CanonicalValue>,
    },
}

impl std::fmt::Display for PostgresObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PostgresObjectId::Schema { name } => write!(f, "schema.{name}"),
            PostgresObjectId::Table { schema, table } => write!(f, "{schema}.{table}"),
            PostgresObjectId::Column {
                schema,
                table,
                column,
            } => {
                write!(f, "{schema}.{table}.{column}")
            }
            PostgresObjectId::Index {
                schema,
                table,
                index,
            } => {
                write!(f, "{schema}.{table}.idx.{index}")
            }
            PostgresObjectId::Constraint {
                schema,
                table,
                constraint,
            } => {
                write!(f, "{schema}.{table}.constraint.{constraint}")
            }
            PostgresObjectId::View { schema, view } => write!(f, "{schema}.view.{view}"),
            PostgresObjectId::Sequence { schema, sequence } => {
                write!(f, "{schema}.sequence.{sequence}")
            }
            PostgresObjectId::Row {
                schema,
                table,
                identity,
            } => {
                write!(f, "{schema}.{table}.row[{}]", identity.len())
            }
        }
    }
}

/// MongoDB object identities.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MongoObjectId {
    Database {
        name: String,
    },
    Collection {
        name: String,
    },
    Index {
        collection: String,
        index: String,
    },
    Validator {
        collection: String,
    },
    View {
        name: String,
    },
    Document {
        collection: String,
        id: CanonicalValue,
    },
}

impl std::fmt::Display for MongoObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MongoObjectId::Database { name } => write!(f, "db.{name}"),
            MongoObjectId::Collection { name } => write!(f, "coll.{name}"),
            MongoObjectId::Index { collection, index } => {
                write!(f, "{collection}.idx.{index}")
            }
            MongoObjectId::Validator { collection } => {
                write!(f, "{collection}.validator")
            }
            MongoObjectId::View { name } => write!(f, "view.{name}"),
            MongoObjectId::Document { collection, id } => {
                write!(f, "{collection}.doc[{}]", id.canonical_bytes().len())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_and_objectid_document_ids_are_distinct() {
        let text_id = MongoObjectId::Document {
            collection: "c".into(),
            id: CanonicalValue::Text("abc".into()),
        };
        let bytes = hex_decode("507f1f77bcf86cd799439011");
        let oid_id = MongoObjectId::Document {
            collection: "c".into(),
            id: CanonicalValue::ObjectId(bytes),
        };
        assert_ne!(text_id, oid_id);
        assert_ne!(format!("{text_id:?}"), format!("{oid_id:?}"));
    }

    fn hex_decode(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }
}

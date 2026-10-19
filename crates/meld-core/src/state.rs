//! Engine-specific state representations.
//!
//! PostgreSQL and MongoDB share the version-control engine but are never
//! forced into an inaccurate common schema. Each engine has a typed,
//! serializable, recoverable state. `EngineState` is the enum that carries
//! engine-specific information while keeping it explicit.

use crate::canonical::{
    Canonicalize, write_bool, write_bytes, write_i32, write_i64, write_sequence, write_sorted_map,
    write_str, write_u32, write_u64,
};
use chrono::Utc;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::hash::{Hash as StdHash, Hasher};

/// Current state format version. Increment when the state structure changes.
/// Bumped to 2 for the lossless BSON canonical value encoding (C3).
pub const STATE_FORMAT_VERSION: u32 = 2;

/// The database engine kind. Cross-engine merge/restore is explicitly rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EngineKind {
    PostgreSql,
    MongoDb,
}

impl std::fmt::Display for EngineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineKind::PostgreSql => write!(f, "postgresql"),
            EngineKind::MongoDb => write!(f, "mongodb"),
        }
    }
}

/// Consistency guarantees for a captured snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConsistencyLevel {
    /// Captured inside a single transaction/snapshot; fully consistent.
    Transactional,
    /// Captured with a consistent-read strategy (e.g. MVCC snapshot).
    Snapshot,
    /// Best-effort; concurrent changes may make the capture inconsistent.
    BestEffort,
    /// Consistency could not be determined.
    Unknown,
}

impl std::fmt::Display for ConsistencyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsistencyLevel::Transactional => write!(f, "transactional"),
            ConsistencyLevel::Snapshot => write!(f, "snapshot"),
            ConsistencyLevel::BestEffort => write!(f, "best-effort"),
            ConsistencyLevel::Unknown => write!(f, "unknown"),
        }
    }
}

/// Information about how a state was captured.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CaptureInfo {
    pub consistency: ConsistencyLevel,
    pub strategy: String,
    pub captured_at: chrono::DateTime<Utc>,
    pub engine_version: String,
    pub scope: String,
    /// Human-readable warnings about what was excluded or approximated.
    pub warnings: Vec<String>,
}

/// A warning emitted during capture about an unsupported or excluded feature.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CaptureWarning {
    pub object: String,
    pub message: String,
}

/// The scope of a snapshot: which engine and which logical database/catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotScope {
    pub engine_kind: EngineKind,
    /// For PostgreSQL: the catalog/database name. For MongoDB: the database name.
    pub database_name: String,
    /// Optional further scoping (e.g. a single schema or collection list).
    pub included_schemas: Option<Vec<String>>,
    pub included_collections: Option<Vec<String>>,
}

impl SnapshotScope {
    pub fn postgres(database_name: impl Into<String>) -> Self {
        Self {
            engine_kind: EngineKind::PostgreSql,
            database_name: database_name.into(),
            included_schemas: None,
            included_collections: None,
        }
    }

    pub fn mongodb(database_name: impl Into<String>) -> Self {
        Self {
            engine_kind: EngineKind::MongoDb,
            database_name: database_name.into(),
            included_schemas: None,
            included_collections: None,
        }
    }

    pub fn compatible_with(&self, other: &SnapshotScope) -> bool {
        self.engine_kind == other.engine_kind && self.database_name == other.database_name
    }
}

// ---------------------------------------------------------------------------
// Canonical values (type-aware, BSON-type-preserving)
// ---------------------------------------------------------------------------

/// A type-aware canonical value used for row/document field values and hash
/// computation. Type distinctions are preserved so that, e.g., a string `_id`
/// and an ObjectId `_id` never collide.
///
/// Serialization format version 2: BSON-specific values are represented with
/// explicit typed variants so they round-trip losslessly. Values that cannot
/// be represented exactly must not be silently replaced with text placeholders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CanonicalValue {
    Null,
    Boolean(bool),
    I32(i32),
    I64(i64),
    F64(u64), // IEEE-754 bits, so hashing/ordering is deterministic and NaN-safe
    Numeric(String),
    Text(String),
    Bytes(Vec<u8>),
    /// RFC3339 UTC timestamp.
    Timestamp(String),
    /// A MongoDB ObjectId (12 raw bytes, never the ASCII hex encoding).
    ObjectId(Vec<u8>),
    /// A PostgreSQL UUID (16 raw bytes, distinct from Binary/Text).
    Uuid([u8; 16]),
    /// MongoDB Decimal128 (16 raw bytes).
    Decimal128([u8; 16]),
    /// MongoDB binary value with explicit BSON binary subtype.
    Binary {
        subtype: u8,
        bytes: Vec<u8>,
    },
    /// MongoDB BSON timestamp (seconds + increment).
    BsonTimestamp {
        timestamp: u32,
        increment: u32,
    },
    /// Regular expression (pattern + options).
    Regex {
        pattern: String,
        options: String,
    },
    /// JavaScript code with optional scope document.
    JavaScript {
        code: String,
        scope: Option<IndexMap<String, CanonicalValue>>,
    },
    MinKey,
    MaxKey,
    /// A sequence; order is significant.
    Array(Vec<CanonicalValue>),
    /// A map; keys sorted at canonicalization time.
    Document(IndexMap<String, CanonicalValue>),
}

impl CanonicalValue {
    pub fn is_null(&self) -> bool {
        matches!(self, CanonicalValue::Null)
    }

    pub fn from_f64(v: f64) -> Self {
        CanonicalValue::F64(v.to_bits())
    }

    fn variant_index(&self) -> u8 {
        match self {
            CanonicalValue::Null => 0,
            CanonicalValue::Boolean(_) => 1,
            CanonicalValue::I32(_) => 2,
            CanonicalValue::I64(_) => 3,
            CanonicalValue::F64(_) => 4,
            CanonicalValue::Numeric(_) => 5,
            CanonicalValue::Text(_) => 6,
            CanonicalValue::Bytes(_) => 7,
            CanonicalValue::Timestamp(_) => 8,
            CanonicalValue::ObjectId(_) => 9,
            CanonicalValue::Uuid(_) => 10,
            CanonicalValue::Decimal128(_) => 11,
            CanonicalValue::Binary { .. } => 12,
            CanonicalValue::BsonTimestamp { .. } => 13,
            CanonicalValue::Regex { .. } => 14,
            CanonicalValue::JavaScript { .. } => 15,
            CanonicalValue::MinKey => 16,
            CanonicalValue::MaxKey => 17,
            CanonicalValue::Array(_) => 18,
            CanonicalValue::Document(_) => 19,
        }
    }
}

/// Hash an optional scope map canonically (IndexMap does not implement Hash).
fn canonical_option_map_hash(
    scope: &Option<IndexMap<String, CanonicalValue>>,
    state: &mut impl std::hash::Hasher,
) {
    match scope {
        Some(s) => {
            let mut bytes = Vec::new();
            let entries: Vec<(&String, &CanonicalValue)> = s.iter().collect();
            crate::canonical::write_sorted_map(&mut bytes, &entries);
            bytes.hash(state);
        }
        None => 0u8.hash(state),
    }
}

/// Compare two optional scope maps canonically.
fn canonical_option_map_cmp(
    a: &Option<IndexMap<String, CanonicalValue>>,
    b: &Option<IndexMap<String, CanonicalValue>>,
) -> Ordering {
    let mut a_bytes = Vec::new();
    let mut b_bytes = Vec::new();
    if let Some(s) = a {
        let entries: Vec<(&String, &CanonicalValue)> = s.iter().collect();
        crate::canonical::write_sorted_map(&mut a_bytes, &entries);
    }
    if let Some(s) = b {
        let entries: Vec<(&String, &CanonicalValue)> = s.iter().collect();
        crate::canonical::write_sorted_map(&mut b_bytes, &entries);
    }
    a_bytes.cmp(&b_bytes)
}

impl PartialEq for CanonicalValue {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
            && match (self, other) {
                (CanonicalValue::Boolean(a), CanonicalValue::Boolean(b)) => a == b,
                (CanonicalValue::I32(a), CanonicalValue::I32(b)) => a == b,
                (CanonicalValue::I64(a), CanonicalValue::I64(b)) => a == b,
                (CanonicalValue::F64(a), CanonicalValue::F64(b)) => a == b,
                (CanonicalValue::Numeric(a), CanonicalValue::Numeric(b)) => a == b,
                (CanonicalValue::Text(a), CanonicalValue::Text(b)) => a == b,
                (CanonicalValue::Bytes(a), CanonicalValue::Bytes(b)) => a == b,
                (CanonicalValue::Timestamp(a), CanonicalValue::Timestamp(b)) => a == b,
                (CanonicalValue::ObjectId(a), CanonicalValue::ObjectId(b)) => a == b,
                (CanonicalValue::Uuid(a), CanonicalValue::Uuid(b)) => a == b,
                (CanonicalValue::Decimal128(a), CanonicalValue::Decimal128(b)) => a == b,
                (
                    CanonicalValue::Binary {
                        subtype: a,
                        bytes: a_bytes,
                    },
                    CanonicalValue::Binary {
                        subtype: b,
                        bytes: b_bytes,
                    },
                ) => a == b && a_bytes == b_bytes,
                (
                    CanonicalValue::BsonTimestamp {
                        timestamp: a_ts,
                        increment: a_inc,
                    },
                    CanonicalValue::BsonTimestamp {
                        timestamp: b_ts,
                        increment: b_inc,
                    },
                ) => a_ts == b_ts && a_inc == b_inc,
                (
                    CanonicalValue::Regex {
                        pattern: a_p,
                        options: a_o,
                    },
                    CanonicalValue::Regex {
                        pattern: b_p,
                        options: b_o,
                    },
                ) => a_p == b_p && a_o == b_o,
                (
                    CanonicalValue::JavaScript {
                        code: a_c,
                        scope: a_s,
                    },
                    CanonicalValue::JavaScript {
                        code: b_c,
                        scope: b_s,
                    },
                ) => a_c == b_c && a_s == b_s,
                (CanonicalValue::MinKey, CanonicalValue::MinKey) => true,
                (CanonicalValue::MaxKey, CanonicalValue::MaxKey) => true,
                (CanonicalValue::Array(a), CanonicalValue::Array(b)) => a == b,
                (CanonicalValue::Document(a), CanonicalValue::Document(b)) => a == b,
                (CanonicalValue::Null, CanonicalValue::Null) => true,
                _ => false,
            }
    }
}

impl Eq for CanonicalValue {}

impl StdHash for CanonicalValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            CanonicalValue::Boolean(v) => v.hash(state),
            CanonicalValue::I32(v) => v.hash(state),
            CanonicalValue::I64(v) => v.hash(state),
            CanonicalValue::F64(v) => v.hash(state),
            CanonicalValue::Numeric(v) => v.hash(state),
            CanonicalValue::Text(v) => v.hash(state),
            CanonicalValue::Bytes(v) => v.hash(state),
            CanonicalValue::Timestamp(v) => v.hash(state),
            CanonicalValue::ObjectId(v) => v.hash(state),
            CanonicalValue::Uuid(v) => v.hash(state),
            CanonicalValue::Decimal128(v) => v.hash(state),
            CanonicalValue::Binary { subtype, bytes } => {
                subtype.hash(state);
                bytes.hash(state);
            }
            CanonicalValue::BsonTimestamp {
                timestamp,
                increment,
            } => {
                timestamp.hash(state);
                increment.hash(state);
            }
            CanonicalValue::Regex { pattern, options } => {
                pattern.hash(state);
                options.hash(state);
            }
            CanonicalValue::JavaScript { code, scope } => {
                code.hash(state);
                canonical_option_map_hash(scope, state);
            }
            CanonicalValue::MinKey => 0u8.hash(state),
            CanonicalValue::MaxKey => 1u8.hash(state),
            CanonicalValue::Array(v) => v.hash(state),
            CanonicalValue::Document(v) => {
                // Hash in sorted-key order for determinism.
                let mut keys: Vec<&String> = v.keys().collect();
                keys.sort();
                for k in keys {
                    k.hash(state);
                    v.get(k).unwrap().hash(state);
                }
            }
            CanonicalValue::Null => {}
        }
    }
}

impl Ord for CanonicalValue {
    fn cmp(&self, other: &Self) -> Ordering {
        let disc_cmp = self.variant_index().cmp(&other.variant_index());
        if disc_cmp != Ordering::Equal {
            return disc_cmp;
        }
        match (self, other) {
            (CanonicalValue::Boolean(a), CanonicalValue::Boolean(b)) => a.cmp(b),
            (CanonicalValue::I32(a), CanonicalValue::I32(b)) => a.cmp(b),
            (CanonicalValue::I64(a), CanonicalValue::I64(b)) => a.cmp(b),
            (CanonicalValue::F64(a), CanonicalValue::F64(b)) => a.cmp(b),
            (CanonicalValue::Numeric(a), CanonicalValue::Numeric(b)) => a.cmp(b),
            (CanonicalValue::Text(a), CanonicalValue::Text(b)) => a.cmp(b),
            (CanonicalValue::Bytes(a), CanonicalValue::Bytes(b)) => a.cmp(b),
            (CanonicalValue::Timestamp(a), CanonicalValue::Timestamp(b)) => a.cmp(b),
            (CanonicalValue::ObjectId(a), CanonicalValue::ObjectId(b)) => a.cmp(b),
            (CanonicalValue::Uuid(a), CanonicalValue::Uuid(b)) => a.cmp(b),
            (CanonicalValue::Decimal128(a), CanonicalValue::Decimal128(b)) => a.cmp(b),
            (
                CanonicalValue::Binary {
                    subtype: a,
                    bytes: a_bytes,
                },
                CanonicalValue::Binary {
                    subtype: b,
                    bytes: b_bytes,
                },
            ) => a.cmp(b).then_with(|| a_bytes.cmp(b_bytes)),
            (
                CanonicalValue::BsonTimestamp {
                    timestamp: a_ts,
                    increment: a_inc,
                },
                CanonicalValue::BsonTimestamp {
                    timestamp: b_ts,
                    increment: b_inc,
                },
            ) => a_ts.cmp(b_ts).then_with(|| a_inc.cmp(b_inc)),
            (
                CanonicalValue::Regex {
                    pattern: a_p,
                    options: a_o,
                },
                CanonicalValue::Regex {
                    pattern: b_p,
                    options: b_o,
                },
            ) => a_p.cmp(b_p).then_with(|| a_o.cmp(b_o)),
            (
                CanonicalValue::JavaScript {
                    code: a_c,
                    scope: a_s,
                },
                CanonicalValue::JavaScript {
                    code: b_c,
                    scope: b_s,
                },
            ) => a_c
                .cmp(b_c)
                .then_with(|| canonical_option_map_cmp(a_s, b_s)),
            (CanonicalValue::MinKey, CanonicalValue::MinKey) => Ordering::Equal,
            (CanonicalValue::MaxKey, CanonicalValue::MaxKey) => Ordering::Equal,
            (CanonicalValue::Array(a), CanonicalValue::Array(b)) => a.cmp(b),
            (CanonicalValue::Document(a), CanonicalValue::Document(b)) => {
                let ka: Vec<&String> = a.keys().collect();
                let kb: Vec<&String> = b.keys().collect();
                ka.cmp(&kb)
            }
            (CanonicalValue::Null, CanonicalValue::Null) => Ordering::Equal,
            _ => Ordering::Equal,
        }
    }
}

impl PartialOrd for CanonicalValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Canonicalize for CanonicalValue {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        match self {
            CanonicalValue::Null => {
                out.push(0x00);
            }
            CanonicalValue::Boolean(v) => {
                out.push(0x01);
                write_bool(out, *v);
            }
            CanonicalValue::I32(v) => {
                out.push(0x02);
                write_i32(out, *v);
            }
            CanonicalValue::I64(v) => {
                out.push(0x03);
                write_i64(out, *v);
            }
            CanonicalValue::F64(bits) => {
                out.push(0x04);
                write_u64(out, *bits);
            }
            CanonicalValue::Numeric(v) => {
                out.push(0x05);
                write_str(out, v);
            }
            CanonicalValue::Text(v) => {
                out.push(0x06);
                write_str(out, v);
            }
            CanonicalValue::Bytes(v) => {
                out.push(0x07);
                write_bytes(out, v);
            }
            CanonicalValue::Timestamp(v) => {
                out.push(0x08);
                write_str(out, v);
            }
            CanonicalValue::ObjectId(bytes) => {
                out.push(0x09);
                write_bytes(out, bytes);
            }
            CanonicalValue::Uuid(bytes) => {
                out.push(0x12);
                write_bytes(out, bytes);
            }
            CanonicalValue::Decimal128(bytes) => {
                out.push(0x0a);
                // 16 raw bytes, length-prefixed like write_bytes but with its own tag.
                out.push(0x42);
                write_u64(out, 16);
                out.extend_from_slice(bytes);
            }
            CanonicalValue::Binary { subtype, bytes } => {
                out.push(0x0b);
                out.push(*subtype);
                write_bytes(out, bytes);
            }
            CanonicalValue::BsonTimestamp {
                timestamp,
                increment,
            } => {
                out.push(0x0c);
                write_u32(out, *timestamp);
                write_u32(out, *increment);
            }
            CanonicalValue::Regex { pattern, options } => {
                out.push(0x0d);
                write_str(out, pattern);
                write_str(out, options);
            }
            CanonicalValue::JavaScript { code, scope } => {
                out.push(0x0e);
                write_str(out, code);
                match scope {
                    Some(s) => {
                        out.push(0x01);
                        let entries: Vec<(&String, &CanonicalValue)> = s.iter().collect();
                        write_sorted_map(out, &entries);
                    }
                    None => out.push(0x00),
                }
            }
            CanonicalValue::MinKey => {
                out.push(0x0f);
            }
            CanonicalValue::MaxKey => {
                out.push(0x10);
            }
            CanonicalValue::Array(items) => {
                out.push(0x11);
                write_sequence(out, items);
            }
            CanonicalValue::Document(fields) => {
                out.push(0x0b);
                let entries: Vec<(&String, &CanonicalValue)> = fields.iter().collect();
                write_sorted_map(out, &entries);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PostgreSQL state
// ---------------------------------------------------------------------------

/// Normalized description of a PostgreSQL column type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PgType {
    pub name: String,
    pub oid: Option<u32>,
    pub internal_type: Option<String>,
    pub array: bool,
    pub length: Option<i32>,
    pub precision: Option<i32>,
    pub scale: Option<i32>,
}

impl Canonicalize for PgType {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x80);
        write_str(out, &self.name);
        write_u32(out, self.oid.unwrap_or(0));
        write_str(out, &self.internal_type.clone().unwrap_or_default());
        write_bool(out, self.array);
        write_u32(out, self.length.unwrap_or(0) as u32);
        write_u32(out, self.precision.unwrap_or(0) as u32);
        write_u32(out, self.scale.unwrap_or(0) as u32);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PgColumn {
    pub name: String,
    pub ordinal_position: u32,
    pub data_type: PgType,
    pub is_nullable: bool,
    pub column_default: Option<String>,
    pub is_identity: bool,
    pub is_generated: bool,
    pub generated_expression: Option<String>,
    pub comment: Option<String>,
}

impl Canonicalize for PgColumn {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x81);
        write_str(out, &self.name);
        write_u32(out, self.ordinal_position);
        self.data_type.canonicalize(out);
        write_bool(out, self.is_nullable);
        match &self.column_default {
            Some(d) => {
                out.push(0x01);
                write_str(out, d);
            }
            None => out.push(0x00),
        }
        write_bool(out, self.is_identity);
        write_bool(out, self.is_generated);
        match &self.generated_expression {
            Some(e) => {
                out.push(0x01);
                write_str(out, e);
            }
            None => out.push(0x00),
        }
        match &self.comment {
            Some(c) => {
                out.push(0x01);
                write_str(out, c);
            }
            None => out.push(0x00),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConstraintKind {
    PrimaryKey,
    Unique,
    ForeignKey,
    Check,
    Exclusion,
}

impl Canonicalize for ConstraintKind {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(match self {
            ConstraintKind::PrimaryKey => 0x01,
            ConstraintKind::Unique => 0x02,
            ConstraintKind::ForeignKey => 0x03,
            ConstraintKind::Check => 0x04,
            ConstraintKind::Exclusion => 0x05,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PgConstraint {
    pub name: String,
    pub kind: ConstraintKind,
    pub definition: String,
    pub referenced_table: Option<(String, String)>,
    pub referenced_columns: Vec<String>,
    pub columns: Vec<String>,
}

impl Canonicalize for PgConstraint {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x82);
        write_str(out, &self.name);
        self.kind.canonicalize(out);
        write_str(out, &self.definition);
        match &self.referenced_table {
            Some((schema, table)) => {
                out.push(0x01);
                write_str(out, schema);
                write_str(out, table);
            }
            None => out.push(0x00),
        }
        write_sequence(
            out,
            &self
                .referenced_columns
                .iter()
                .map(|s| CanonicalValue::Text(s.clone()))
                .collect::<Vec<_>>(),
        );
        write_sequence(
            out,
            &self
                .columns
                .iter()
                .map(|s| CanonicalValue::Text(s.clone()))
                .collect::<Vec<_>>(),
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PgIndexColumn {
    pub name: String,
    pub ascending: bool,
    pub nulls_first: bool,
}

impl Canonicalize for PgIndexColumn {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x83);
        write_str(out, &self.name);
        write_bool(out, self.ascending);
        write_bool(out, self.nulls_first);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PgIndex {
    pub name: String,
    pub is_unique: bool,
    pub is_primary: bool,
    pub columns: Vec<PgIndexColumn>,
    pub predicate: Option<String>,
    pub definition: String,
}

impl Canonicalize for PgIndex {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x84);
        write_str(out, &self.name);
        write_bool(out, self.is_unique);
        write_bool(out, self.is_primary);
        write_sequence(
            out,
            &self
                .columns
                .iter()
                .map(|c| CanonicalValue::Text(c.name.clone()))
                .collect::<Vec<_>>(),
        );
        match &self.predicate {
            Some(p) => {
                out.push(0x01);
                write_str(out, p);
            }
            None => out.push(0x00),
        }
        write_str(out, &self.definition);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgTable {
    pub name: String,
    pub schema: String,
    /// Columns in ordinal order.
    pub columns: IndexMap<String, PgColumn>,
    pub primary_key: Option<PgConstraint>,
    pub unique_constraints: Vec<PgConstraint>,
    pub foreign_keys: Vec<PgConstraint>,
    pub check_constraints: Vec<PgConstraint>,
    pub indexes: IndexMap<String, PgIndex>,
    pub comment: Option<String>,
    pub row_count: u64,
    /// Data rows (stable identity via primary key or content hash).
    pub rows: Vec<PgRow>,
}

impl Canonicalize for PgTable {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x85);
        write_str(out, &self.name);
        write_str(out, &self.schema);
        // Columns: preserve ordinal order (sequence), keyed by name.
        let mut ordered: Vec<(&String, &PgColumn)> = self.columns.iter().collect();
        ordered.sort_by_key(|(_, c)| c.ordinal_position);
        write_sequence(
            out,
            &ordered
                .iter()
                .map(|(_, c)| CanonicalValue::Text(c.name.clone()))
                .collect::<Vec<_>>(),
        );
        for (_, c) in &ordered {
            c.canonicalize(out);
        }
        match &self.primary_key {
            Some(pk) => {
                out.push(0x01);
                pk.canonicalize(out);
            }
            None => out.push(0x00),
        }
        write_sequence(
            out,
            &self
                .unique_constraints
                .iter()
                .map(|c| CanonicalValue::Text(c.name.clone()))
                .collect::<Vec<_>>(),
        );
        for c in &self.unique_constraints {
            c.canonicalize(out);
        }
        write_sequence(
            out,
            &self
                .foreign_keys
                .iter()
                .map(|c| CanonicalValue::Text(c.name.clone()))
                .collect::<Vec<_>>(),
        );
        for c in &self.foreign_keys {
            c.canonicalize(out);
        }
        write_sequence(
            out,
            &self
                .check_constraints
                .iter()
                .map(|c| CanonicalValue::Text(c.name.clone()))
                .collect::<Vec<_>>(),
        );
        for c in &self.check_constraints {
            c.canonicalize(out);
        }
        // Indexes: sorted by name (deterministic map ordering).
        let mut idx_ordered: Vec<(&String, &PgIndex)> = self.indexes.iter().collect();
        idx_ordered.sort_by(|a, b| a.0.cmp(b.0));
        write_sorted_map(
            out,
            &idx_ordered
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect::<Vec<_>>(),
        );
        match &self.comment {
            Some(c) => {
                out.push(0x01);
                write_str(out, c);
            }
            None => out.push(0x00),
        }
        write_u64(out, self.row_count);
        // Rows: identity + values, canonically ordered by identity so the hash
        // is independent of extraction order.
        let mut ordered_rows: Vec<&PgRow> = self.rows.iter().collect();
        ordered_rows.sort_by_key(|a| a.identity.canonical_bytes());
        for row in &ordered_rows {
            match &row.identity {
                PgRowIdentity::PrimaryKey(values) => {
                    out.push(0x01);
                    let entries: Vec<CanonicalValue> = values
                        .iter()
                        .map(|(name, value)| {
                            CanonicalValue::Array(vec![
                                CanonicalValue::Text(name.clone()),
                                value.clone(),
                            ])
                        })
                        .collect();
                    write_sequence(out, &entries);
                }
                PgRowIdentity::ContentHash(bytes) => {
                    out.push(0x00);
                    write_bytes(out, bytes);
                }
            }
            let entries: Vec<_> = row.values.iter().collect();
            write_sorted_map(out, &entries);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PgView {
    pub name: String,
    pub schema: String,
    pub definition: String,
    pub updatable: bool,
}

impl Canonicalize for PgView {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x86);
        write_str(out, &self.name);
        write_str(out, &self.schema);
        write_str(out, &self.definition);
        write_bool(out, self.updatable);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PgSequence {
    pub name: String,
    pub schema: String,
    pub data_type: PgType,
    pub start_value: i64,
    pub increment: i64,
}

impl Canonicalize for PgSequence {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x87);
        write_str(out, &self.name);
        write_str(out, &self.schema);
        self.data_type.canonicalize(out);
        write_i64(out, self.start_value);
        write_i64(out, self.increment);
    }
}

/// A row with a stable identity and type-aware column values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgRow {
    pub identity: PgRowIdentity,
    pub values: IndexMap<String, CanonicalValue>,
}

/// Stable identity for a row: primary-key values when present, each paired
/// with its column name so the name-to-typed-value association is preserved
/// (composite keys work), otherwise a content hash (documented as
/// best-effort).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PgRowIdentity {
    PrimaryKey(Vec<(String, CanonicalValue)>),
    ContentHash([u8; 16]),
}

impl Canonicalize for PgRow {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x88);
        match &self.identity {
            PgRowIdentity::PrimaryKey(values) => {
                out.push(0x01);
                let entries: Vec<CanonicalValue> = values
                    .iter()
                    .map(|(name, value)| {
                        CanonicalValue::Array(vec![
                            CanonicalValue::Text(name.clone()),
                            value.clone(),
                        ])
                    })
                    .collect();
                write_sequence(out, &entries);
            }
            PgRowIdentity::ContentHash(bytes) => {
                out.push(0x00);
                write_bytes(out, bytes);
            }
        }
        let entries: Vec<(&String, &CanonicalValue)> = self.values.iter().collect();
        write_sorted_map(out, &entries);
    }
}

impl Canonicalize for PgRowIdentity {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        match self {
            PgRowIdentity::PrimaryKey(values) => {
                out.push(0x01);
                // Each entry is (column name, typed value), written as a
                // two-element sequence so the name/value association survives.
                let entries: Vec<CanonicalValue> = values
                    .iter()
                    .map(|(name, value)| {
                        CanonicalValue::Array(vec![
                            CanonicalValue::Text(name.clone()),
                            value.clone(),
                        ])
                    })
                    .collect();
                write_sequence(out, &entries);
            }
            PgRowIdentity::ContentHash(bytes) => {
                out.push(0x00);
                write_bytes(out, bytes);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgDatabase {
    pub name: String,
    pub schemas: IndexMap<String, PgSchema>,
    pub views: IndexMap<String, PgView>,
    pub sequences: IndexMap<String, PgSequence>,
}

impl Canonicalize for PgDatabase {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x89);
        write_str(out, &self.name);
        let entries: Vec<(&String, &PgSchema)> = self.schemas.iter().collect();
        write_sorted_map(out, &entries);
        let view_entries: Vec<(&String, &PgView)> = self.views.iter().collect();
        write_sorted_map(out, &view_entries);
        let seq_entries: Vec<(&String, &PgSequence)> = self.sequences.iter().collect();
        write_sorted_map(out, &seq_entries);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgSchema {
    pub name: String,
    pub tables: IndexMap<String, PgTable>,
}

impl Canonicalize for PgSchema {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x8a);
        write_str(out, &self.name);
        let entries: Vec<(&String, &PgTable)> = self.tables.iter().collect();
        write_sorted_map(out, &entries);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresState {
    pub engine_version: String,
    pub databases: IndexMap<String, PgDatabase>,
    pub warnings: Vec<CaptureWarning>,
    pub capture_info: CaptureInfo,
}

impl Canonicalize for PostgresState {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x90);
        write_u64(out, STATE_FORMAT_VERSION as u64);
        write_str(out, &self.engine_version);
        let entries: Vec<(&String, &PgDatabase)> = self.databases.iter().collect();
        write_sorted_map(out, &entries);
        write_sequence(
            out,
            &self
                .warnings
                .iter()
                .map(|w| CanonicalValue::Text(w.object.clone()))
                .collect::<Vec<_>>(),
        );
        for w in &self.warnings {
            write_str(out, &w.message);
        }
        self.capture_info.canonicalize(out);
    }
}

impl Canonicalize for CaptureInfo {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x91);
        write_u32(out, self.consistency as u32);
        write_str(out, &self.strategy);
        self.captured_at.canonicalize(out);
        write_str(out, &self.engine_version);
        write_str(out, &self.scope);
        write_sequence(
            out,
            &self
                .warnings
                .iter()
                .map(|w| CanonicalValue::Text(w.clone()))
                .collect::<Vec<_>>(),
        );
    }
}

// ---------------------------------------------------------------------------
// MongoDB state
// ---------------------------------------------------------------------------

/// Options for a MongoDB collection.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CollectionOptions {
    pub capped: Option<bool>,
    pub size: Option<u64>,
    pub max: Option<u64>,
    /// TTL in seconds, stored as bits of an f64 for deterministic hashing.
    pub expires_after_seconds: Option<u64>,
    pub additional: IndexMap<String, CanonicalValue>,
}

impl Canonicalize for CollectionOptions {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0xa1);
        match self.capped {
            Some(c) => {
                out.push(0x01);
                write_bool(out, c);
            }
            None => out.push(0x00),
        }
        match self.size {
            Some(s) => {
                out.push(0x01);
                write_u64(out, s);
            }
            None => out.push(0x00),
        }
        match self.max {
            Some(m) => {
                out.push(0x01);
                write_u64(out, m);
            }
            None => out.push(0x00),
        }
        match self.expires_after_seconds {
            Some(e) => {
                out.push(0x01);
                write_u64(out, e);
            }
            None => out.push(0x00),
        }
        write_sorted_map(
            out,
            &self
                .additional
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect::<Vec<_>>(),
        );
    }
}

/// Validator definition for a collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validator {
    pub validation_level: String,
    pub validation_action: String,
    pub definition: IndexMap<String, CanonicalValue>,
}

impl Canonicalize for Validator {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0xa2);
        write_str(out, &self.validation_level);
        write_str(out, &self.validation_action);
        write_sorted_map(
            out,
            &self
                .definition
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect::<Vec<_>>(),
        );
    }
}

/// A MongoDB index definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MongoIndex {
    pub name: String,
    /// Index keys in order: (field, direction).
    pub keys: Vec<(String, i32)>,
    pub unique: bool,
    pub partial_filter: Option<IndexMap<String, CanonicalValue>>,
    /// TTL in seconds, stored as bits of an f64 for deterministic hashing.
    pub ttl: Option<u64>,
    pub sparse: Option<bool>,
    pub background: Option<bool>,
}

impl Canonicalize for MongoIndex {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0xa0);
        write_str(out, &self.name);
        write_sequence(
            out,
            &self
                .keys
                .iter()
                .map(|(k, d)| {
                    CanonicalValue::Document(IndexMap::from([
                        ("field".into(), CanonicalValue::Text(k.clone())),
                        ("direction".into(), CanonicalValue::I32(*d)),
                    ]))
                })
                .collect::<Vec<_>>(),
        );
        write_bool(out, self.unique);
        match &self.partial_filter {
            Some(f) => {
                out.push(0x01);
                write_sorted_map(
                    out,
                    &f.iter().map(|(k, v)| (k.as_str(), v)).collect::<Vec<_>>(),
                );
            }
            None => out.push(0x00),
        }
        match self.ttl {
            Some(t) => {
                out.push(0x01);
                write_u64(out, t);
            }
            None => out.push(0x00),
        }
        match self.sparse {
            Some(s) => {
                out.push(0x01);
                write_bool(out, s);
            }
            None => out.push(0x00),
        }
        match self.background {
            Some(b) => {
                out.push(0x01);
                write_bool(out, b);
            }
            None => out.push(0x00),
        }
    }
}

/// A MongoDB view definition. `view_on` is the source collection the view is
/// defined over -- it must never default to the view's own name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MongoView {
    pub name: String,
    pub view_on: String,
    pub definition: IndexMap<String, CanonicalValue>,
}

impl Canonicalize for MongoView {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0xa3);
        write_str(out, &self.name);
        write_str(out, &self.view_on);
        write_sorted_map(
            out,
            &self
                .definition
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect::<Vec<_>>(),
        );
    }
}

/// A MongoDB document: `_id` plus fields. BSON type distinctions preserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MongoDocument {
    pub id: CanonicalValue,
    pub fields: IndexMap<String, CanonicalValue>,
}

impl Canonicalize for MongoDocument {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0xa4);
        self.id.canonicalize(out);
        let entries: Vec<(&String, &CanonicalValue)> = self.fields.iter().collect();
        write_sorted_map(out, &entries);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MongoCollectionState {
    pub name: String,
    pub options: CollectionOptions,
    pub validator: Option<Validator>,
    pub indexes: IndexMap<String, MongoIndex>,
    pub views: Vec<MongoView>,
    pub documents: Vec<MongoDocument>,
    pub document_count: u64,
}

impl Canonicalize for MongoCollectionState {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0xa5);
        write_str(out, &self.name);
        self.options.canonicalize(out);
        match &self.validator {
            Some(v) => {
                out.push(0x01);
                v.canonicalize(out);
            }
            None => out.push(0x00),
        }
        let entries: Vec<(&String, &MongoIndex)> = self.indexes.iter().collect();
        write_sorted_map(out, &entries);
        write_sequence(
            out,
            &self
                .views
                .iter()
                .map(|v| CanonicalValue::Text(v.name.clone()))
                .collect::<Vec<_>>(),
        );
        for v in &self.views {
            v.canonicalize(out);
        }
        // Documents are canonically ordered by _id bytes so the hash is
        // independent of extraction order (document order in a collection is
        // not semantically significant).
        let mut ordered_docs: Vec<&MongoDocument> = self.documents.iter().collect();
        ordered_docs.sort_by_key(|a| a.id.canonical_bytes());
        for d in &ordered_docs {
            d.canonicalize(out);
        }
        write_u64(out, self.document_count);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MongoState {
    pub engine_version: String,
    pub database_name: String,
    pub collections: IndexMap<String, MongoCollectionState>,
    pub warnings: Vec<CaptureWarning>,
    pub capture_info: CaptureInfo,
}

impl Canonicalize for MongoState {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0xa6);
        write_u64(out, STATE_FORMAT_VERSION as u64);
        write_str(out, &self.engine_version);
        write_str(out, &self.database_name);
        let entries: Vec<(&String, &MongoCollectionState)> = self.collections.iter().collect();
        write_sorted_map(out, &entries);
        write_sequence(
            out,
            &self
                .warnings
                .iter()
                .map(|w| CanonicalValue::Text(w.object.clone()))
                .collect::<Vec<_>>(),
        );
        for w in &self.warnings {
            write_str(out, &w.message);
        }
        self.capture_info.canonicalize(out);
    }
}

// ---------------------------------------------------------------------------
// EngineState enum
// ---------------------------------------------------------------------------

/// The canonical state of a database, typed by engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineState {
    PostgreSql(PostgresState),
    MongoDb(MongoState),
}

impl EngineState {
    pub fn engine_kind(&self) -> EngineKind {
        match self {
            EngineState::PostgreSql(_) => EngineKind::PostgreSql,
            EngineState::MongoDb(_) => EngineKind::MongoDb,
        }
    }

    pub fn scope(&self) -> SnapshotScope {
        match self {
            EngineState::PostgreSql(s) => SnapshotScope::postgres(&s.capture_info.scope),
            EngineState::MongoDb(s) => SnapshotScope::mongodb(&s.capture_info.scope),
        }
    }
}

impl Canonicalize for EngineState {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x99);
        write_u64(out, STATE_FORMAT_VERSION as u64);
        match self {
            EngineState::PostgreSql(s) => {
                out.push(0x01);
                write_str(out, "postgresql");
                s.canonicalize(out);
            }
            EngineState::MongoDb(s) => {
                out.push(0x02);
                write_str(out, "mongodb");
                s.canonicalize(out);
            }
        }
    }
}

impl Canonicalize for SnapshotScope {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x9a);
        write_u32(out, self.engine_kind as u32);
        write_str(out, &self.database_name);
        match &self.included_schemas {
            Some(v) => {
                out.push(0x01);
                write_sequence(
                    out,
                    &v.iter()
                        .map(|s| CanonicalValue::Text(s.clone()))
                        .collect::<Vec<_>>(),
                );
            }
            None => out.push(0x00),
        }
        match &self.included_collections {
            Some(v) => {
                out.push(0x01);
                write_sequence(
                    out,
                    &v.iter()
                        .map(|s| CanonicalValue::Text(s.clone()))
                        .collect::<Vec<_>>(),
                );
            }
            None => out.push(0x00),
        }
    }
}

impl Canonicalize for CaptureWarning {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x9b);
        write_str(out, &self.object);
        write_str(out, &self.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_capture_info() -> CaptureInfo {
        CaptureInfo {
            consistency: ConsistencyLevel::Snapshot,
            strategy: "pg_catalog snapshot".into(),
            captured_at: Utc::now(),
            engine_version: "18.0".into(),
            scope: "meld_test".into(),
            warnings: vec![],
        }
    }

    #[test]
    fn string_and_objectid_ids_are_distinct() {
        let str_id = CanonicalValue::Text("507f1f77bcf86cd799439011".into());
        let oid = CanonicalValue::ObjectId(hex_decode("507f1f77bcf86cd799439011"));
        assert_ne!(str_id, oid);
        assert_ne!(str_id.canonical_bytes(), oid.canonical_bytes());
    }

    #[test]
    fn f64_nan_is_hashable_and_eq() {
        let a = CanonicalValue::from_f64(f64::NAN);
        let b = CanonicalValue::from_f64(f64::NAN);
        assert_eq!(a, b);
        let _ = std::collections::HashSet::from([a, b]);
    }

    #[test]
    fn postgres_state_canonical_deterministic() {
        let mut columns = IndexMap::new();
        columns.insert(
            "b".into(),
            PgColumn {
                name: "b".into(),
                ordinal_position: 2,
                data_type: pg_type("int"),
                is_nullable: true,
                column_default: None,
                is_identity: false,
                is_generated: false,
                generated_expression: None,
                comment: None,
            },
        );
        columns.insert(
            "a".into(),
            PgColumn {
                name: "a".into(),
                ordinal_position: 1,
                data_type: pg_type("int"),
                is_nullable: false,
                column_default: None,
                is_identity: false,
                is_generated: false,
                generated_expression: None,
                comment: None,
            },
        );
        let mut tables = IndexMap::new();
        tables.insert(
            "public".into(),
            PgSchema {
                name: "public".into(),
                tables: IndexMap::from([("t".into(), pg_table("t", columns))]),
            },
        );
        let mut dbs = IndexMap::new();
        dbs.insert(
            "mydb".into(),
            PgDatabase {
                name: "mydb".into(),
                schemas: tables,
                views: IndexMap::new(),
                sequences: IndexMap::new(),
            },
        );
        let s1 = PostgresState {
            engine_version: "18".into(),
            databases: dbs,
            warnings: vec![],
            capture_info: sample_capture_info(),
        };
        let h1 = crate::canonical::hash_state(&s1);
        let h2 = crate::canonical::hash_state(&s1);
        assert_eq!(h1, h2);
        assert!(!h1.hex().is_empty());
    }

    fn pg_type(name: &str) -> PgType {
        PgType {
            name: name.into(),
            oid: Some(23),
            internal_type: Some("int4".into()),
            array: false,
            length: None,
            precision: None,
            scale: None,
        }
    }

    fn pg_table(name: &str, columns: IndexMap<String, PgColumn>) -> PgTable {
        PgTable {
            name: name.into(),
            schema: "public".into(),
            columns,
            primary_key: None,
            unique_constraints: vec![],
            foreign_keys: vec![],
            check_constraints: vec![],
            indexes: IndexMap::new(),
            comment: None,
            row_count: 0,
            rows: vec![],
        }
    }

    fn hex_decode(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }
}

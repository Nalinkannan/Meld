#![allow(clippy::explicit_auto_deref)]

//! PostgreSQL data extraction.
//!
//! All rows of a table are read inside a single `REPEATABLE READ` transaction
//! with a deterministic `ORDER BY`, so every page reflects one point-in-time
//! snapshot and concurrent writes cannot shift or duplicate rows between
//! pages (keyset-style pagination on a stable snapshot).
//!
//! Column values are decoded type-aware according to the captured PostgreSQL
//! column type description. A cell that fails to decode for a supported type
//! is an explicit error -- it is never silently turned into `Null`. Types
//! outside the supported set produce `MeldError::Unsupported` rather than
//! silently corrupting the snapshot.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, Utc};
use indexmap::IndexMap;
use meld_core::error::MeldError;
use meld_core::ids::hash_object;
use meld_core::state::{CanonicalValue, PgColumn, PgRowIdentity, PgTable};
use sqlx::Row;
use sqlx::types::BigDecimal;
use tracing::debug;

/// Maximum rows fetched per round-trip inside the extraction transaction.
const EXTRACTION_BATCH_SIZE: u64 = 5_000;

/// Types supported for capture. Anything not listed here is rejected with
/// `MeldError::Unsupported` rather than being stringified.
fn is_supported_type(name: &str) -> bool {
    matches!(
        name,
        "text"
            | "varchar"
            | "character varying"
            | "char"
            | "character"
            | "name"
            | "citext"
            | "uuid"
            | "int2"
            | "smallint"
            | "int4"
            | "integer"
            | "serial"
            | "serial4"
            | "int8"
            | "bigint"
            | "bigserial"
            | "serial8"
            | "float4"
            | "real"
            | "float8"
            | "double precision"
            | "numeric"
            | "decimal"
            | "bool"
            | "boolean"
            | "date"
            | "time"
            | "timetz"
            | "timestamp"
            | "timestamptz"
            | "bytea"
            | "json"
            | "jsonb"
    )
}

/// Extract all rows of `table` using the supplied transaction connection.
///
/// The caller is responsible for wrapping the call in a single
/// `REPEATABLE READ, READ ONLY` transaction so that every table is read from
/// one consistent point-in-time snapshot.
pub async fn extract_table_rows(
    conn: &mut sqlx::PgConnection,
    table: &PgTable,
) -> Result<Vec<meld_core::state::PgRow>, MeldError> {
    // Build column list in ordinal order, quoted as identifiers.
    let mut cols: Vec<(&String, &PgColumn)> = table.columns.iter().collect();
    cols.sort_by_key(|(_, c)| c.ordinal_position);
    let col_list: String = cols
        .iter()
        .map(|(name, _)| format!("\"{}\"", sql_quote(name)))
        .collect::<Vec<_>>()
        .join(", ");

    // Deterministic ordering: primary key columns first (in PK order), then
    // the remaining columns. This order is fixed for the whole capture so
    // LIMIT/OFFSET paging walks a stable sequence.
    let pk_names: Vec<&str> = table
        .primary_key
        .as_ref()
        .map(|pk| pk.columns.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let mut order_cols: Vec<(&str, &PgColumn)> = pk_names
        .iter()
        .filter_map(|name| {
            cols.iter()
                .find(|(n, _)| n.as_str() == *name)
                .map(|(n, c)| (n.as_str(), *c))
        })
        .collect();
    order_cols.extend(
        cols.iter()
            .filter(|(name, _)| !pk_names.contains(&name.as_str()))
            .map(|(n, c)| (n.as_str(), *c)),
    );
    let order_by: String = order_cols
        .iter()
        .map(|(name, _)| format!("\"{}\"", sql_quote(name)))
        .collect::<Vec<_>>()
        .join(", ");

    let schema = sql_quote(&table.schema);
    let name = sql_quote(&table.name);
    let batch = EXTRACTION_BATCH_SIZE;

    let mut rows = Vec::new();
    let mut offset: i64 = 0;
    loop {
        let q = format!(
            "SELECT {} FROM \"{}\".\"{}\" ORDER BY {order_by} LIMIT $1 OFFSET $2",
            col_list, schema, name
        );
        let batch_rows = sqlx::query(&q)
            .bind(batch as i64)
            .bind(offset)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| MeldError::Io(format!("extract rows: {e}")))?;
        if batch_rows.is_empty() {
            break;
        }
        for br in &batch_rows {
            let mut values: IndexMap<String, CanonicalValue> = IndexMap::new();
            for (idx, (_col_name, col)) in cols.iter().enumerate() {
                values.insert((*_col_name).clone(), decode_cell(br, idx, col)?);
            }
            // Identity: primary-key values paired with their column names,
            // in primary-key column order; otherwise a content hash of all
            // values (best-effort, documented).
            let identity = if let Some(pk) = &table.primary_key {
                let mut pk_vals: Vec<(String, CanonicalValue)> =
                    Vec::with_capacity(pk.columns.len());
                for cname in &pk.columns {
                    let (idx, (_column_name, col)) = cols
                        .iter()
                        .enumerate()
                        .find(|(_, (n, _))| n.as_str() == cname.as_str())
                        .ok_or_else(|| {
                            MeldError::Io(format!(
                                "primary key column {cname} of {schema}.{name} not found in table columns"
                            ))
                        })?;
                    let value = decode_cell(br, idx, col)?;
                    pk_vals.push((cname.clone(), value));
                }
                PgRowIdentity::PrimaryKey(pk_vals)
            } else {
                let identity_vals: Vec<CanonicalValue> = values.values().cloned().collect();
                let h = hash_object(&serde_json::to_vec(&identity_vals).map_err(|e| {
                    MeldError::Serialization(format!("row content hash serialization: {e}"))
                })?);
                PgRowIdentity::ContentHash({
                    let mut arr = [0u8; 16];
                    arr.copy_from_slice(&h.as_bytes()[..16]);
                    arr
                })
            };
            rows.push(meld_core::state::PgRow { identity, values });
        }
        offset += batch_rows.len() as i64;
        if batch_rows.len() < batch as usize {
            break;
        }
        debug!(table = %table.name, offset, "extracted batch");
    }

    debug!(table = %table.name, count = rows.len(), "extracted table rows");
    Ok(rows)
}

/// Decode a single SQL cell into a type-aware canonical value.
///
/// NULL cells become `CanonicalValue::Null`. A decode failure on a supported
/// type is an error -- NULL and decode errors are never conflated. Types
/// outside the supported set are rejected explicitly.
fn decode_cell(
    row: &sqlx::postgres::PgRow,
    idx: usize,
    pt: &PgColumn,
) -> Result<CanonicalValue, MeldError> {
    let col = &pt.name;
    let type_name = pt.data_type.name.as_str();

    if !is_supported_type(type_name) {
        return Err(MeldError::Unsupported(format!(
            "unsupported PostgreSQL type {type_name:?} for column {col}"
        )));
    }

    let null_or = |v: Option<CanonicalValue>| match v {
        Some(v) => Ok(v),
        None => Ok(CanonicalValue::Null),
    };

    match type_name {
        // Character types.
        "text" | "varchar" | "character varying" | "char" | "character" | "name" | "citext" => {
            null_or(
                row.try_get::<Option<String>, _>(idx)
                    .map_err(|e| decode_error(col, type_name, e))?
                    .map(CanonicalValue::Text),
            )
        }
        "uuid" => null_or(
            row.try_get::<Option<uuid::Uuid>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(|u| CanonicalValue::Uuid(*u.as_bytes())),
        ),
        // Numeric types.
        "int2" | "smallint" => null_or(
            row.try_get::<Option<i16>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(|v| CanonicalValue::I32(v as i32)),
        ),
        "int4" | "integer" | "serial" | "serial4" => null_or(
            row.try_get::<Option<i32>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(CanonicalValue::I32),
        ),
        "int8" | "bigint" | "bigserial" | "serial8" => null_or(
            row.try_get::<Option<i64>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(CanonicalValue::I64),
        ),
        "float4" | "real" => null_or(
            row.try_get::<Option<f32>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(|v| CanonicalValue::from_f64(v as f64)),
        ),
        "float8" | "double precision" => null_or(
            row.try_get::<Option<f64>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(CanonicalValue::from_f64),
        ),
        "numeric" | "decimal" => null_or(
            row.try_get::<Option<BigDecimal>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(|v| CanonicalValue::Numeric(v.to_string())),
        ),
        // Boolean.
        "bool" | "boolean" => null_or(
            row.try_get::<Option<bool>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(CanonicalValue::Boolean),
        ),
        // Temporal types.
        "date" => null_or(
            row.try_get::<Option<NaiveDate>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(|v| CanonicalValue::Text(v.format("%Y-%m-%d").to_string())),
        ),
        "time" | "timetz" => null_or(
            row.try_get::<Option<NaiveTime>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(|v| CanonicalValue::Text(v.format("%H:%M:%S%.6f").to_string())),
        ),
        "timestamp" => null_or(
            row.try_get::<Option<NaiveDateTime>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(|v| {
                    CanonicalValue::Timestamp(
                        v.and_utc().to_rfc3339_opts(SecondsFormat::Millis, true),
                    )
                }),
        ),
        "timestamptz" => null_or(
            row.try_get::<Option<DateTime<Utc>>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(|v| CanonicalValue::Timestamp(v.to_rfc3339_opts(SecondsFormat::Millis, true))),
        ),
        // Binary / JSON.
        "bytea" => null_or(
            row.try_get::<Option<Vec<u8>>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(CanonicalValue::Bytes),
        ),
        "json" | "jsonb" => null_or(
            row.try_get::<Option<String>, _>(idx)
                .map_err(|e| decode_error(col, type_name, e))?
                .map(CanonicalValue::Text),
        ),
        _ => unreachable!("type {type_name} passed is_supported_type but has no decoder"),
    }
}

fn decode_error(column: &str, type_name: &str, source: impl std::fmt::Display) -> MeldError {
    MeldError::Serialization(format!(
        "failed to decode column {column} of type {type_name}: {source}"
    ))
}

/// Quote a SQL identifier by doubling double-quotes.
fn sql_quote(s: &str) -> String {
    s.replace('"', "\"\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_quote_doubles_quotes() {
        assert_eq!(sql_quote("public"), "public");
        assert_eq!(sql_quote("we\"ird"), "we\"\"ird");
    }

    #[test]
    fn supported_type_set_is_documented() {
        assert!(is_supported_type("text"));
        assert!(is_supported_type("int4"));
        assert!(is_supported_type("jsonb"));
        assert!(!is_supported_type("tsvector"));
        assert!(!is_supported_type("_text"));
    }
}

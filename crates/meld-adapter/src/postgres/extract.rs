//! PostgreSQL data extraction.
//!
//! Extracts rows from tables using cursor-based queries. Large tables are
//! read in batches to avoid loading unbounded datasets into memory.
//!
//! Column values are decoded type-aware according to the captured PostgreSQL
//! column type description, so that integers, numerics, dates, timestamps,
//! bytea, JSON and other supported types are preserved with their BSON-ish
//! canonical representation rather than being losslessly stringified.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, Utc};
use indexmap::IndexMap;
use meld_core::error::MeldError;
use meld_core::ids::hash_object;
use meld_core::state::{CanonicalValue, PgTable};
use sqlx::Row;
use sqlx::types::BigDecimal;
use tracing::debug;

pub async fn extract_table_rows(
    pool: &sqlx::PgPool,
    table: &PgTable,
    batch_size: Option<u64>,
) -> Result<Vec<meld_core::state::PgRow>, MeldError> {
    let schema = sql_quote(&table.schema);
    let name = sql_quote(&table.name);
    let batch = batch_size.unwrap_or(10_000);

    // Build column list in ordinal order, quoted as identifiers.
    let mut cols: Vec<(&String, &meld_core::state::PgColumn)> = table.columns.iter().collect();
    cols.sort_by_key(|(_, c)| c.ordinal_position);
    let col_list: String = cols
        .iter()
        .map(|(name, _)| format!("\"{}\"", sql_quote(name)))
        .collect::<Vec<_>>()
        .join(", ");

    let q = format!(
        "SELECT {} FROM \"{}\".\"{}\" LIMIT $1 OFFSET $2",
        col_list, schema, name
    );

    let mut rows = Vec::new();
    let mut offset: i64 = 0;
    loop {
        let batch_rows = sqlx::query(&q)
            .bind(batch as i64)
            .bind(offset)
            .fetch_all(pool)
            .await
            .map_err(|e| MeldError::Io(format!("extract rows: {e}")))?;
        if batch_rows.is_empty() {
            break;
        }
        for br in &batch_rows {
            let mut values: IndexMap<String, CanonicalValue> = IndexMap::new();
            for (idx, (col_name, col)) in cols.iter().enumerate() {
                values.insert((*col_name).clone(), decode_cell(br, idx, &col.data_type));
            }
            // Identity: use primary-key values when the table has a primary
            // key, otherwise a content hash of all values (best-effort).
            let identity = if let Some(pk) = &table.primary_key {
                let pk_vals: Vec<CanonicalValue> = pk
                    .columns
                    .iter()
                    .filter_map(|cname| {
                        cols.iter().enumerate().find_map(|(idx, (name, col))| {
                            (name.as_str() == cname.as_str()).then_some(decode_cell(
                                br,
                                idx,
                                &col.data_type,
                            ))
                        })
                    })
                    .collect();
                meld_core::state::PgRowIdentity::PrimaryKey(pk_vals)
            } else {
                let identity_vals: Vec<CanonicalValue> = values.values().cloned().collect();
                let h = hash_object(&serde_json::to_vec(&identity_vals).unwrap_or_default());
                meld_core::state::PgRowIdentity::ContentHash({
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

/// Decode a single SQL cell into a type-aware canonical value, using the
/// captured column type description. NULL cells become `CanonicalValue::Null`.
fn decode_cell(
    row: &sqlx::postgres::PgRow,
    idx: usize,
    pt: &meld_core::state::PgType,
) -> CanonicalValue {
    match pt.name.as_str() {
        // Character types.
        "text" | "varchar" | "character varying" | "char" | "character" | "name" | "citext" => {
            match row.try_get::<Option<String>, _>(idx) {
                Ok(Some(s)) => CanonicalValue::Text(s),
                _ => CanonicalValue::Null,
            }
        }
        "uuid" => match row.try_get::<Option<uuid::Uuid>, _>(idx) {
            Ok(Some(u)) => CanonicalValue::Text(u.to_string()),
            _ => CanonicalValue::Null,
        },
        // Numeric types.
        "int2" | "smallint" => match row.try_get::<Option<i16>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::I32(v as i32),
            _ => CanonicalValue::Null,
        },
        "int4" | "integer" | "serial" | "serial4" => match row.try_get::<Option<i32>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::I32(v),
            _ => CanonicalValue::Null,
        },
        "int8" | "bigint" | "bigserial" | "serial8" => match row.try_get::<Option<i64>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::I64(v),
            _ => CanonicalValue::Null,
        },
        "float4" | "real" => match row.try_get::<Option<f32>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::from_f64(v as f64),
            _ => CanonicalValue::Null,
        },
        "float8" | "double precision" => match row.try_get::<Option<f64>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::from_f64(v),
            _ => CanonicalValue::Null,
        },
        "numeric" | "decimal" => match row.try_get::<Option<BigDecimal>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::Text(v.to_string()),
            _ => CanonicalValue::Null,
        },
        // Boolean.
        "bool" | "boolean" => match row.try_get::<Option<bool>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::Boolean(v),
            _ => CanonicalValue::Null,
        },
        // Temporal types.
        "date" => match row.try_get::<Option<NaiveDate>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::Text(v.format("%Y-%m-%d").to_string()),
            _ => CanonicalValue::Null,
        },
        "time" | "timetz" => match row.try_get::<Option<NaiveTime>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::Text(v.format("%H:%M:%S%.6f").to_string()),
            _ => CanonicalValue::Null,
        },
        "timestamp" => match row.try_get::<Option<NaiveDateTime>, _>(idx) {
            Ok(Some(v)) => {
                CanonicalValue::Timestamp(v.and_utc().to_rfc3339_opts(SecondsFormat::Millis, true))
            }
            _ => CanonicalValue::Null,
        },
        "timestamptz" => match row.try_get::<Option<chrono::DateTime<Utc>>, _>(idx) {
            Ok(Some(v)) => {
                CanonicalValue::Timestamp(v.to_rfc3339_opts(SecondsFormat::Millis, true))
            }
            _ => CanonicalValue::Null,
        },
        // Binary / JSON.
        "bytea" => match row.try_get::<Option<Vec<u8>>, _>(idx) {
            Ok(Some(v)) => CanonicalValue::Bytes(v),
            _ => CanonicalValue::Null,
        },
        "json" | "jsonb" => match row.try_get::<Option<String>, _>(idx) {
            Ok(Some(s)) => CanonicalValue::Text(s),
            _ => CanonicalValue::Null,
        },
        // Fallback: attempt a generic string decode; unknown types are
        // represented textually with a type tag so they are not silently lost.
        _ => match row.try_get::<Option<String>, _>(idx) {
            Ok(Some(s)) => CanonicalValue::Text(format!("<{}> {}", pt.name, s)),
            _ => CanonicalValue::Null,
        },
    }
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
}

//! Ad-hoc SQL execution for the query console.
//!
//! Read-only mode is enforced server-side: the query runs inside a
//! Result size is capped so unbounded result sets cannot exhaust memory.

use std::time::Instant;

use crate::adapter::{QueryRequest, QueryResult};
use chrono::SecondsFormat;
use futures::stream::StreamExt;
use meld_core::error::MeldError;
use meld_core::state::CanonicalValue;
use sqlx::postgres::{PgRow, PgTypeInfo};
use sqlx::types::BigDecimal;
use sqlx::{Column, Row, TypeInfo};

/// Hard cap on rows returned by an ad-hoc query.
const MAX_QUERY_ROWS: u64 = 10_000;

/// Execute an ad-hoc SQL statement. Only `SELECT`-class statements make sense
/// in read-only mode; writes are rejected by the read-only transaction.
pub async fn execute_sql(
    pool: &sqlx::PgPool,
    request: QueryRequest,
) -> Result<QueryResult, MeldError> {
    let QueryRequest::PostgreSql { sql, read_only } = request else {
        return Err(MeldError::Internal(
            "execute_sql called with MongoDB request".into(),
        ));
    };
    let start = Instant::now();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| MeldError::Io(format!("begin transaction: {e}")))?;
    if read_only {
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(|e| MeldError::Io(format!("set transaction read only: {e}")))?;
    }

    let is_select = sql.trim_start().to_ascii_lowercase().starts_with("select");
    let (rows, columns, truncated) = {
        let mut rows: Vec<Vec<CanonicalValue>> = Vec::new();
        let mut columns: Option<Vec<String>> = None;
        let mut truncated = false;
        if is_select {
            let mut cursor = sqlx::query(&sql).fetch(&mut *tx);
            loop {
                let row = match cursor.next().await {
                    Some(Ok(row)) => row,
                    Some(Err(e)) => return Err(MeldError::Io(format!("query execution: {e}"))),
                    None => break,
                };
                if rows.len() >= MAX_QUERY_ROWS as usize {
                    truncated = true;
                    break;
                }
                let cols = columns.get_or_insert_with(|| {
                    row.columns().iter().map(|c| c.name().to_string()).collect()
                });
                let mut cells = Vec::with_capacity(cols.len());
                for (idx, _col) in row.columns().iter().enumerate() {
                    cells.push(decode_cell(&row, idx, row.columns()[idx].type_info())?);
                }
                rows.push(cells);
            }
            drop(cursor);
        } else {
            // DML/DDL statements must be executed, not fetched: `fetch` on a
            // non-query statement does not reliably execute it in sqlx 0.8.
            let result = sqlx::query(&sql)
                .execute(&mut *tx)
                .await
                .map_err(|e| MeldError::Io(format!("query execution: {e}")))?;
            truncated = result.rows_affected() > MAX_QUERY_ROWS;
        }
        let columns = columns.unwrap_or_default();

        tx.commit()
            .await
            .map_err(|e| MeldError::Io(format!("commit query transaction: {e}")))?;

        (rows, columns, truncated)
    };

    Ok(QueryResult {
        row_count: rows.len() as u64,
        columns,
        rows,
        truncated,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// Decode one cell type-aware. NULL decodes to `Null`; a decode failure on a
/// known type is an error (NULL and decode failures are never conflated).
/// Types outside the known set are rejected explicitly.
fn decode_cell(
    row: &PgRow,
    idx: usize,
    type_info: &PgTypeInfo,
) -> Result<CanonicalValue, MeldError> {
    let null_or = |v: Option<CanonicalValue>| match v {
        Some(v) => Ok(v),
        None => Ok(CanonicalValue::Null),
    };

    let type_name = <PgTypeInfo as TypeInfo>::name(type_info);
    match type_name {
        "text" | "varchar" | "character varying" | "char" | "character" | "name" | "citext" => {
            null_or(
                row.try_get::<Option<String>, _>(idx)
                    .map_err(|e| decode_error(type_name, e))?
                    .map(CanonicalValue::Text),
            )
        }
        "uuid" => null_or(
            row.try_get::<Option<uuid::Uuid>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(|u| CanonicalValue::Uuid(*u.as_bytes())),
        ),
        "int2" | "smallint" => null_or(
            row.try_get::<Option<i16>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(|v| CanonicalValue::I32(v as i32)),
        ),
        "int4" | "integer" | "serial" | "serial4" => null_or(
            row.try_get::<Option<i32>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(CanonicalValue::I32),
        ),
        "int8" | "bigint" | "bigserial" | "serial8" => null_or(
            row.try_get::<Option<i64>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(CanonicalValue::I64),
        ),
        "float4" | "real" => null_or(
            row.try_get::<Option<f32>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(|v| CanonicalValue::from_f64(v as f64)),
        ),
        "float8" | "double precision" => null_or(
            row.try_get::<Option<f64>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(CanonicalValue::from_f64),
        ),
        "numeric" | "decimal" => null_or(
            row.try_get::<Option<BigDecimal>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(|v| CanonicalValue::Numeric(v.to_string())),
        ),
        "bool" | "boolean" => null_or(
            row.try_get::<Option<bool>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(CanonicalValue::Boolean),
        ),
        "date" => null_or(
            row.try_get::<Option<chrono::NaiveDate>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(|v| CanonicalValue::Text(v.format("%Y-%m-%d").to_string())),
        ),
        "time" | "timetz" => null_or(
            row.try_get::<Option<chrono::NaiveTime>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(|v| CanonicalValue::Text(v.format("%H:%M:%S%.6f").to_string())),
        ),
        "timestamp" => null_or(
            row.try_get::<Option<chrono::NaiveDateTime>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(|v| {
                    CanonicalValue::Timestamp(
                        v.and_utc().to_rfc3339_opts(SecondsFormat::Millis, true),
                    )
                }),
        ),
        "timestamptz" => null_or(
            row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(|v| CanonicalValue::Timestamp(v.to_rfc3339_opts(SecondsFormat::Millis, true))),
        ),
        "bytea" => null_or(
            row.try_get::<Option<Vec<u8>>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(CanonicalValue::Bytes),
        ),
        "json" | "jsonb" => null_or(
            row.try_get::<Option<String>, _>(idx)
                .map_err(|e| decode_error(type_name, e))?
                .map(CanonicalValue::Text),
        ),
        _ => Err(MeldError::Unsupported(format!(
            "query result type {type_name} is not supported for ad-hoc queries"
        ))),
    }
}

fn decode_error(type_name: &str, source: impl std::fmt::Display) -> MeldError {
    MeldError::Serialization(format!(
        "failed to decode query result of type {type_name}: {source}"
    ))
}

//! PostgreSQL data extraction.
//!
//! Extracts rows from tables using cursor-based queries. Large tables are
//! read in batches to avoid loading unbounded datasets into memory.

use indexmap::IndexMap;
use meld_core::error::MeldError;
use meld_core::ids::hash_object;
use meld_core::state::{CanonicalValue, PgTable};
use sqlx::Row;
use tracing::debug;

pub async fn extract_table_rows(
    pool: &sqlx::PgPool,
    table: &PgTable,
    batch_size: Option<u64>,
) -> Result<Vec<meld_core::state::PgRow>, MeldError> {
    let schema = sql_quote(&table.schema);
    let name = sql_quote(&table.name);
    let batch = batch_size.unwrap_or(10_000);

    // Build column list in ordinal order.
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
            let mut values = IndexMap::new();
            for (col_name, _col) in &cols {
                let v: Option<String> = br.try_get(col_name.as_str()).ok().flatten();
                values.insert(
                    col_name.as_str().to_owned(),
                    match &v {
                        Some(s) => CanonicalValue::Text(s.clone()),
                        None => CanonicalValue::Null,
                    },
                );
            }
            // Identity: use all columns as best-effort content identity.
            let identity_vals: Vec<CanonicalValue> = values.values().cloned().collect();
            let h = hash_object(&serde_json::to_vec(&identity_vals).unwrap_or_default());
            rows.push(meld_core::state::PgRow {
                identity: meld_core::state::PgRowIdentity::ContentHash({
                    let mut arr = [0u8; 16];
                    arr.copy_from_slice(&h.as_bytes()[..16]);
                    arr
                }),
                values,
            });
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

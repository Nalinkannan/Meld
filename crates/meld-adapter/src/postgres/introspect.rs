//! PostgreSQL catalog introspection.
//!
//! Queries pg_catalog to build a typed `PostgresState`. Schemas other than
//! `public` are respected.

use chrono::Utc;
use indexmap::IndexMap;
use meld_core::error::MeldError;
use meld_core::state::{
    CaptureInfo, ConsistencyLevel, PgColumn, PgConstraint, PgDatabase, PgIndex, PgIndexColumn,
    PgSchema, PgSequence, PgTable, PgType, PgView, PostgresState,
};

use super::extract::extract_table_rows;
use sqlx::Row;
use tracing::debug;

pub async fn introspect(pool: &sqlx::PgPool) -> Result<meld_core::state::EngineState, MeldError> {
    // Current database only (the one we're connected to).
    let db_name: String = sqlx::query("SELECT current_database()")
        .fetch_one(pool)
        .await
        .map_err(|e| MeldError::Io(format!("current_database: {e}")))?
        .try_get("current_database")
        .unwrap_or_default();

    let mut schemas = IndexMap::new();

    let schema_rows = sqlx::query(
        "SELECT schema_name FROM information_schema.schemata
         WHERE schema_name NOT IN ('pg_catalog','information_schema','pg_toast')
         ORDER BY schema_name",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| MeldError::Io(format!("list schemas: {e}")))?;

    for sr in &schema_rows {
        let schema_name: String = sr.try_get("schema_name").unwrap_or_default();
        let mut tables = IndexMap::new();

        let table_rows = sqlx::query(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = $1 AND table_type = 'BASE TABLE'
             ORDER BY table_name",
        )
        .bind(&schema_name)
        .fetch_all(pool)
        .await
        .map_err(|e| MeldError::Io(format!("list tables in {schema_name}: {e}")))?;

        for tr in &table_rows {
            let table_name: String = tr.try_get("table_name").unwrap_or_default();
            let object_identity = format!("{schema_name}.{table_name}");
            debug!(object = %object_identity, "introspecting table");

            let mut columns = IndexMap::new();
            let col_rows = sqlx::query(
                "SELECT column_name, ordinal_position, data_type, udt_name, is_nullable,
                        column_default, is_identity, is_generated, generation_expression,
                        character_maximum_length, numeric_precision, numeric_scale
                 FROM information_schema.columns
                 WHERE table_schema = $1 AND table_name = $2
                 ORDER BY ordinal_position",
            )
            .bind(&schema_name)
            .bind(&table_name)
            .fetch_all(pool)
            .await
            .map_err(|e| MeldError::Io(format!("list columns of {object_identity}: {e}")))?;

            for cr in &col_rows {
                let col_name: String = cr.try_get("column_name").unwrap_or_default();
                let data_type: String = cr.try_get("data_type").unwrap_or("unknown".into());
                let udt_name: Option<String> = cr.try_get("udt_name").ok().flatten();
                let is_nullable: String = cr.try_get("is_nullable").unwrap_or("NO".into());
                let column_default: Option<String> = cr.try_get("column_default").ok().flatten();
                let is_identity: String = cr.try_get("is_identity").unwrap_or("NEVER".into());
                let is_generated: String = cr.try_get("is_generated").unwrap_or("NEVER".into());
                let generation_expression: Option<String> =
                    cr.try_get("generation_expression").ok().flatten();
                let char_max_len: Option<i32> =
                    cr.try_get("character_maximum_length").ok().flatten();
                let numeric_precision: Option<i32> = cr.try_get("numeric_precision").ok().flatten();
                let numeric_scale: Option<i32> = cr.try_get("numeric_scale").ok().flatten();

                let pg_type = PgType {
                    name: data_type.clone(),
                    oid: None,
                    internal_type: udt_name,
                    array: data_type.ends_with("[]") || data_type.starts_with("_"),
                    length: char_max_len,
                    precision: numeric_precision,
                    scale: numeric_scale,
                };

                columns.insert(
                    col_name.clone(),
                    PgColumn {
                        name: col_name,
                        ordinal_position: cr.try_get::<i32, _>("ordinal_position").unwrap_or(0)
                            as u32,
                        data_type: pg_type,
                        is_nullable: is_nullable == "YES",
                        column_default,
                        is_identity: is_identity == "ALWAYS" || is_identity == "BY DEFAULT",
                        is_generated: is_generated == "ALWAYS",
                        generated_expression: generation_expression,
                        comment: None,
                    },
                );
            }

            // Primary key constraint.
            let primary_key = get_primary_key(pool, &schema_name, &table_name)
                .await
                .ok()
                .flatten();

            // Unique constraints.
            let unique_constraints =
                get_constraints_of_kind(pool, &schema_name, &table_name, "UNIQUE")
                    .await
                    .unwrap_or_default();

            // Foreign keys.
            let foreign_keys =
                get_constraints_of_kind(pool, &schema_name, &table_name, "FOREIGN KEY")
                    .await
                    .unwrap_or_default();

            // Check constraints.
            let check_constraints =
                get_constraints_of_kind(pool, &schema_name, &table_name, "CHECK")
                    .await
                    .unwrap_or_default();

            // Indexes.
            let indexes = get_indexes(pool, &schema_name, &table_name)
                .await
                .unwrap_or_default();

            let row_count: u64 = count_rows(pool, &schema_name, &table_name)
                .await
                .unwrap_or(0);

            let temp_table = PgTable {
                name: table_name.clone(),
                schema: schema_name.clone(),
                columns: columns.clone(),
                primary_key: primary_key.clone(),
                unique_constraints: unique_constraints.clone(),
                foreign_keys: foreign_keys.clone(),
                check_constraints: check_constraints.clone(),
                indexes: indexes.clone(),
                comment: None,
                row_count,
                rows: vec![],
            };
            let rows = extract_table_rows(pool, &temp_table, None).await?;

            tables.insert(
                table_name.clone(),
                PgTable {
                    name: table_name,
                    schema: schema_name.clone(),
                    columns,
                    primary_key,
                    unique_constraints,
                    foreign_keys,
                    check_constraints,
                    indexes,
                    comment: None,
                    row_count,
                    rows,
                },
            );
        }

        let mut views = IndexMap::new();
        let view_rows = sqlx::query(
            "SELECT table_name, view_definition FROM information_schema.views
             WHERE table_schema = $1 ORDER BY table_name",
        )
        .bind(&schema_name)
        .fetch_all(pool)
        .await
        .map_err(|e| MeldError::Io(format!("list views in {schema_name}: {e}")))?;
        for vr in &view_rows {
            let view_name: String = vr.try_get("table_name").unwrap_or_default();
            let definition: String = vr.try_get("view_definition").unwrap_or_default();
            views.insert(
                view_name.clone(),
                PgView {
                    name: view_name,
                    schema: schema_name.clone(),
                    definition,
                    updatable: false,
                },
            );
        }

        let mut sequences = IndexMap::new();
        let seq_rows = sqlx::query(
            "SELECT sequence_name, data_type, start_value, increment
             FROM information_schema.sequences
             WHERE sequence_schema = $1 ORDER BY sequence_name",
        )
        .bind(&schema_name)
        .fetch_all(pool)
        .await
        .map_err(|e| MeldError::Io(format!("list sequences in {schema_name}: {e}")))?;
        for sr2 in &seq_rows {
            let seq_name: String = sr2.try_get("sequence_name").unwrap_or_default();
            sequences.insert(
                seq_name.clone(),
                PgSequence {
                    name: seq_name.clone(),
                    schema: schema_name.clone(),
                    data_type: PgType {
                        name: sr2.try_get("data_type").unwrap_or("bigint".into()),
                        oid: None,
                        internal_type: None,
                        array: false,
                        length: None,
                        precision: None,
                        scale: None,
                    },
                    start_value: sr2.try_get::<i64, _>("start_value").unwrap_or(1),
                    increment: sr2.try_get::<i64, _>("increment").unwrap_or(1),
                },
            );
        }

        schemas.insert(
            schema_name.clone(),
            PgSchema {
                name: schema_name,
                tables,
            },
        );
        let _ = views;
        let _ = sequences;
    }

    let state = PostgresState {
        engine_version: sqlx::query("SHOW server_version")
            .fetch_one(pool)
            .await
            .and_then(|r| r.try_get::<String, _>("server_version"))
            .unwrap_or_else(|_| "unknown".into()),
        databases: {
            let mut db_map = IndexMap::new();
            let mut schemas_out = IndexMap::new();
            for (name, schema) in schemas {
                schemas_out.insert(name, schema);
            }
            db_map.insert(db_name.clone(), PgDatabase { name: db_name.clone(), schemas: schemas_out, views: IndexMap::new(), sequences: IndexMap::new() });
            db_map
        },
        warnings: vec![],
        capture_info: CaptureInfo {
            consistency: ConsistencyLevel::Snapshot,
            strategy: "single-statement-per-catalog-query; not transactionally isolated across the whole capture".into(),
            captured_at: Utc::now(),
            engine_version: "postgresql".into(),
            scope: db_name.clone(),
            warnings: vec!["PostgreSQL snapshot is captured across multiple catalog queries; concurrent writes between queries may make the capture non-transactional.".into()],
        },
    };

    Ok(meld_core::state::EngineState::PostgreSql(state))
}

async fn get_primary_key(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
) -> Result<Option<PgConstraint>, MeldError> {
    let row = sqlx::query(
        "SELECT conname, pg_get_constraintdef(oid) AS def
         FROM pg_constraint
         JOIN pg_class ON pg_class.oid = conrelid
         JOIN pg_namespace ON pg_namespace.oid = pg_class.relnamespace
         WHERE contype = 'p' AND pg_namespace.nspname = $1 AND pg_class.relname = $2",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(pool)
    .await
    .map_err(|e| MeldError::Io(format!("primary key: {e}")))?;
    let pk_columns_vec = match &row {
        Some(_) => pk_columns(pool, schema, table).await.unwrap_or_default(),
        None => vec![],
    };
    Ok(row.map(|r| PgConstraint {
        name: r.try_get("conname").unwrap_or_default(),
        kind: meld_core::state::ConstraintKind::PrimaryKey,
        definition: r.try_get("def").unwrap_or_default(),
        referenced_table: None,
        referenced_columns: vec![],
        columns: pk_columns_vec.clone(),
    }))
}

async fn pk_columns(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, MeldError> {
    let rows = sqlx::query(
        "SELECT a.attname
         FROM pg_index i
         JOIN pg_class c ON c.oid = i.indrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace
         JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY(i.indkey)
         WHERE n.nspname = $1 AND c.relname = $2 AND i.indisprimary
         ORDER BY a.attnum",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await
    .map_err(|e| MeldError::Io(format!("pk columns: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|r| r.try_get("attname").unwrap_or_default())
        .collect())
}

async fn get_constraints_of_kind(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
    _kind: &str,
) -> Result<Vec<PgConstraint>, MeldError> {
    let rows = sqlx::query(
        "SELECT conname, contype, pg_get_constraintdef(oid) AS def
         FROM pg_constraint
         JOIN pg_class ON pg_class.oid = conrelid
         JOIN pg_namespace ON pg_namespace.oid = pg_class.relnamespace
         WHERE pg_namespace.nspname = $1 AND pg_class.relname = $2",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await
    .map_err(|e| MeldError::Io(format!("constraints: {e}")))?;

    let mut out = Vec::new();
    for r in rows {
        let contype: String = r.try_get("contype").unwrap_or_default();
        let kind = match contype.as_str() {
            "p" => meld_core::state::ConstraintKind::PrimaryKey,
            "u" => meld_core::state::ConstraintKind::Unique,
            "f" => meld_core::state::ConstraintKind::ForeignKey,
            "c" => meld_core::state::ConstraintKind::Check,
            _ => meld_core::state::ConstraintKind::Exclusion,
        };
        out.push(PgConstraint {
            name: r.try_get("conname").unwrap_or_default(),
            kind,
            definition: r.try_get("def").unwrap_or_default(),
            referenced_table: None,
            referenced_columns: vec![],
            columns: vec![],
        });
    }
    Ok(out)
}

async fn get_indexes(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
) -> Result<IndexMap<String, PgIndex>, MeldError> {
    let rows = sqlx::query(
        "SELECT indexname, indexdef, indisunique, indisprimary
         FROM pg_indexes
         WHERE schemaname = $1 AND tablename = $2",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await
    .map_err(|e| MeldError::Io(format!("indexes: {e}")))?;

    let mut out = IndexMap::new();
    for r in rows {
        let name: String = r.try_get("indexname").unwrap_or_default();
        let definition: String = r.try_get("indexdef").unwrap_or_default();
        let is_unique: bool = r.try_get::<bool, _>("indisunique").unwrap_or(false);
        let is_primary: bool = r.try_get::<bool, _>("indisprimary").unwrap_or(false);
        // Column names for the index.
        let columns = index_columns(pool, schema, table, &name)
            .await
            .unwrap_or_default();
        out.insert(
            name.clone(),
            PgIndex {
                name,
                is_unique,
                is_primary,
                columns: columns
                    .into_iter()
                    .map(|c| PgIndexColumn {
                        name: c,
                        ascending: true,
                        nulls_first: true,
                    })
                    .collect(),
                predicate: None,
                definition,
            },
        );
    }
    Ok(out)
}

async fn index_columns(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
    _index: &str,
) -> Result<Vec<String>, MeldError> {
    let rows = sqlx::query(
        "SELECT a.attname
         FROM pg_index i
         JOIN pg_class c ON c.oid = i.indexrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace
         JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
         WHERE n.nspname = $1 AND c.relname = $2",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await
    .map_err(|e| MeldError::Io(format!("index columns: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|r| r.try_get("attname").unwrap_or_default())
        .collect())
}

async fn count_rows(pool: &sqlx::PgPool, schema: &str, table: &str) -> Result<u64, MeldError> {
    let q = format!(
        "SELECT count(*) FROM \"{}\".\"{}\"",
        sql_quote(schema),
        sql_quote(table)
    );
    let row = sqlx::query(&q)
        .fetch_one(pool)
        .await
        .map_err(|e| MeldError::Io(format!("count rows {schema}.{table}: {e}")))?;
    Ok(row.try_get::<i64, _>(0).unwrap_or(0) as u64)
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

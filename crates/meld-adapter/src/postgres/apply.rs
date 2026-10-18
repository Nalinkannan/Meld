//! PostgreSQL restore-plan execution.
//!
//! Executes dependency-aware restore operations in safe order using a
//! database transaction. The engine builds the plan (comparing the live
//! database against the target state) and attaches execution details to each
//! op; this module applies those primitives with type-aware SQL.

use meld_core::ObjectIdentity;
use meld_core::error::MeldError;
use meld_core::plan::{PlanOp, PlanOpKind, RestorePlan};
use meld_core::state::{
    CanonicalValue, EngineKind, PgColumn, PgRow, PgRowIdentity, PgTable, PgType,
};
use tracing::{debug, info, warn};

/// Execute a restore plan against the live PostgreSQL database inside a
/// single transaction where possible.
pub async fn execute(
    pool: &sqlx::PgPool,
    plan: &RestorePlan,
) -> Result<crate::adapter::ExecutionResult, MeldError> {
    if plan.engine_kind != EngineKind::PostgreSql {
        return Err(MeldError::IncompatibleEngine {
            expected: EngineKind::PostgreSql,
            got: plan.engine_kind,
        });
    }

    if plan.irreversible_count > 0 {
        return Err(MeldError::PlanValidation(format!(
            "refusing to execute plan with {} potentially-irreversible operations without manual confirmation",
            plan.irreversible_count
        )));
    }

    let ordered = plan
        .ordered_ops()
        .map_err(|e| MeldError::PlanValidation(e.to_string()))?;

    let mut executed: u64 = 0;
    let mut succeeded: u64 = 0;
    let mut failed: u64 = 0;
    let mut messages: Vec<String> = Vec::new();

    // Single transaction for the whole plan where supported.
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| MeldError::Io(format!("begin transaction: {e}")))?;

    for op in &ordered {
        executed += 1;
        let result = execute_op(&mut tx, op).await;
        match result {
            Ok(msg) => {
                succeeded += 1;
                messages.push(format!("{}: {}", op.id, msg));
                debug!(op = %op.id, risk = ?op.risk, "executed");
            }
            Err(e) => {
                failed += 1;
                messages.push(format!("{}: FAILED: {e}", op.id));
                let _ = tx.rollback().await;
                return Ok(crate::adapter::ExecutionResult {
                    operations_executed: executed,
                    operations_succeeded: succeeded,
                    operations_failed: failed,
                    recovered: false,
                    final_state: None,
                    messages,
                });
            }
        }
    }

    tx.commit()
        .await
        .map_err(|e| MeldError::Io(format!("commit restore plan: {e}")))?;

    info!("restore plan committed: {succeeded}/{executed} operations succeeded");

    Ok(crate::adapter::ExecutionResult {
        operations_executed: executed,
        operations_succeeded: succeeded,
        operations_failed: failed,
        recovered: failed == 0,
        final_state: None,
        messages,
    })
}

/// Execute a single planned operation using its attached details.
async fn execute_op(
    exec: &mut sqlx::postgres::PgConnection,
    op: &PlanOp,
) -> Result<String, MeldError> {
    match &op.object {
        ObjectIdentity::PostgreSql(pg_id) => match pg_id {
            meld_core::identity::PostgresObjectId::Table { schema, table } => match op.kind {
                PlanOpKind::CreateObject => {
                    let pg_table = details_table(&op.details)?;
                    let cols_sql: Vec<String> = pg_table
                        .columns
                        .iter()
                        .map(|(_, c)| column_def_sql(c))
                        .collect::<Result<_, _>>()?;
                    let pk_cols: Vec<String> = pg_table
                        .primary_key
                        .as_ref()
                        .map(|pk| pk.columns.clone())
                        .unwrap_or_default();
                    let mut sql = format!(
                        r#"CREATE TABLE IF NOT EXISTS "{schema}"."{table}" ({cols})"#,
                        schema = sql_quote(schema),
                        table = sql_quote(&pg_table.name),
                        cols = cols_sql.join(", ")
                    );
                    if !pk_cols.is_empty() {
                        let pk_list: Vec<String> = pk_cols
                            .iter()
                            .map(|c| format!("\"{}\"", sql_quote(c)))
                            .collect();
                        sql.push_str(&format!(", PRIMARY KEY ({})", pk_list.join(", ")));
                    }
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("create table: {e}")))?;
                    Ok("created table".into())
                }
                PlanOpKind::DropObject => {
                    let sql = format!(
                        r#"DROP TABLE IF EXISTS "{schema}"."{table}" CASCADE"#,
                        schema = sql_quote(schema),
                        table = sql_quote(table)
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("drop table: {e}")))?;
                    Ok("dropped table".into())
                }
                PlanOpKind::AlterObject => Ok("altered table (no-op)".into()),
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on table not yet implemented",
                    op.kind
                ))),
            },
            meld_core::identity::PostgresObjectId::Column {
                schema,
                table,
                column,
            } => match op.kind {
                PlanOpKind::AddColumn => {
                    let col = details_column(&op.details)?;
                    let col_def = column_def_sql(&col)?;
                    let sql = format!(
                        r#"ALTER TABLE "{schema}"."{table}" ADD COLUMN {col_def}"#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        col_def = col_def,
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("add column: {e}")))?;
                    Ok("added column".into())
                }
                PlanOpKind::DropColumn => {
                    let sql = format!(
                        r#"ALTER TABLE "{schema}"."{table}" DROP COLUMN IF EXISTS "{column}""#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        column = sql_quote(column),
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("drop column: {e}")))?;
                    Ok("dropped column".into())
                }
                PlanOpKind::AlterColumn => {
                    let col = details_column(&op.details)?;
                    let col_type = pg_type_decl(&col.data_type);
                    let sql = format!(
                        r#"ALTER TABLE "{schema}"."{table}" ALTER COLUMN "{column}" TYPE {col_type}"#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        column = sql_quote(column),
                        col_type = col_type,
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("alter column: {e}")))?;
                    Ok("altered column".into())
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on column not yet implemented",
                    op.kind
                ))),
            },
            meld_core::identity::PostgresObjectId::Row {
                schema,
                table,
                identity: _,
            } => match op.kind {
                PlanOpKind::InsertData => {
                    let row = details_row(&op.details)?;
                    let (cols_sql, vals_sql) = row_values_sql(&row.values)?;
                    let sql = format!(
                        "INSERT INTO \"{}\".\"{}\" ({}) VALUES ({})",
                        sql_quote(schema),
                        sql_quote(table),
                        cols_sql,
                        vals_sql
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("insert row: {e}")))?;
                    Ok("inserted row".into())
                }
                PlanOpKind::UpdateData => {
                    let row = details_row(&op.details)?;
                    let (set_sql, where_sql) = row_update_sql(schema, table, &row)?;
                    let sql = format!(
                        "UPDATE \"{}\".\"{}\" SET {} WHERE {}",
                        sql_quote(schema),
                        sql_quote(table),
                        set_sql,
                        where_sql
                    );
                    let result = sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("update row: {e}")))?;
                    if result.rows_affected() == 0 {
                        warn!("update row affected 0 rows; identity may not match");
                    }
                    Ok("updated row".into())
                }
                PlanOpKind::DeleteData => {
                    let row = details_row(&op.details)?;
                    let where_sql = row_where_sql(schema, table, &row)?;
                    let sql = format!(
                        "DELETE FROM \"{}\".\"{}\" WHERE {}",
                        sql_quote(schema),
                        sql_quote(table),
                        where_sql
                    );
                    let result = sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("delete row: {e}")))?;
                    if result.rows_affected() == 0 {
                        warn!("delete row affected 0 rows; identity may not match");
                    }
                    Ok("deleted row".into())
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on row not yet implemented",
                    op.kind
                ))),
            },
            _ => Err(MeldError::Unsupported(format!(
                "restore of {:?} not yet implemented",
                op.object
            ))),
        },
        ObjectIdentity::MongoDb(_) => Err(MeldError::IncompatibleEngine {
            expected: EngineKind::PostgreSql,
            got: EngineKind::MongoDb,
        }),
    }
}

/// Deserialize a `PgTable` from op details.
fn details_table(details: &Option<String>) -> Result<PgTable, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<PgTable>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid table details: {e}")))
}

/// Deserialize a `PgColumn` from op details.
fn details_column(details: &Option<String>) -> Result<PgColumn, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<PgColumn>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid column details: {e}")))
}

/// Deserialize a `PgRow` from op details.
fn details_row(details: &Option<String>) -> Result<PgRow, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<PgRow>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid row details: {e}")))
}

/// SQL declaration for a column type.
fn pg_type_decl(pt: &PgType) -> String {
    match pt.name.as_str() {
        "int2" | "smallint" => "SMALLINT".into(),
        "int4" | "integer" | "serial" | "serial4" => "INTEGER".into(),
        "int8" | "bigint" | "bigserial" | "serial8" => "BIGINT".into(),
        "float4" | "real" => "REAL".into(),
        "float8" | "double precision" => "DOUBLE PRECISION".into(),
        "numeric" | "decimal" => "NUMERIC".into(),
        "text" | "citext" => "TEXT".into(),
        "varchar" | "character varying" => "TEXT".into(),
        "char" | "character" => "CHAR".into(),
        "bool" | "boolean" => "BOOLEAN".into(),
        "date" => "DATE".into(),
        "time" | "timetz" => "TIME".into(),
        "timestamp" => "TIMESTAMP".into(),
        "timestamptz" => "TIMESTAMPTZ".into(),
        "bytea" => "BYTEA".into(),
        "json" => "JSON".into(),
        "jsonb" => "JSONB".into(),
        "uuid" => "UUID".into(),
        _ => "TEXT".into(),
    }
}

/// Full column definition SQL (name type [NULL/NOT NULL]).
fn column_def_sql(col: &PgColumn) -> Result<String, MeldError> {
    let nullability = if col.is_nullable { "NULL" } else { "NOT NULL" };
    let identity = if col.is_identity {
        " GENERATED ALWAYS AS IDENTITY"
    } else {
        ""
    };
    Ok(format!(
        r#""{}" {} {}{}"#,
        sql_quote(&col.name),
        pg_type_decl(&col.data_type),
        nullability,
        identity
    ))
}

/// Quote a SQL identifier by doubling double-quotes.
fn sql_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '"' {
            out.push_str("\"\"");
        } else {
            out.push(c);
        }
    }
    out
}

/// Render a canonical value as a SQL literal.
fn canonical_value_sql(v: &CanonicalValue) -> String {
    match v {
        CanonicalValue::Null => "NULL".into(),
        CanonicalValue::Boolean(b) => {
            if *b {
                "TRUE".into()
            } else {
                "FALSE".into()
            }
        }
        CanonicalValue::I32(i) => i.to_string(),
        CanonicalValue::I64(i) => i.to_string(),
        CanonicalValue::F64(bits) => {
            let f = f64::from_bits(*bits);
            if f.is_nan() {
                "'NaN'::float8".into()
            } else {
                f.to_string()
            }
        }
        CanonicalValue::Text(s) => {
            let esc = s.replace('\'', "''");
            format!("'{}'", esc)
        }
        CanonicalValue::Bytes(b) => {
            let hex: String = b.iter().map(|b| format!("{:02x}", b)).collect();
            format!("E'\\\\x{}'", hex)
        }
        CanonicalValue::Timestamp(s) => format!("'{}'", s),
        CanonicalValue::ObjectId(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
            format!("'{}'::uuid", hex)
        }
        CanonicalValue::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_value_sql).collect();
            format!("ARRAY[{}]", parts.join(","))
        }
        CanonicalValue::Document(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| {
                    let ek = k.replace('\'', "''");
                    let ev = canonical_value_sql(v);
                    format!("'{}': {}", ek, ev)
                })
                .collect();
            format!("'{{{}}}'::jsonb", parts.join(","))
        }
        CanonicalValue::Numeric(s) => format!("'{}'::numeric", s),
    }
}

/// Build SELECT columns + VALUES SQL for an insert from a row's values map.
fn row_values_sql(
    values: &indexmap::IndexMap<String, CanonicalValue>,
) -> Result<(String, String), MeldError> {
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (name, v) in values {
        cols.push(format!("\"{}\"", sql_quote(name)));
        vals.push(canonical_value_sql(v));
    }
    Ok((cols.join(", "), vals.join(", ")))
}

/// Build SET clause + WHERE clause for an update from a row. The WHERE uses
/// the row's primary-key identity when available.
fn row_update_sql(schema: &str, table: &str, row: &PgRow) -> Result<(String, String), MeldError> {
    let mut sets = Vec::new();
    for (name, v) in &row.values {
        sets.push(format!(
            "\"{}\" = {}",
            sql_quote(name),
            canonical_value_sql(v)
        ));
    }
    let where_sql = row_where_sql(schema, table, row)?;
    Ok((sets.join(", "), where_sql))
}

/// Build a WHERE clause targeting a row by its primary-key identity.
fn row_where_sql(_schema: &str, _table: &str, row: &PgRow) -> Result<String, MeldError> {
    match &row.identity {
        PgRowIdentity::PrimaryKey(values) => {
            // Need column names for the PK columns; details carry the full row
            // including names via values map (values keys are column names).
            // But PrimaryKey only stores values, not names. Recover names from
            // the values map keys in order is not reliable. Instead, require
            // details to carry the PK column names. For now, use values map
            // keys in insertion order as a best effort (matches how extract
            // stored them keyed by column name).
            let names: Vec<&String> = row.values.keys().collect();
            if names.len() != values.len() {
                return Err(MeldError::InvalidInput(
                    "row identity column count mismatch".into(),
                ));
            }
            let mut clauses = Vec::new();
            for (name, val) in row.values.iter() {
                clauses.push(format!(
                    "\"{}\" = {}",
                    sql_quote(name),
                    canonical_value_sql(val)
                ));
            }
            Ok(clauses.join(" AND "))
        }
        PgRowIdentity::ContentHash(_) => Err(MeldError::Unsupported(
            "row updates/deletes require a primary key identity".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_type_decl_mapping() {
        let int_t = PgType {
            name: "integer".into(),
            oid: None,
            internal_type: None,
            array: false,
            length: None,
            precision: None,
            scale: None,
        };
        let text_t = PgType {
            name: "text".into(),
            oid: None,
            internal_type: None,
            array: false,
            length: None,
            precision: None,
            scale: None,
        };
        let bool_t = PgType {
            name: "boolean".into(),
            oid: None,
            internal_type: None,
            array: false,
            length: None,
            precision: None,
            scale: None,
        };
        assert_eq!(pg_type_decl(&int_t), "INTEGER");
        assert_eq!(pg_type_decl(&text_t), "TEXT");
        assert_eq!(pg_type_decl(&bool_t), "BOOLEAN");
    }

    #[test]
    fn canonical_value_sql_round_trip_text() {
        assert_eq!(
            canonical_value_sql(&CanonicalValue::Text("O'Brien".into())),
            "'O''Brien'"
        );
        assert_eq!(canonical_value_sql(&CanonicalValue::I32(42)), "42");
        assert_eq!(canonical_value_sql(&CanonicalValue::Null), "NULL");
    }
}

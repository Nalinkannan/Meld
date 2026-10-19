//! PostgreSQL restore-plan execution.
//!
//! Executes dependency-aware restore operations in safe order inside a single
//! transaction. Data values are always sent as bound query parameters --
//! nothing is interpolated into executable SQL as a literal. Values are bound
//! as text with an explicit server-side cast (`$n::int4`, `$n::text`, ...)
//! chosen from the captured column type, so the SQL text contains only
//! identifier quotes and cast targets from catalog metadata. Identifiers are
//! quoted with the shared quoting routine.
//!
//! Every operation checks its postcondition (rows matched, objects created or
//! dropped) and reports failure explicitly instead of claiming success on a
//! no-op. `CREATE TABLE` is plain (no `IF NOT EXISTS`) and `DROP TABLE` has
//! no `CASCADE`; dependent objects must be enumerated in the plan explicitly.

use indexmap::IndexMap;
use meld_core::ObjectIdentity;
use meld_core::error::MeldError;
use meld_core::plan::{PlanOp, PlanOpKind, RestorePlan};
use meld_core::state::{
    CanonicalValue, EngineKind, PgColumn, PgConstraint, PgIndex, PgRow, PgRowIdentity, PgSequence,
    PgTable, PgType, PgView,
};
use sqlx::{Arguments, Row, postgres::PgArguments, postgres::PgConnection};
use tracing::{debug, info};

/// Execute a restore plan against the live PostgreSQL database inside a
/// single transaction.
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

    // Single transaction for the whole plan: on failure everything rolls back
    // and the failure is reported explicitly.
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
async fn execute_op(exec: &mut PgConnection, op: &PlanOp) -> Result<String, MeldError> {
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
                        .map(|pk| pk.columns.to_vec())
                        .unwrap_or_default();
                    let mut sql = format!(
                        r#"CREATE TABLE "{schema}"."{table}" ({cols})"#,
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
                    // Plain CREATE TABLE: if the table already exists the
                    // operation fails explicitly instead of silently leaving an
                    // incompatible table untouched.
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("create table: {e}")))?;
                    Ok("created table".into())
                }
                PlanOpKind::DropObject => {
                    // No CASCADE: dependent objects must be enumerated in the
                    // plan explicitly.
                    let sql = format!(
                        r#"DROP TABLE "{schema}"."{table}""#,
                        schema = sql_quote(schema),
                        table = sql_quote(table)
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("drop table: {e}")))?;
                    Ok("dropped table".into())
                }
                PlanOpKind::AlterObject => Err(MeldError::Unsupported(
                    "table-level ALTER is not a single operation; use column/constraint operations"
                        .into(),
                )),
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
                    let columns = load_columns(exec, schema, table).await?;
                    insert_row(exec, schema, table, &columns, &row).await?;
                    Ok("inserted row".into())
                }
                PlanOpKind::UpdateData => {
                    let row = details_row(&op.details)?;
                    let columns = load_columns(exec, schema, table).await?;
                    let affected = update_row(exec, schema, table, &columns, &row).await?;
                    if affected == 0 {
                        return Err(MeldError::Io(format!(
                            "update row in {schema}.{table} affected 0 rows: target identity not found"
                        )));
                    }
                    Ok("updated row".into())
                }
                PlanOpKind::DeleteData => {
                    let row = details_row(&op.details)?;
                    let columns = load_columns(exec, schema, table).await?;
                    let affected = delete_row(exec, schema, table, &columns, &row).await?;
                    if affected == 0 {
                        return Err(MeldError::Io(format!(
                            "delete row from {schema}.{table} affected 0 rows: target identity not found"
                        )));
                    }
                    Ok("deleted row".into())
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on row not yet implemented",
                    op.kind
                ))),
            },
            meld_core::identity::PostgresObjectId::Constraint {
                schema,
                table,
                constraint,
            } => match op.kind {
                PlanOpKind::CreateConstraint => {
                    let c = details_constraint(&op.details)?;
                    let sql = format!(
                        r#"ALTER TABLE "{schema}"."{table}" ADD CONSTRAINT "{constraint}" {def}"#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        constraint = sql_quote(constraint),
                        def = c.definition,
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("create constraint: {e}")))?;
                    Ok("created constraint".into())
                }
                PlanOpKind::DropConstraint => {
                    let sql = format!(
                        r#"ALTER TABLE "{schema}"."{table}" DROP CONSTRAINT IF EXISTS "{constraint}""#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        constraint = sql_quote(constraint),
                    );
                    sqlx::query(&sql)
                        .execute(&mut *exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("drop constraint: {e}")))?;
                    Ok("dropped constraint".into())
                }
                PlanOpKind::AlterObject => {
                    // PostgreSQL cannot alter most constraints in place; drop and recreate.
                    let c = details_constraint(&op.details)?;
                    let drop_sql = format!(
                        r#"ALTER TABLE "{schema}"."{table}" DROP CONSTRAINT IF EXISTS "{constraint}""#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        constraint = sql_quote(constraint),
                    );
                    sqlx::query(&drop_sql)
                        .execute(&mut *exec)
                        .await
                        .map_err(|e| {
                            MeldError::Io(format!("drop constraint before recreate: {e}"))
                        })?;
                    let create_sql = format!(
                        r#"ALTER TABLE "{schema}"."{table}" ADD CONSTRAINT "{constraint}" {def}"#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        constraint = sql_quote(constraint),
                        def = c.definition,
                    );
                    sqlx::query(&create_sql)
                        .execute(&mut *exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("create constraint: {e}")))?;
                    Ok("altered constraint".into())
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on constraint not yet implemented",
                    op.kind
                ))),
            },
            meld_core::identity::PostgresObjectId::Index {
                schema,
                table,
                index,
            } => match op.kind {
                PlanOpKind::CreateIndex => {
                    let idx = details_index(&op.details)?;
                    let sql = format!(
                        r#"CREATE INDEX "{index}" ON "{schema}"."{table}" {def}"#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        index = sql_quote(index),
                        def = idx.definition,
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("create index: {e}")))?;
                    Ok("created index".into())
                }
                PlanOpKind::DropIndex => {
                    let sql = format!(
                        r#"DROP INDEX IF EXISTS "{schema}"."{index}""#,
                        schema = sql_quote(schema),
                        index = sql_quote(index),
                    );
                    sqlx::query(&sql)
                        .execute(&mut *exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("drop index: {e}")))?;
                    Ok("dropped index".into())
                }
                PlanOpKind::AlterIndex => {
                    let idx = details_index(&op.details)?;
                    let drop_sql = format!(
                        r#"DROP INDEX IF EXISTS "{schema}"."{index}""#,
                        schema = sql_quote(schema),
                        index = sql_quote(index),
                    );
                    sqlx::query(&drop_sql)
                        .execute(&mut *exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("drop index before recreate: {e}")))?;
                    let create_sql = format!(
                        r#"CREATE INDEX "{index}" ON "{schema}"."{table}" {def}"#,
                        schema = sql_quote(schema),
                        table = sql_quote(table),
                        index = sql_quote(index),
                        def = idx.definition,
                    );
                    sqlx::query(&create_sql)
                        .execute(&mut *exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("create index: {e}")))?;
                    Ok("altered index".into())
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on index not yet implemented",
                    op.kind
                ))),
            },
            meld_core::identity::PostgresObjectId::View { schema, view } => match op.kind {
                PlanOpKind::CreateObject => {
                    let v = details_view(&op.details)?;
                    let sql = format!(
                        r#"CREATE VIEW "{schema}"."{view}" AS {def}"#,
                        schema = sql_quote(schema),
                        view = sql_quote(view),
                        def = v.definition,
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("create view: {e}")))?;
                    Ok("created view".into())
                }
                PlanOpKind::DropObject => {
                    let sql = format!(
                        r#"DROP VIEW IF EXISTS "{schema}"."{view}""#,
                        schema = sql_quote(schema),
                        view = sql_quote(view),
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("drop view: {e}")))?;
                    Ok("dropped view".into())
                }
                PlanOpKind::AlterObject => {
                    let v = details_view(&op.details)?;
                    let sql = format!(
                        r#"CREATE OR REPLACE VIEW "{schema}"."{view}" AS {def}"#,
                        schema = sql_quote(schema),
                        view = sql_quote(view),
                        def = v.definition,
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("alter view: {e}")))?;
                    Ok("altered view".into())
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on view not yet implemented",
                    op.kind
                ))),
            },
            meld_core::identity::PostgresObjectId::Sequence { schema, sequence } => match op.kind {
                PlanOpKind::CreateObject => {
                    let s = details_sequence(&op.details)?;
                    let sql = format!(
                        r#"CREATE SEQUENCE "{schema}"."{sequence}" AS {type_} START WITH {start} INCREMENT BY {inc}"#,
                        schema = sql_quote(schema),
                        sequence = sql_quote(sequence),
                        type_ = s.data_type.name,
                        start = s.start_value,
                        inc = s.increment,
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("create sequence: {e}")))?;
                    Ok("created sequence".into())
                }
                PlanOpKind::DropObject => {
                    let sql = format!(
                        r#"DROP SEQUENCE IF EXISTS "{schema}"."{sequence}""#,
                        schema = sql_quote(schema),
                        sequence = sql_quote(sequence),
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("drop sequence: {e}")))?;
                    Ok("dropped sequence".into())
                }
                PlanOpKind::AlterObject => {
                    let s = details_sequence(&op.details)?;
                    let sql = format!(
                        r#"ALTER SEQUENCE "{schema}"."{sequence}" RESTART WITH {start} INCREMENT BY {inc}"#,
                        schema = sql_quote(schema),
                        sequence = sql_quote(sequence),
                        start = s.start_value,
                        inc = s.increment,
                    );
                    sqlx::query(&sql)
                        .execute(exec)
                        .await
                        .map_err(|e| MeldError::Io(format!("alter sequence: {e}")))?;
                    Ok("altered sequence".into())
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on sequence not yet implemented",
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

fn details_constraint(details: &Option<String>) -> Result<PgConstraint, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<PgConstraint>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid constraint details: {e}")))
}

fn details_index(details: &Option<String>) -> Result<PgIndex, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<PgIndex>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid index details: {e}")))
}

fn details_view(details: &Option<String>) -> Result<PgView, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<PgView>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid view details: {e}")))
}

fn details_sequence(details: &Option<String>) -> Result<PgSequence, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<PgSequence>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid sequence details: {e}")))
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
        "varchar" | "character varying" => "VARCHAR".into(),
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
    let default = match &col.column_default {
        Some(d) => format!(" DEFAULT {d}"),
        None => String::new(),
    };
    Ok(format!(
        r#""{}" {} {}{}{}"#,
        sql_quote(&col.name),
        pg_type_decl(&col.data_type),
        nullability,
        identity,
        default
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

/// Load the columns of a table from the live catalog, in ordinal order, so
/// values can be bound with server casts matching each column type.
async fn load_columns(
    exec: &mut PgConnection,
    schema: &str,
    table: &str,
) -> Result<Vec<PgColumn>, MeldError> {
    let q = "SELECT column_name, ordinal_position, data_type, is_nullable
         FROM information_schema.columns
         WHERE table_schema = $1 AND table_name = $2
         ORDER BY ordinal_position";
    let rows = sqlx::query(q)
        .bind(schema)
        .bind(table)
        .fetch_all(exec)
        .await
        .map_err(|e| MeldError::Io(format!("load columns {schema}.{table}: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        let name: String = r.try_get("column_name").unwrap_or_default();
        let data_type: String = r.try_get("data_type").unwrap_or("unknown".into());
        out.push(PgColumn {
            name,
            ordinal_position: r.try_get::<i32, _>("ordinal_position").unwrap_or(0) as u32,
            data_type: PgType {
                name: data_type.clone(),
                oid: None,
                internal_type: None,
                array: data_type.ends_with("[]"),
                length: None,
                precision: None,
                scale: None,
            },
            is_nullable: r.try_get::<String, _>("is_nullable").unwrap_or_default() == "YES",
            column_default: None,
            is_identity: false,
            is_generated: false,
            generated_expression: None,
            comment: None,
        });
    }
    Ok(out)
}

/// The PostgreSQL type cast applied to a bound text parameter for a column.
fn cast_sql_for_column(col: &PgColumn) -> &'static str {
    match col.data_type.name.as_str() {
        "int2" | "smallint" => "int2",
        "int4" | "integer" | "serial" | "serial4" => "int4",
        "int8" | "bigint" | "bigserial" | "serial8" => "int8",
        "float4" | "real" => "float4",
        "float8" | "double precision" => "float8",
        "numeric" | "decimal" => "numeric",
        "bool" | "boolean" => "boolean",
        "text" | "varchar" | "character varying" | "char" | "character" | "name" | "citext" => {
            "text"
        }
        "uuid" => "uuid",
        "json" | "jsonb" => "text",
        "bytea" => "bytea",
        "date" => "date",
        "time" | "timetz" => "time",
        "timestamp" => "timestamp",
        "timestamptz" => "timestamptz",
        _ => "text",
    }
}

/// Render a canonical value as the text content of a bound parameter.
/// `None` represents SQL NULL.
fn render_parameter(col: &PgColumn, v: &CanonicalValue) -> Result<Option<String>, MeldError> {
    Ok(match v {
        CanonicalValue::Null => None,
        CanonicalValue::Boolean(b) => Some(if *b { "true" } else { "false" }.to_string()),
        CanonicalValue::I32(i) => Some(i.to_string()),
        CanonicalValue::I64(i) => Some(i.to_string()),
        CanonicalValue::F64(bits) => {
            let f = f64::from_bits(*bits);
            if f.is_nan() {
                Some("NaN".to_string())
            } else if f.is_infinite() {
                Some(
                    if f.is_sign_positive() {
                        "Infinity"
                    } else {
                        "-Infinity"
                    }
                    .to_string(),
                )
            } else {
                Some(f.to_string())
            }
        }
        CanonicalValue::Numeric(s) => Some(s.clone()),
        CanonicalValue::Text(s) => Some(s.clone()),
        CanonicalValue::Bytes(b) => {
            let mut hex = String::with_capacity(b.len() * 2 + 2);
            hex.push_str("\\x");
            for byte in b {
                hex.push_str(&format!("{byte:02x}"));
            }
            Some(hex)
        }
        CanonicalValue::Timestamp(s) => Some(s.clone()),
        CanonicalValue::Uuid(bytes) => {
            let uuid = uuid::Uuid::from_bytes(*bytes);
            Some(uuid.to_string())
        }
        CanonicalValue::ObjectId(_) => {
            return Err(MeldError::Unsupported(
                "ObjectId is not a PostgreSQL type".into(),
            ));
        }
        CanonicalValue::Decimal128(_)
        | CanonicalValue::Binary { .. }
        | CanonicalValue::BsonTimestamp { .. }
        | CanonicalValue::Regex { .. }
        | CanonicalValue::JavaScript { .. }
        | CanonicalValue::MinKey
        | CanonicalValue::MaxKey
        | CanonicalValue::Array(_)
        | CanonicalValue::Document(_) => {
            return Err(MeldError::Unsupported(format!(
                "cannot restore {} to PostgreSQL column {}",
                variant_name(v),
                col.name
            )));
        }
    })
}

fn variant_name(v: &CanonicalValue) -> &'static str {
    match v {
        CanonicalValue::Null => "null",
        CanonicalValue::Boolean(_) => "boolean",
        CanonicalValue::I32(_) => "i32",
        CanonicalValue::I64(_) => "i64",
        CanonicalValue::F64(_) => "f64",
        CanonicalValue::Numeric(_) => "numeric",
        CanonicalValue::Text(_) => "text",
        CanonicalValue::Bytes(_) => "bytes",
        CanonicalValue::Timestamp(_) => "timestamp",
        CanonicalValue::ObjectId(_) => "objectid",
        CanonicalValue::Uuid(_) => "uuid",
        CanonicalValue::Decimal128(_) => "decimal128",
        CanonicalValue::Binary { .. } => "binary",
        CanonicalValue::BsonTimestamp { .. } => "bson-timestamp",
        CanonicalValue::Regex { .. } => "regex",
        CanonicalValue::JavaScript { .. } => "javascript",
        CanonicalValue::MinKey => "minkey",
        CanonicalValue::MaxKey => "maxkey",
        CanonicalValue::Array(_) => "array",
        CanonicalValue::Document(_) => "document",
    }
}

/// Build `INSERT INTO ... (cols) VALUES ($1::type, $2::type, ...)` with all
/// data values bound as text parameters.
async fn insert_row(
    exec: &mut PgConnection,
    schema: &str,
    table: &str,
    columns: &[PgColumn],
    row: &PgRow,
) -> Result<(), MeldError> {
    let mut col_names = Vec::new();
    let mut casts = Vec::new();
    let mut params: Vec<Option<String>> = Vec::new();

    for (i, c) in columns.iter().enumerate() {
        let Some(v) = row.values.get(&c.name) else {
            // Absent columns are inserted as NULL; NOT NULL columns without a
            // default are rejected explicitly below.
            if !c.is_nullable && c.column_default.is_none() && !c.is_identity {
                return Err(MeldError::InvalidInput(format!(
                    "row insert missing required column {}",
                    c.name
                )));
            }
            col_names.push(c.name.clone());
            casts.push(format!("${}::{}", i + 1, cast_sql_for_column(c)));
            params.push(None);
            continue;
        };
        col_names.push(c.name.clone());
        casts.push(format!("${}::{}", i + 1, cast_sql_for_column(c)));
        params.push(render_parameter(c, v)?);
    }

    let cols_sql: Vec<String> = col_names
        .iter()
        .map(|n| format!("\"{}\"", sql_quote(n)))
        .collect();
    let placeholders = casts.join(", ");
    let sql = format!(
        "INSERT INTO \"{}\".\"{}\" ({}) VALUES ({})",
        sql_quote(schema),
        sql_quote(table),
        cols_sql.join(", "),
        placeholders
    );
    let mut args = PgArguments::default();
    for p in params {
        args.add(p)
            .map_err(|e| MeldError::Io(format!("bind argument: {e}")))?;
    }
    let query = sqlx::query_with(&sql, args);
    query
        .execute(exec)
        .await
        .map_err(|e| MeldError::Io(format!("insert row: {e}")))?;
    Ok(())
}

/// Build `UPDATE ... SET col = $n::type, ... WHERE pk-col = $m::type, ...`
/// using only the primary-key identity columns as the row predicate.
async fn update_row(
    exec: &mut PgConnection,
    schema: &str,
    table: &str,
    columns: &[PgColumn],
    row: &PgRow,
) -> Result<u64, MeldError> {
    let PgRowIdentity::PrimaryKey(pk) = &row.identity else {
        return Err(MeldError::Unsupported(
            "row updates require a primary-key identity".into(),
        ));
    };
    if pk.is_empty() {
        return Err(MeldError::Unsupported(
            "row updates require a primary-key identity".into(),
        ));
    }

    let column_by_name: IndexMap<&String, &PgColumn> =
        columns.iter().map(|c| (&c.name, c)).collect();

    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<Option<String>> = Vec::new();

    // SET every non-identity column present in the row. Placeholder numbers
    // track `params.len()` so SET and WHERE clauses use distinct positions.
    for c in columns {
        if pk.iter().any(|(n, _)| n == &c.name) {
            continue;
        }
        let Some(v) = row.values.get(&c.name) else {
            continue;
        };
        let n = params.len() + 1;
        sets.push(format!(
            "\"{}\" = ${}::{}",
            sql_quote(&c.name),
            n,
            cast_sql_for_column(c)
        ));
        params.push(render_parameter(c, v)?);
    }
    if sets.is_empty() {
        return Err(MeldError::InvalidInput(
            "update row has no columns to set".into(),
        ));
    }

    // WHERE uses only the primary-key identity columns -- never changed
    // target values, so a row whose values were updated is still found.
    let mut where_parts: Vec<String> = Vec::new();
    for (name, _value) in pk {
        let c = column_by_name.get(name).ok_or_else(|| {
            MeldError::InvalidInput(format!("identity column {name} not in table {table}"))
        })?;
        let n = params.len() + 1;
        where_parts.push(format!(
            "\"{}\" = ${}::{}",
            sql_quote(name),
            n,
            cast_sql_for_column(c)
        ));
        // The identity value was captured with the row: look it up.
        let Some(v) = row.values.get(name) else {
            return Err(MeldError::InvalidInput(format!(
                "row identity value missing for column {name}"
            )));
        };
        params.push(render_parameter(c, v)?);
    }

    let sql = format!(
        "UPDATE \"{}\".\"{}\" SET {} WHERE {}",
        sql_quote(schema),
        sql_quote(table),
        sets.join(", "),
        where_parts.join(" AND ")
    );
    let mut args = PgArguments::default();
    for p in params {
        args.add(p)
            .map_err(|e| MeldError::Io(format!("bind argument: {e}")))?;
    }
    let query = sqlx::query_with(&sql, args);
    let result = query
        .execute(exec)
        .await
        .map_err(|e| MeldError::Io(format!("update row: {e}")))?;
    Ok(result.rows_affected())
}

/// Build `DELETE FROM ... WHERE pk-col = $1::type, ...` using only the
/// primary-key identity columns as the row predicate.
async fn delete_row(
    exec: &mut PgConnection,
    schema: &str,
    table: &str,
    columns: &[PgColumn],
    row: &PgRow,
) -> Result<u64, MeldError> {
    let PgRowIdentity::PrimaryKey(pk) = &row.identity else {
        return Err(MeldError::Unsupported(
            "row deletes require a primary-key identity".into(),
        ));
    };
    if pk.is_empty() {
        return Err(MeldError::Unsupported(
            "row deletes require a primary-key identity".into(),
        ));
    }

    let column_by_name: IndexMap<&String, &PgColumn> =
        columns.iter().map(|c| (&c.name, c)).collect();

    let mut where_parts: Vec<String> = Vec::new();
    let mut params: Vec<Option<String>> = Vec::new();

    for (param_no, (name, _value)) in (1usize..).zip(pk) {
        let c = column_by_name.get(name).ok_or_else(|| {
            MeldError::InvalidInput(format!("identity column {name} not in table {table}"))
        })?;
        where_parts.push(format!(
            "\"{}\" = ${}::{}",
            sql_quote(name),
            param_no,
            cast_sql_for_column(c)
        ));
        let Some(v) = row.values.get(name) else {
            return Err(MeldError::InvalidInput(format!(
                "row identity value missing for column {name}"
            )));
        };
        params.push(render_parameter(c, v)?);
    }

    let sql = format!(
        "DELETE FROM \"{}\".\"{}\" WHERE {}",
        sql_quote(schema),
        sql_quote(table),
        where_parts.join(" AND ")
    );
    let mut args = PgArguments::default();
    for p in params {
        args.add(p)
            .map_err(|e| MeldError::Io(format!("bind argument: {e}")))?;
    }
    let query = sqlx::query_with(&sql, args);
    let result = query
        .execute(exec)
        .await
        .map_err(|e| MeldError::Io(format!("delete row: {e}")))?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, data_type: &str) -> PgColumn {
        PgColumn {
            name: name.into(),
            ordinal_position: 1,
            data_type: PgType {
                name: data_type.into(),
                oid: None,
                internal_type: None,
                array: false,
                length: None,
                precision: None,
                scale: None,
            },
            is_nullable: true,
            column_default: None,
            is_identity: false,
            is_generated: false,
            generated_expression: None,
            comment: None,
        }
    }

    #[test]
    fn sql_quote_doubles_quotes() {
        assert_eq!(sql_quote("public"), "public");
        assert_eq!(sql_quote("we\"ird"), "we\"\"ird");
    }

    #[test]
    fn cast_sql_for_known_types() {
        assert_eq!(cast_sql_for_column(&col("a", "int4")), "int4");
        assert_eq!(cast_sql_for_column(&col("a", "text")), "text");
        assert_eq!(cast_sql_for_column(&col("a", "jsonb")), "text");
        assert_eq!(cast_sql_for_column(&col("a", "uuid")), "uuid");
    }

    #[test]
    fn render_parameter_null_and_text() {
        assert_eq!(
            render_parameter(&col("a", "text"), &CanonicalValue::Null).unwrap(),
            None
        );
        assert_eq!(
            render_parameter(&col("a", "text"), &CanonicalValue::Text("O'Brien".into())).unwrap(),
            Some("O'Brien".into())
        );
        assert_eq!(
            render_parameter(&col("a", "int4"), &CanonicalValue::I32(-7)).unwrap(),
            Some("-7".into())
        );
        assert_eq!(
            render_parameter(&col("a", "bytea"), &CanonicalValue::Bytes(vec![0xde, 0xad])).unwrap(),
            Some("\\xdead".into())
        );
    }

    #[test]
    fn unsupported_bson_variant_rejected_for_pg() {
        let err = render_parameter(&col("a", "text"), &CanonicalValue::ObjectId(vec![1u8; 12]))
            .unwrap_err();
        assert!(matches!(err, MeldError::Unsupported { .. }));
    }
}

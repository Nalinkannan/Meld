//! Restore-plan building.
//!
//! Compares a target commit state against the current live database state and
//! produces a dependency-aware, execution-ready plan. The engine owns this
//! logic so the plan can be previewed and confirmed before the adapter
//! executes it.

use meld_adapter::DatabaseAdapter;
use meld_core::plan::{OperationRisk, PlanOp, PlanOpKind, RestorePlan};
use meld_core::state::{EngineKind, EngineState, PgRowIdentity, PgTable, PostgresState};
use serde_json;

use crate::error::EngineError;

/// Build a dependency-aware restore plan that transforms the live database
/// into the state captured at `target_commit`.
pub async fn build_restore_plan(
    adapter: &mut dyn DatabaseAdapter,
    target_commit: &meld_core::commit::Commit,
) -> Result<RestorePlan, EngineError> {
    let engine_kind = target_commit.state.engine_kind();
    let root_hash = target_commit.root_hash;

    let live_state = adapter
        .introspect()
        .await
        .map_err(|e| EngineError::Restore(format!("failed to introspect live database: {e}")))?;

    let ops = match (&target_commit.state, &live_state) {
        (EngineState::PostgreSql(target_state), EngineState::PostgreSql(live_state)) => {
            build_postgres_plan(adapter, target_state, live_state).await?
        }
        (EngineState::PostgreSql(_), EngineState::MongoDb(_)) => {
            return Err(EngineError::IncompatibleEngine {
                expected: EngineKind::PostgreSql,
                got: EngineKind::MongoDb,
            });
        }
        (EngineState::MongoDb(_), _) => {
            return Err(EngineError::Unsupported(
                "MongoDB restore planning not yet implemented".into(),
            ));
        }
    };

    Ok(RestorePlan::new(target_commit.hash, root_hash, engine_kind).with_ops(ops))
}

/// Build a PostgreSQL restore plan by diffing the target state against the
/// live state, table by table.
async fn build_postgres_plan(
    adapter: &dyn DatabaseAdapter,
    target: &PostgresState,
    live: &PostgresState,
) -> Result<Vec<PlanOp>, EngineError> {
    let mut ops: Vec<PlanOp> = Vec::new();

    for (db_name, target_db) in &target.databases {
        for (schema_name, target_schema) in &target_db.schemas {
            let live_schema = live
                .databases
                .get(db_name)
                .and_then(|db| db.schemas.get(schema_name));
            for (table_name, target_table) in &target_schema.tables {
                match live_schema.and_then(|s| s.tables.get(table_name)) {
                    None => {
                        ops.push(table_op(
                            next_op_id(),
                            PlanOpKind::CreateObject,
                            schema_name,
                            table_name,
                            OperationRisk::Reversible,
                            format!("create table {schema_name}.{table_name}"),
                            Some(
                                serde_json::to_string(target_table)
                                    .map_err(|e| EngineError::Internal(e.to_string()))?,
                            ),
                        ));
                    }
                    Some(live_table) => {
                        ops.extend(
                            diff_table(schema_name, table_name, target_table, live_table, adapter)
                                .await?,
                        );
                    }
                }
            }
        }
    }

    Ok(ops)
}

/// Build plan ops diffing one table (columns + rows).
async fn diff_table(
    schema: &str,
    table: &str,
    target: &PgTable,
    live: &PgTable,
    adapter: &dyn DatabaseAdapter,
) -> Result<Vec<PlanOp>, EngineError> {
    let mut ops: Vec<PlanOp> = Vec::new();

    // Columns.
    for (name, col) in &target.columns {
        if !live.columns.contains_key(name) {
            ops.push(col_op(
                next_op_id(),
                ColOpParams {
                    schema: schema.to_string(),
                    table: table.to_string(),
                    column: name.to_string(),
                    kind: PlanOpKind::AddColumn,
                    risk: OperationRisk::Reversible,
                    reason: format!("add column {schema}.{table}.{name}"),
                    details: Some(
                        serde_json::to_string(col)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                },
            ));
        }
    }
    for (name, _col) in &live.columns {
        if !target.columns.contains_key(name) {
            ops.push(col_op(
                next_op_id(),
                ColOpParams {
                    schema: schema.to_string(),
                    table: table.to_string(),
                    column: name.to_string(),
                    kind: PlanOpKind::DropColumn,
                    risk: OperationRisk::Destructive,
                    reason: format!("drop column {schema}.{table}.{name}"),
                    details: None,
                },
            ));
        }
    }
    for (name, tcol) in &target.columns {
        if let Some(_lcol) = live.columns.get(name).filter(|lcol| *lcol != tcol) {
            ops.push(col_op(
                next_op_id(),
                ColOpParams {
                    schema: schema.to_string(),
                    table: table.to_string(),
                    column: name.to_string(),
                    kind: PlanOpKind::AlterColumn,
                    risk: OperationRisk::Destructive,
                    reason: format!("alter column {schema}.{table}.{name}"),
                    details: Some(
                        serde_json::to_string(tcol)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                },
            ));
        }
    }

    // Rows: extract live rows and diff against target rows by identity.
    let live_rows = adapter
        .extract_table_rows(live)
        .await
        .map_err(|e| EngineError::Restore(format!("failed to extract live rows: {e}")))?;
    ops.extend(diff_rows(schema, table, target, &live_rows)?);

    Ok(ops)
}

fn diff_rows(
    schema: &str,
    table: &str,
    target: &PgTable,
    live_rows: &[meld_core::state::PgRow],
) -> Result<Vec<PlanOp>, EngineError> {
    let mut ops: Vec<PlanOp> = Vec::new();

    // Index live rows by identity.
    let mut live_by_id: std::collections::HashMap<PgRowIdentity, &meld_core::state::PgRow> =
        std::collections::HashMap::new();
    for row in live_rows {
        live_by_id.insert(row.identity.clone(), row);
    }

    let mut matched: std::collections::HashSet<PgRowIdentity> = std::collections::HashSet::new();

    for trow in &target.rows {
        match live_by_id.get(&trow.identity) {
            Some(lrow) => {
                matched.insert(trow.identity.clone());
                if trow.values != lrow.values {
                    ops.push(row_op(
                        next_op_id(),
                        PlanOpKind::UpdateData,
                        schema,
                        table,
                        OperationRisk::Destructive,
                        format!("update row in {schema}.{table}"),
                        Some(
                            serde_json::to_string(trow)
                                .map_err(|e| EngineError::Internal(e.to_string()))?,
                        ),
                    ));
                }
            }
            None => {
                ops.push(row_op(
                    next_op_id(),
                    PlanOpKind::InsertData,
                    schema,
                    table,
                    OperationRisk::Reversible,
                    format!("insert row into {schema}.{table}"),
                    Some(
                        serde_json::to_string(trow)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ));
            }
        }
    }

    // Live rows not present in target: delete them.
    for lrow in live_rows {
        if !matched.contains(&lrow.identity) {
            ops.push(row_op(
                next_op_id(),
                PlanOpKind::DeleteData,
                schema,
                table,
                OperationRisk::Destructive,
                format!("delete row from {schema}.{table}"),
                Some(
                    serde_json::to_string(lrow)
                        .map_err(|e| EngineError::Internal(e.to_string()))?,
                ),
            ));
        }
    }

    Ok(ops)
}

fn next_op_id() -> String {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("op-{}", n)
}

fn plan_op(
    id: String,
    kind: PlanOpKind,
    object: meld_core::identity::ObjectIdentity,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object,
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

fn table_op(
    id: String,
    kind: PlanOpKind,
    schema: &str,
    table: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    plan_op(
        id,
        kind,
        meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Table {
                schema: schema.to_string(),
                table: table.to_string(),
            },
        ),
        risk,
        reason,
        details,
    )
}

struct ColOpParams {
    schema: String,
    table: String,
    column: String,
    kind: PlanOpKind,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
}

fn col_op(id: String, params: ColOpParams) -> PlanOp {
    plan_op(
        id,
        params.kind,
        meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Column {
                schema: params.schema,
                table: params.table,
                column: params.column,
            },
        ),
        params.risk,
        params.reason,
        params.details,
    )
}

fn row_op(
    id: String,
    kind: PlanOpKind,
    schema: &str,
    table: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    plan_op(
        id,
        kind,
        meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Table {
                schema: schema.to_string(),
                table: table.to_string(),
            },
        ),
        risk,
        reason,
        details,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_op_id_is_unique() {
        assert_ne!(next_op_id(), next_op_id());
    }
}

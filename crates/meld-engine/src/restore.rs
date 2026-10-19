//! Restore-plan building.
//!
//! Compares a target commit state against the current live database state and
//! produces a dependency-aware, execution-ready plan. The engine owns this
//! logic so the plan can be previewed and confirmed before the adapter
//! executes it.
//!
//! Dependency ordering (via `depends_on`) so that `ordered_ops` executes
//! safely:
//!   * column / constraint / index / row operations on a table depend on the
//!     table's creation when the table itself is being created;
//!   * a table drop depends on dropping its constraints, indexes, columns,
//!     and the views defined over it;
//!   * view creation depends on all tables existing; view drops precede table
//!     drops.
//!
//! MongoDB collection drops depend on dropping the views defined over them.

use meld_adapter::DatabaseAdapter;
use meld_core::plan::{OperationRisk, PlanOp, PlanOpKind, RestorePlan};
use meld_core::state::{
    CanonicalValue, EngineKind, EngineState, MongoCollectionState, MongoDocument, PgDatabase,
    PgRowIdentity, PgTable, PostgresState,
};
use serde_json;

use crate::error::EngineError;

/// Build a dependency-aware restore plan that transforms the live database
/// into the state captured at `target_commit`.
pub async fn build_restore_plan(
    adapter: &dyn DatabaseAdapter,
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
        (EngineState::MongoDb(target_state), EngineState::MongoDb(live_state)) => {
            build_mongodb_plan(target_state, live_state).await?
        }
        (EngineState::PostgreSql(_), EngineState::MongoDb(_)) => {
            return Err(EngineError::IncompatibleEngine {
                expected: EngineKind::PostgreSql,
                got: EngineKind::MongoDb,
            });
        }
        (EngineState::MongoDb(_), EngineState::PostgreSql(_)) => {
            return Err(EngineError::IncompatibleEngine {
                expected: EngineKind::MongoDb,
                got: EngineKind::PostgreSql,
            });
        }
    };

    Ok(RestorePlan::new(target_commit.hash, root_hash, engine_kind).with_ops(ops))
}

// ---------------------------------------------------------------------------
// PostgreSQL restore planning
// ---------------------------------------------------------------------------

/// A build step: a planned op plus the table it touches (for dependency
/// wiring) and a phase hint.
pub(crate) struct PendingOp {
    op: PlanOp,
    /// The schema-qualified table this op touches, when applicable.
    table: Option<(String, String)>,
}

/// Build a PostgreSQL restore plan reconciling the complete captured
/// supported state: tables, columns, constraints, indexes, views, sequences,
/// and rows.
async fn build_postgres_plan(
    adapter: &dyn DatabaseAdapter,
    target: &PostgresState,
    live: &PostgresState,
) -> Result<Vec<PlanOp>, EngineError> {
    let mut pending: Vec<PendingOp> = Vec::new();

    for (db_name, target_db) in &target.databases {
        let live_db = live.databases.get(db_name);
        for (schema_name, target_schema) in &target_db.schemas {
            let live_schema = live_db.and_then(|db| db.schemas.get(schema_name));

            // Views and sequences live on the database; reconcile once per database.
            if let Some(live_db) = live_db {
                diff_views(target_db, live_db, &mut pending)?;
                diff_sequences(target_db, live_db, &mut pending)?;
            }

            for (table_name, target_table) in &target_schema.tables {
                match live_schema.and_then(|s| s.tables.get(table_name)) {
                    None => {
                        pending.push(PendingOp {
                            op: table_op(
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
                            ),
                            table: Some((schema_name.clone(), table_name.clone())),
                        });
                    }
                    Some(live_table) => {
                        pending.extend(diff_table(
                            schema_name,
                            table_name,
                            target_table,
                            live_table,
                        )?);
                        // Row-level reconciliation against freshly extracted
                        // live rows (consistent snapshot).
                        diff_rows_for_plan(
                            adapter,
                            schema_name,
                            table_name,
                            target_table,
                            &mut pending,
                        )
                        .await?;
                    }
                }
            }

            // Tables present live but absent from the target must be dropped.
            if let Some(live_schema) = live_schema {
                for table_name in live_schema.tables.keys() {
                    if !target_schema.tables.contains_key(table_name) {
                        pending.push(PendingOp {
                            op: table_op(
                                next_op_id(),
                                PlanOpKind::DropObject,
                                schema_name,
                                table_name,
                                OperationRisk::Destructive,
                                format!("drop table {schema_name}.{table_name}"),
                                None,
                            ),
                            table: Some((schema_name.clone(), table_name.clone())),
                        });
                    }
                }
            }
        }
    }

    wire_dependencies(&mut pending);
    Ok(pending.into_iter().map(|p| p.op).collect())
}

/// Build plan ops diffing one table's columns, constraints, and indexes.
fn diff_table(
    schema: &str,
    table: &str,
    target: &PgTable,
    live: &PgTable,
) -> Result<Vec<PendingOp>, EngineError> {
    let mut ops: Vec<PendingOp> = Vec::new();
    let tkey = (schema.to_string(), table.to_string());

    // Columns: additions.
    for (name, col) in &target.columns {
        if !live.columns.contains_key(name) {
            ops.push(PendingOp {
                op: col_op(
                    next_op_id(),
                    schema,
                    table,
                    name,
                    PlanOpKind::AddColumn,
                    OperationRisk::Reversible,
                    format!("add column {schema}.{table}.{name}"),
                    Some(
                        serde_json::to_string(col)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    // Columns: removals.
    for (name, _col) in &live.columns {
        if !target.columns.contains_key(name) {
            ops.push(PendingOp {
                op: col_op(
                    next_op_id(),
                    schema,
                    table,
                    name,
                    PlanOpKind::DropColumn,
                    OperationRisk::Destructive,
                    format!("drop column {schema}.{table}.{name}"),
                    None,
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    // Columns: alterations.
    for (name, tcol) in &target.columns {
        if live.columns.get(name).is_some_and(|lcol| lcol != tcol) {
            ops.push(PendingOp {
                op: col_op(
                    next_op_id(),
                    schema,
                    table,
                    name,
                    PlanOpKind::AlterColumn,
                    OperationRisk::Destructive,
                    format!("alter column {schema}.{table}.{name}"),
                    Some(
                        serde_json::to_string(tcol)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: Some(tkey.clone()),
            });
        }
    }

    // Constraints: primary key, unique, foreign key, check.
    diff_constraints(schema, table, target, live, &mut ops)?;
    // Indexes (non-constraint-backed).
    diff_indexes(schema, table, target, live, &mut ops)?;

    Ok(ops)
}

fn diff_constraints(
    schema: &str,
    table: &str,
    target: &PgTable,
    live: &PgTable,
    ops: &mut Vec<PendingOp>,
) -> Result<(), EngineError> {
    let risk = OperationRisk::Destructive;
    let tkey = (schema.to_string(), table.to_string());

    match (&target.primary_key, &live.primary_key) {
        (Some(tp), None) => ops.push(PendingOp {
            op: constraint_op(
                next_op_id(),
                schema,
                table,
                &tp.name,
                PlanOpKind::CreateConstraint,
                risk,
                format!("create primary key constraint {}", tp.name),
                Some(serde_json::to_string(tp).map_err(|e| EngineError::Internal(e.to_string()))?),
            ),
            table: Some(tkey.clone()),
        }),
        (None, Some(lp)) => ops.push(PendingOp {
            op: constraint_op(
                next_op_id(),
                schema,
                table,
                &lp.name,
                PlanOpKind::DropConstraint,
                risk,
                "drop primary key constraint".to_string(),
                None,
            ),
            table: Some(tkey.clone()),
        }),
        (Some(tp), Some(lp)) if tp != lp => alter_constraint(
            schema,
            table,
            &tp.name,
            Some(serde_json::to_string(tp).map_err(|e| EngineError::Internal(e.to_string()))?),
            ops,
            risk,
        ),
        _ => {}
    }

    for tc in &target.unique_constraints {
        if !live.unique_constraints.iter().any(|c| c.name == tc.name) {
            ops.push(PendingOp {
                op: constraint_op(
                    next_op_id(),
                    schema,
                    table,
                    &tc.name,
                    PlanOpKind::CreateConstraint,
                    risk,
                    format!("create unique constraint {}", tc.name),
                    Some(
                        serde_json::to_string(tc)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    for lc in &live.unique_constraints {
        if !target.unique_constraints.iter().any(|c| c.name == lc.name) {
            ops.push(PendingOp {
                op: constraint_op(
                    next_op_id(),
                    schema,
                    table,
                    &lc.name,
                    PlanOpKind::DropConstraint,
                    risk,
                    format!("drop unique constraint {}", lc.name),
                    None,
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    for tc in &target.foreign_keys {
        if !live.foreign_keys.iter().any(|c| c.name == tc.name) {
            ops.push(PendingOp {
                op: constraint_op(
                    next_op_id(),
                    schema,
                    table,
                    &tc.name,
                    PlanOpKind::CreateConstraint,
                    risk,
                    format!("create foreign key constraint {}", tc.name),
                    Some(
                        serde_json::to_string(tc)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    for lc in &live.foreign_keys {
        if !target.foreign_keys.iter().any(|c| c.name == lc.name) {
            ops.push(PendingOp {
                op: constraint_op(
                    next_op_id(),
                    schema,
                    table,
                    &lc.name,
                    PlanOpKind::DropConstraint,
                    risk,
                    format!("drop foreign key constraint {}", lc.name),
                    None,
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    for tc in &target.check_constraints {
        if !live.check_constraints.iter().any(|c| c.name == tc.name) {
            ops.push(PendingOp {
                op: constraint_op(
                    next_op_id(),
                    schema,
                    table,
                    &tc.name,
                    PlanOpKind::CreateConstraint,
                    risk,
                    format!("create check constraint {}", tc.name),
                    Some(
                        serde_json::to_string(tc)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    for lc in &live.check_constraints {
        if !target.check_constraints.iter().any(|c| c.name == lc.name) {
            ops.push(PendingOp {
                op: constraint_op(
                    next_op_id(),
                    schema,
                    table,
                    &lc.name,
                    PlanOpKind::DropConstraint,
                    risk,
                    format!("drop check constraint {}", lc.name),
                    None,
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    Ok(())
}

/// Altering a constraint means dropping and recreating it.
fn alter_constraint(
    schema: &str,
    table: &str,
    constraint: &str,
    details: Option<String>,
    ops: &mut Vec<PendingOp>,
    risk: OperationRisk,
) {
    let tkey = (schema.to_string(), table.to_string());
    ops.push(PendingOp {
        op: constraint_op(
            next_op_id(),
            schema,
            table,
            constraint,
            PlanOpKind::DropConstraint,
            risk,
            format!("drop constraint {constraint} before recreate"),
            None,
        ),
        table: Some(tkey.clone()),
    });
    ops.push(PendingOp {
        op: constraint_op(
            next_op_id(),
            schema,
            table,
            constraint,
            PlanOpKind::CreateConstraint,
            risk,
            format!("recreate constraint {constraint}"),
            details,
        ),
        table: Some(tkey),
    });
}

fn diff_indexes(
    schema: &str,
    table: &str,
    target: &PgTable,
    live: &PgTable,
    ops: &mut Vec<PendingOp>,
) -> Result<(), EngineError> {
    let risk = OperationRisk::Reversible;
    let tkey = (schema.to_string(), table.to_string());
    for (name, ti) in &target.indexes {
        if ti.is_primary || ti.is_unique {
            // Constraint-backed indexes are managed through constraints.
            continue;
        }
        match live.indexes.get(name) {
            None => ops.push(PendingOp {
                op: index_op(
                    next_op_id(),
                    schema,
                    table,
                    name,
                    PlanOpKind::CreateIndex,
                    risk,
                    format!("create index {name} on {schema}.{table}"),
                    Some(
                        serde_json::to_string(ti)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: Some(tkey.clone()),
            }),
            Some(li) if li != ti => ops.push(PendingOp {
                op: index_op(
                    next_op_id(),
                    schema,
                    table,
                    name,
                    PlanOpKind::AlterIndex,
                    risk,
                    format!("alter index {name} on {schema}.{table}"),
                    Some(
                        serde_json::to_string(ti)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: Some(tkey.clone()),
            }),
            _ => {}
        }
    }
    for name in live.indexes.keys() {
        if name == "_" {
            continue;
        }
        let skip = live
            .indexes
            .get(name)
            .is_some_and(|i| i.is_primary || i.is_unique);
        if skip {
            continue;
        }
        if !target.indexes.contains_key(name) {
            ops.push(PendingOp {
                op: index_op(
                    next_op_id(),
                    schema,
                    table,
                    name,
                    PlanOpKind::DropIndex,
                    OperationRisk::Destructive,
                    format!("drop index {name} on {schema}.{table}"),
                    None,
                ),
                table: Some(tkey.clone()),
            });
        }
    }
    Ok(())
}

fn diff_views(
    target_db: &PgDatabase,
    live_db: &PgDatabase,
    pending: &mut Vec<PendingOp>,
) -> Result<(), EngineError> {
    let risk = OperationRisk::Reversible;
    for (qualified, tv) in &target_db.views {
        let (schema, name) = qualified
            .split_once('.')
            .unwrap_or((qualified.as_str(), ""));
        match live_db.views.get(qualified) {
            None => pending.push(PendingOp {
                op: view_op(
                    next_op_id(),
                    schema,
                    name,
                    PlanOpKind::CreateObject,
                    risk,
                    format!("create view {schema}.{name}"),
                    Some(
                        serde_json::to_string(tv)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: None,
            }),
            Some(lv) if tv != lv => pending.push(PendingOp {
                op: view_op(
                    next_op_id(),
                    schema,
                    name,
                    PlanOpKind::AlterObject,
                    risk,
                    format!("alter view {schema}.{name}"),
                    Some(
                        serde_json::to_string(tv)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: None,
            }),
            _ => {}
        }
    }
    for qualified in live_db.views.keys() {
        if !target_db.views.contains_key(qualified.as_str()) {
            let (schema, name) = qualified
                .split_once('.')
                .unwrap_or((qualified.as_str(), ""));
            pending.push(PendingOp {
                op: view_op(
                    next_op_id(),
                    schema,
                    name,
                    PlanOpKind::DropObject,
                    OperationRisk::Destructive,
                    format!("drop view {schema}.{name}"),
                    None,
                ),
                table: None,
            });
        }
    }
    Ok(())
}

fn diff_sequences(
    target_db: &PgDatabase,
    live_db: &PgDatabase,
    pending: &mut Vec<PendingOp>,
) -> Result<(), EngineError> {
    let risk = OperationRisk::Reversible;
    for (qualified, ts) in &target_db.sequences {
        let (schema, name) = qualified
            .split_once('.')
            .unwrap_or((qualified.as_str(), ""));
        match live_db.sequences.get(qualified) {
            None => pending.push(PendingOp {
                op: sequence_op(
                    next_op_id(),
                    schema,
                    name,
                    PlanOpKind::CreateObject,
                    risk,
                    format!("create sequence {schema}.{name}"),
                    Some(
                        serde_json::to_string(ts)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: None,
            }),
            Some(ls) if ts != ls => pending.push(PendingOp {
                op: sequence_op(
                    next_op_id(),
                    schema,
                    name,
                    PlanOpKind::AlterObject,
                    risk,
                    format!("alter sequence {schema}.{name}"),
                    Some(
                        serde_json::to_string(ts)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: None,
            }),
            _ => {}
        }
    }
    for qualified in live_db.sequences.keys() {
        if !target_db.sequences.contains_key(qualified.as_str()) {
            let (schema, name) = qualified
                .split_once('.')
                .unwrap_or((qualified.as_str(), ""));
            pending.push(PendingOp {
                op: sequence_op(
                    next_op_id(),
                    schema,
                    name,
                    PlanOpKind::DropObject,
                    OperationRisk::Destructive,
                    format!("drop sequence {schema}.{name}"),
                    None,
                ),
                table: None,
            });
        }
    }
    Ok(())
}

/// Add row-level operations. Requires the live rows (freshly extracted inside
/// a consistent transaction) so inserts/updates/deletes target real rows.
///
/// Refuses row-level reconciliation for tables without a primary key: row
/// identity cannot be determined reliably without a PK, so we never guess.
pub(crate) async fn diff_rows_for_plan(
    adapter: &dyn DatabaseAdapter,
    schema: &str,
    table: &str,
    target: &PgTable,
    ops: &mut Vec<PendingOp>,
) -> Result<(), EngineError> {
    // Row-level diff/restore requires a primary key for stable row identity.
    // Tables without a PK are skipped for row operations (schema-level ops
    // still apply).
    let Some(pk) = &target.primary_key else {
        return Ok(());
    };
    let pk_cols: Vec<&str> = pk.columns.iter().map(String::as_str).collect();

    let live_rows = adapter
        .extract_table_rows(target)
        .await
        .map_err(|e| EngineError::Restore(format!("failed to extract live rows: {e}")))?;

    let mut live_by_id: std::collections::HashMap<PgRowIdentity, &meld_core::state::PgRow> =
        std::collections::HashMap::new();
    for row in &live_rows {
        live_by_id.insert(row.identity.clone(), row);
    }
    let mut matched: std::collections::HashSet<PgRowIdentity> = std::collections::HashSet::new();

    let tkey = (schema.to_string(), table.to_string());
    let risk = OperationRisk::Destructive;

    for trow in &target.rows {
        let PgRowIdentity::PrimaryKey(trow_pk_vals) = &trow.identity else {
            return Err(EngineError::Restore(format!(
                "target row in {schema}.{table} lacks primary-key identity"
            )));
        };
        match live_by_id.get(&trow.identity) {
            Some(lrow) => {
                matched.insert(trow.identity.clone());
                if trow.values != lrow.values {
                    let identity_vals: Vec<CanonicalValue> =
                        trow_pk_vals.iter().map(|(_, v)| v.clone()).collect();
                    ops.push(PendingOp {
                        op: row_op(
                            next_op_id(),
                            PlanOpKind::UpdateData,
                            schema,
                            table,
                            &pk_cols,
                            identity_vals,
                            risk,
                            format!("update row in {schema}.{table}"),
                            Some(
                                serde_json::to_string(trow)
                                    .map_err(|e| EngineError::Internal(e.to_string()))?,
                            ),
                        ),
                        table: Some(tkey.clone()),
                    });
                }
            }
            None => {
                let identity_vals: Vec<CanonicalValue> =
                    trow_pk_vals.iter().map(|(_, v)| v.clone()).collect();
                ops.push(PendingOp {
                    op: row_op(
                        next_op_id(),
                        PlanOpKind::InsertData,
                        schema,
                        table,
                        &pk_cols,
                        identity_vals,
                        OperationRisk::Reversible,
                        format!("insert row into {schema}.{table}"),
                        Some(
                            serde_json::to_string(trow)
                                .map_err(|e| EngineError::Internal(e.to_string()))?,
                        ),
                    ),
                    table: Some(tkey.clone()),
                });
            }
        }
    }

    for lrow in live_rows {
        if !matched.contains(&lrow.identity) {
            let PgRowIdentity::PrimaryKey(lrow_pk_vals) = &lrow.identity else {
                return Err(EngineError::Restore(format!(
                    "live row in {schema}.{table} lacks primary-key identity"
                )));
            };
            let identity_vals: Vec<CanonicalValue> =
                lrow_pk_vals.iter().map(|(_, v)| v.clone()).collect();
            ops.push(PendingOp {
                op: row_op(
                    next_op_id(),
                    PlanOpKind::DeleteData,
                    schema,
                    table,
                    &pk_cols,
                    identity_vals,
                    risk,
                    format!("delete row from {schema}.{table}"),
                    Some(
                        serde_json::to_string(&lrow)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ),
                table: Some(tkey.clone()),
            });
        }
    }

    Ok(())
}

/// Wire `depends_on` between ops so `ordered_ops` produces a safe order.
fn wire_dependencies(pending: &mut [PendingOp]) {
    let create_table: Vec<(String, String, String)> = pending
        .iter()
        .filter_map(|p| {
            let (PlanOpKind::CreateObject, Some((schema, table))) = (&p.op.kind, &p.table) else {
                return None;
            };
            Some((schema.clone(), table.clone(), p.op.id.clone()))
        })
        .collect();
    let view_drop_ids: Vec<String> = pending
        .iter()
        .filter_map(|p| {
            let is_view_drop = matches!(p.op.kind, PlanOpKind::DropObject)
                && matches!(
                    &p.op.object,
                    meld_core::identity::ObjectIdentity::PostgreSql(
                        meld_core::identity::PostgresObjectId::View { .. }
                    )
                );
            is_view_drop.then_some(p.op.id.clone())
        })
        .collect();

    // Compute all dependencies first (immutable borrows only), then apply.
    let mut new_deps: Vec<Vec<String>> = vec![Vec::new(); pending.len()];
    for (i, p) in pending.iter().enumerate() {
        let (kind, table) = (&p.op.kind, &p.table);

        match (kind, table) {
            // Columns/constraints/indexes/rows need the table to exist first.
            (PlanOpKind::AddColumn | PlanOpKind::DropColumn | PlanOpKind::AlterColumn, Some(t))
            | (PlanOpKind::CreateConstraint | PlanOpKind::DropConstraint, Some(t))
            | (PlanOpKind::CreateIndex | PlanOpKind::DropIndex | PlanOpKind::AlterIndex, Some(t))
            | (PlanOpKind::InsertData | PlanOpKind::UpdateData | PlanOpKind::DeleteData, Some(t))
                if create_table
                    .iter()
                    .any(|(s, t2, _)| s == &t.0 && t2 == &t.1) =>
            {
                let id = create_table
                    .iter()
                    .find(|(s, t2, _)| s == &t.0 && t2 == &t.1)
                    .unwrap()
                    .2
                    .clone();
                new_deps[i].push(id);
            }
            // Row inserts after added columns.
            (PlanOpKind::InsertData, Some(t)) => {
                for p2 in pending.iter() {
                    let (PlanOpKind::AddColumn, Some(t2)) = (&p2.op.kind, &p2.table) else {
                        continue;
                    };
                    if t2 == t {
                        new_deps[i].push(p2.op.id.clone());
                    }
                }
            }
            _ => {}
        }

        // Table drops need constraints, indexes, columns, and views gone first.
        if let (PlanOpKind::DropObject, Some((schema, table))) = (kind, table) {
            for p2 in pending.iter() {
                let same_table = p2
                    .table
                    .as_ref()
                    .is_some_and(|t2| t2.0 == schema.as_str() && t2.1 == table.as_str());
                let is_drop = matches!(
                    p2.op.kind,
                    PlanOpKind::DropConstraint | PlanOpKind::DropIndex | PlanOpKind::DropColumn
                );
                let is_view_drop = matches!(
                    &p2.op.object,
                    meld_core::identity::ObjectIdentity::PostgreSql(
                        meld_core::identity::PostgresObjectId::View { .. }
                    )
                ) && matches!(p2.op.kind, PlanOpKind::DropObject);
                if same_table && (is_drop || is_view_drop) {
                    new_deps[i].push(p2.op.id.clone());
                }
            }
            // Any view drop precedes any table drop.
            new_deps[i].extend(view_drop_ids.clone());
        }

        // View creation needs all tables to exist first.
        if let (
            meld_core::identity::ObjectIdentity::PostgreSql(
                meld_core::identity::PostgresObjectId::View { .. },
            ),
            PlanOpKind::CreateObject,
        ) = (&p.op.object, kind)
        {
            for (_, _, id) in &create_table {
                new_deps[i].push(id.clone());
            }
        }
    }

    // Apply computed dependencies and de-duplicate.
    for (p, deps) in pending.iter_mut().zip(new_deps) {
        p.op.depends_on.extend(deps);
        p.op.depends_on.sort();
        p.op.depends_on.dedup();
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
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Table {
                schema: schema.to_string(),
                table: table.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

#[allow(clippy::too_many_arguments)]
fn col_op(
    id: String,
    schema: &str,
    table: &str,
    column: &str,
    kind: PlanOpKind,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Column {
                schema: schema.to_string(),
                table: table.to_string(),
                column: column.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

#[allow(clippy::too_many_arguments)]
fn constraint_op(
    id: String,
    schema: &str,
    table: &str,
    constraint: &str,
    kind: PlanOpKind,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Constraint {
                schema: schema.to_string(),
                table: table.to_string(),
                constraint: constraint.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

#[allow(clippy::too_many_arguments)]
fn index_op(
    id: String,
    schema: &str,
    table: &str,
    index: &str,
    kind: PlanOpKind,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Index {
                schema: schema.to_string(),
                table: table.to_string(),
                index: index.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

fn view_op(
    id: String,
    schema: &str,
    view: &str,
    kind: PlanOpKind,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::View {
                schema: schema.to_string(),
                view: view.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

fn sequence_op(
    id: String,
    schema: &str,
    sequence: &str,
    kind: PlanOpKind,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Sequence {
                schema: schema.to_string(),
                sequence: sequence.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

#[allow(clippy::too_many_arguments)]
fn row_op(
    id: String,
    kind: PlanOpKind,
    schema: &str,
    table: &str,
    _pk_columns: &[&str],
    identity: Vec<CanonicalValue>,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::PostgreSql(
            meld_core::identity::PostgresObjectId::Row {
                schema: schema.to_string(),
                table: table.to_string(),
                identity,
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

fn next_op_id() -> String {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("op-{n}")
}

// ---------------------------------------------------------------------------
// MongoDB restore planning
// ---------------------------------------------------------------------------

async fn build_mongodb_plan(
    target: &meld_core::state::MongoState,
    live: &meld_core::state::MongoState,
) -> Result<Vec<PlanOp>, EngineError> {
    let mut ops: Vec<PlanOp> = Vec::new();

    // Collections in target but not in live -> create
    for (coll_name, target_coll) in &target.collections {
        match live.collections.get(coll_name) {
            None => {
                ops.push(collection_op(
                    next_op_id(),
                    PlanOpKind::CreateCollection,
                    coll_name,
                    OperationRisk::Reversible,
                    format!("create collection {coll_name}"),
                    Some(
                        serde_json::to_string(target_coll)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ));
            }
            Some(live_coll) => {
                ops.extend(diff_collection(coll_name, target_coll, live_coll)?);
            }
        }
    }

    // Collections in live but not in target -> drop (views over them first).
    for coll_name in live.collections.keys() {
        if !target.collections.contains_key(coll_name) {
            // Drop views defined over this collection before the collection.
            for (view_name, view_on) in view_ons(target) {
                if view_on.as_str() == coll_name {
                    ops.push(view_op_mongo(
                        next_op_id(),
                        &view_name,
                        PlanOpKind::DropObject,
                        OperationRisk::Destructive,
                        format!("drop view {view_name}"),
                        None,
                    ));
                }
            }
            ops.push(collection_op(
                next_op_id(),
                PlanOpKind::DropCollection,
                coll_name,
                OperationRisk::Destructive,
                format!("drop collection {coll_name}"),
                None,
            ));
        }
    }

    Ok(ops)
}

/// Views of target collections as (view_name, view_on) pairs.
fn view_ons(target: &meld_core::state::MongoState) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for coll in target.collections.values() {
        for v in &coll.views {
            out.push((v.name.clone(), v.view_on.clone()));
        }
    }
    out
}

fn diff_collection(
    coll_name: &str,
    target: &MongoCollectionState,
    live: &MongoCollectionState,
) -> Result<Vec<PlanOp>, EngineError> {
    let mut ops: Vec<PlanOp> = Vec::new();

    if target.options != live.options {
        ops.push(collection_op(
            next_op_id(),
            PlanOpKind::AlterCollection,
            coll_name,
            OperationRisk::Reversible,
            format!("alter collection options {coll_name}"),
            Some(serde_json::to_string(target).map_err(|e| EngineError::Internal(e.to_string()))?),
        ));
    }

    match (&target.validator, &live.validator) {
        (Some(tval), None) => ops.push(validator_op(
            next_op_id(),
            coll_name,
            OperationRisk::Reversible,
            format!("add validator {coll_name}"),
            Some(serde_json::to_string(tval).map_err(|e| EngineError::Internal(e.to_string()))?),
        )),
        (Some(tval), Some(lval)) if tval != lval => ops.push(validator_op(
            next_op_id(),
            coll_name,
            OperationRisk::Reversible,
            format!("change validator {coll_name}"),
            Some(serde_json::to_string(tval).map_err(|e| EngineError::Internal(e.to_string()))?),
        )),
        (None, Some(_)) => ops.push(validator_op(
            next_op_id(),
            coll_name,
            OperationRisk::Destructive,
            format!("remove validator {coll_name}"),
            None,
        )),
        _ => {}
    }

    // Views defined over this collection.
    for tv in &target.views {
        if tv.view_on != coll_name {
            continue;
        }
        match live.views.iter().find(|lv| lv.name == tv.name) {
            None => ops.push(view_op_mongo(
                next_op_id(),
                &tv.name,
                PlanOpKind::CreateObject,
                OperationRisk::Reversible,
                format!("create view {} on {coll_name}", tv.name),
                Some(serde_json::to_string(tv).map_err(|e| EngineError::Internal(e.to_string()))?),
            )),
            Some(lv) if lv != tv => ops.push(view_op_mongo(
                next_op_id(),
                &tv.name,
                PlanOpKind::AlterObject,
                OperationRisk::Reversible,
                format!("alter view {} on {coll_name}", tv.name),
                Some(serde_json::to_string(tv).map_err(|e| EngineError::Internal(e.to_string()))?),
            )),
            _ => {}
        }
    }
    for lv in &live.views {
        if lv.view_on != coll_name {
            continue;
        }
        if !target.views.iter().any(|tv| tv.name == lv.name) {
            ops.push(view_op_mongo(
                next_op_id(),
                &lv.name,
                PlanOpKind::DropObject,
                OperationRisk::Destructive,
                format!("drop view {} on {coll_name}", lv.name),
                None,
            ));
        }
    }

    // Indexes.
    for (idx_name, target_idx) in &target.indexes {
        if idx_name == "_id_" {
            continue;
        }
        match live.indexes.get(idx_name) {
            None => ops.push(index_op_mongo(
                next_op_id(),
                coll_name,
                idx_name,
                PlanOpKind::CreateIndex,
                OperationRisk::Reversible,
                format!("create index {idx_name} on {coll_name}"),
                Some(
                    serde_json::to_string(target_idx)
                        .map_err(|e| EngineError::Internal(e.to_string()))?,
                ),
            )),
            Some(live_idx) if live_idx != target_idx => ops.push(index_op_mongo(
                next_op_id(),
                coll_name,
                idx_name,
                PlanOpKind::AlterIndex,
                OperationRisk::Destructive,
                format!("alter index {idx_name} on {coll_name}"),
                Some(
                    serde_json::to_string(target_idx)
                        .map_err(|e| EngineError::Internal(e.to_string()))?,
                ),
            )),
            _ => {}
        }
    }
    for idx_name in live.indexes.keys() {
        if idx_name == "_id_" {
            continue;
        }
        if !target.indexes.contains_key(idx_name) {
            ops.push(index_op_mongo(
                next_op_id(),
                coll_name,
                idx_name,
                PlanOpKind::DropIndex,
                OperationRisk::Destructive,
                format!("drop index {idx_name} on {coll_name}"),
                None,
            ));
        }
    }

    // Documents: diff by _id.
    ops.extend(diff_documents(coll_name, target, live)?);

    Ok(ops)
}

fn view_op_mongo(
    id: String,
    view: &str,
    kind: PlanOpKind,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::MongoDb(
            meld_core::identity::MongoObjectId::View {
                name: view.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

fn index_op_mongo(
    id: String,
    collection: &str,
    index: &str,
    kind: PlanOpKind,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::MongoDb(
            meld_core::identity::MongoObjectId::Index {
                collection: collection.to_string(),
                index: index.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

fn diff_documents(
    coll_name: &str,
    target: &MongoCollectionState,
    live: &MongoCollectionState,
) -> Result<Vec<PlanOp>, EngineError> {
    let mut ops: Vec<PlanOp> = Vec::new();

    let mut live_by_id: std::collections::HashMap<CanonicalValue, &MongoDocument> =
        std::collections::HashMap::new();
    for doc in &live.documents {
        live_by_id.insert(doc.id.clone(), doc);
    }

    let mut matched: std::collections::HashSet<CanonicalValue> = std::collections::HashSet::new();

    for tdoc in &target.documents {
        match live_by_id.get(&tdoc.id) {
            Some(ldoc) => {
                matched.insert(tdoc.id.clone());
                if tdoc.fields != ldoc.fields {
                    ops.push(document_op(
                        next_op_id(),
                        PlanOpKind::UpdateDocument,
                        coll_name,
                        OperationRisk::Destructive,
                        format!("update document in {coll_name}"),
                        Some(
                            serde_json::to_string(tdoc)
                                .map_err(|e| EngineError::Internal(e.to_string()))?,
                        ),
                    ));
                }
            }
            None => {
                ops.push(document_op(
                    next_op_id(),
                    PlanOpKind::InsertDocument,
                    coll_name,
                    OperationRisk::Reversible,
                    format!("insert document into {coll_name}"),
                    Some(
                        serde_json::to_string(tdoc)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ));
            }
        }
    }

    for ldoc in &live.documents {
        if !matched.contains(&ldoc.id) {
            ops.push(document_op(
                next_op_id(),
                PlanOpKind::DeleteDocument,
                coll_name,
                OperationRisk::Destructive,
                format!("delete document from {coll_name}"),
                Some(
                    serde_json::to_string(ldoc)
                        .map_err(|e| EngineError::Internal(e.to_string()))?,
                ),
            ));
        }
    }

    Ok(ops)
}

fn collection_op(
    id: String,
    kind: PlanOpKind,
    collection: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::MongoDb(
            meld_core::identity::MongoObjectId::Collection {
                name: collection.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

fn document_op(
    id: String,
    kind: PlanOpKind,
    collection: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind,
        object: meld_core::identity::ObjectIdentity::MongoDb(
            meld_core::identity::MongoObjectId::Collection {
                name: collection.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

fn validator_op(
    id: String,
    collection: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    PlanOp {
        id,
        kind: PlanOpKind::ChangeValidator,
        object: meld_core::identity::ObjectIdentity::MongoDb(
            meld_core::identity::MongoObjectId::Validator {
                collection: collection.to_string(),
            },
        ),
        risk,
        depends_on: Vec::new(),
        reason,
        requires_confirmation: risk.is_destructive(),
        details,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_op_ids_are_unique() {
        assert_ne!(next_op_id(), next_op_id());
    }
}

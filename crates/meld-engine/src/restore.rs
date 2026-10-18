//! Restore-plan building.
//!
//! Compares a target commit state against the current live database state and
//! produces a dependency-aware, execution-ready plan. The engine owns this
//! logic so the plan can be previewed and confirmed before the adapter
//! executes it.

use meld_adapter::DatabaseAdapter;
use meld_core::plan::{OperationRisk, PlanOp, PlanOpKind, RestorePlan};
use meld_core::state::{
    CanonicalValue, EngineKind, EngineState, MongoCollectionState, MongoDocument, MongoState,
    PgRowIdentity, PgTable, PostgresState,
};
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

// ---------------------------------------------------------------------------
// MongoDB restore plan building
// ---------------------------------------------------------------------------

/// Build a MongoDB restore plan by diffing the target state against the
/// live state, collection by collection.
async fn build_mongodb_plan(
    target: &MongoState,
    live: &MongoState,
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

    // Collections in live but not in target -> drop
    for coll_name in live.collections.keys() {
        if !target.collections.contains_key(coll_name) {
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

/// Build plan ops diffing one collection (options, validator, indexes, documents).
fn diff_collection(
    coll_name: &str,
    target: &MongoCollectionState,
    live: &MongoCollectionState,
) -> Result<Vec<PlanOp>, EngineError> {
    let mut ops: Vec<PlanOp> = Vec::new();

    // Collection options (capped, size, max, TTL).
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

    // Validator.
    match (&target.validator, &live.validator) {
        (Some(tval), None) => {
            ops.push(validator_op(
                next_op_id(),
                coll_name,
                OperationRisk::Reversible,
                format!("add validator {coll_name}"),
                Some(
                    serde_json::to_string(tval)
                        .map_err(|e| EngineError::Internal(e.to_string()))?,
                ),
            ));
        }
        (Some(tval), Some(lval)) if tval != lval => {
            ops.push(validator_op(
                next_op_id(),
                coll_name,
                OperationRisk::Reversible,
                format!("change validator {coll_name}"),
                Some(
                    serde_json::to_string(tval)
                        .map_err(|e| EngineError::Internal(e.to_string()))?,
                ),
            ));
        }
        (None, Some(_)) => {
            ops.push(validator_op(
                next_op_id(),
                coll_name,
                OperationRisk::Destructive,
                format!("remove validator {coll_name}"),
                None,
            ));
        }
        _ => {}
    }

    // Indexes.
    for (idx_name, target_idx) in &target.indexes {
        if idx_name == "_id_" {
            continue; // implicit index, never managed
        }
        match live.indexes.get(idx_name) {
            None => {
                ops.push(index_op(
                    next_op_id(),
                    PlanOpKind::CreateIndex,
                    coll_name,
                    idx_name,
                    OperationRisk::Reversible,
                    format!("create index {idx_name} on {coll_name}"),
                    Some(
                        serde_json::to_string(target_idx)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ));
            }
            Some(live_idx) if live_idx != target_idx => {
                ops.push(index_op(
                    next_op_id(),
                    PlanOpKind::AlterIndex,
                    coll_name,
                    idx_name,
                    OperationRisk::Destructive,
                    format!("alter index {idx_name} on {coll_name}"),
                    Some(
                        serde_json::to_string(target_idx)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                    ),
                ));
            }
            _ => {}
        }
    }
    for idx_name in live.indexes.keys() {
        if idx_name == "_id_" {
            continue;
        }
        if !target.indexes.contains_key(idx_name) {
            ops.push(index_op(
                next_op_id(),
                PlanOpKind::DropIndex,
                coll_name,
                idx_name,
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

fn diff_documents(
    coll_name: &str,
    target: &MongoCollectionState,
    live: &MongoCollectionState,
) -> Result<Vec<PlanOp>, EngineError> {
    let mut ops: Vec<PlanOp> = Vec::new();

    // Index live documents by _id.
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

    // Live documents not present in target: delete them.
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

// ---------------------------------------------------------------------------
// MongoDB plan op helpers
// ---------------------------------------------------------------------------

fn collection_op(
    id: String,
    kind: PlanOpKind,
    collection: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    plan_op(
        id,
        kind,
        meld_core::identity::ObjectIdentity::MongoDb(
            meld_core::identity::MongoObjectId::Collection {
                name: collection.to_string(),
            },
        ),
        risk,
        reason,
        details,
    )
}

fn index_op(
    id: String,
    kind: PlanOpKind,
    collection: &str,
    index: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    plan_op(
        id,
        kind,
        meld_core::identity::ObjectIdentity::MongoDb(meld_core::identity::MongoObjectId::Index {
            collection: collection.to_string(),
            index: index.to_string(),
        }),
        risk,
        reason,
        details,
    )
}

fn validator_op(
    id: String,
    collection: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    plan_op(
        id,
        PlanOpKind::ChangeValidator,
        meld_core::identity::ObjectIdentity::MongoDb(
            meld_core::identity::MongoObjectId::Validator {
                collection: collection.to_string(),
            },
        ),
        risk,
        reason,
        details,
    )
}

fn document_op(
    id: String,
    kind: PlanOpKind,
    collection: &str,
    risk: OperationRisk,
    reason: String,
    details: Option<String>,
) -> PlanOp {
    plan_op(
        id,
        kind,
        meld_core::identity::ObjectIdentity::MongoDb(
            meld_core::identity::MongoObjectId::Collection {
                name: collection.to_string(),
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

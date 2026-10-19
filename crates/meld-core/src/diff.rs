//! Semantic diff between two snapshot states.
//!
//! Produces typed `ChangeOp` operations rather than relying on raw
//! serialized-state comparison.

use crate::canonical::hash_state;
use crate::error::MeldError;
use crate::identity::{MongoObjectId, ObjectIdentity, PostgresObjectId};
use crate::ids::ObjectHash;
use crate::state::{
    CanonicalValue, CaptureInfo, EngineKind, EngineState, MongoCollectionState, MongoDocument,
    MongoIndex, MongoView, PgColumn, PgConstraint, PgIndex, PgTable,
};
use crate::transition::{ChangeKind, ChangeOp, ChangeSummary};
use indexmap::IndexMap;

/// The result of diffing two snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SnapshotDiff {
    pub from_root: ObjectHash,
    pub to_root: ObjectHash,
    pub engine_kind: EngineKind,
    pub ops: Vec<ChangeOp>,
    pub summary: ChangeSummary,
    pub from_info: Option<CaptureInfo>,
    pub to_info: Option<CaptureInfo>,
}

impl SnapshotDiff {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Diff two engine states. Returns an error if the engines or scopes are
/// incompatible.
pub fn diff_states(from: &EngineState, to: &EngineState) -> Result<SnapshotDiff, MeldError> {
    if from.engine_kind() != to.engine_kind() {
        return Err(MeldError::IncompatibleEngine {
            expected: from.engine_kind(),
            got: to.engine_kind(),
        });
    }
    if !from.scope().compatible_with(&to.scope()) {
        return Err(MeldError::IncompatibleScope(format!(
            "scope mismatch: {:?} vs {:?}",
            from.scope(),
            to.scope()
        )));
    }

    let engine_kind = from.engine_kind();
    let ops = match engine_kind {
        EngineKind::PostgreSql => diff_postgres(from, to)?,
        EngineKind::MongoDb => diff_mongodb(from, to)?,
    };
    let summary = ChangeSummary::from_ops(&ops);

    Ok(SnapshotDiff {
        from_root: state_hash(from),
        to_root: state_hash(to),
        engine_kind,
        ops,
        summary,
        from_info: capture_info(from),
        to_info: capture_info(to),
    })
}

fn state_hash(state: &EngineState) -> ObjectHash {
    ObjectHash::from_bytes(hash_state(state).as_bytes())
}

fn capture_info(state: &EngineState) -> Option<CaptureInfo> {
    match state {
        EngineState::PostgreSql(s) => Some(s.capture_info.clone()),
        EngineState::MongoDb(s) => Some(s.capture_info.clone()),
    }
}

// ---------------------------------------------------------------------------
// PostgreSQL diff
// ---------------------------------------------------------------------------

fn diff_postgres(from: &EngineState, to: &EngineState) -> Result<Vec<ChangeOp>, MeldError> {
    let EngineState::PostgreSql(fs) = from else {
        unreachable!()
    };
    let EngineState::PostgreSql(ts) = to else {
        unreachable!()
    };

    let mut ops: Vec<ChangeOp> = vec![];

    // Databases / schemas.
    for (name, fdb) in &fs.databases {
        match ts.databases.get(name) {
            None => {
                ops.push(drop_db(name));
            }
            Some(tdb) => {
                ops.extend(diff_database(fdb, tdb)?);
            }
        }
    }
    for name in ts.databases.keys() {
        if !fs.databases.contains_key(name) {
            ops.push(create_db(name));
        }
    }

    Ok(ops)
}

fn diff_database(
    from: &crate::state::PgDatabase,
    to: &crate::state::PgDatabase,
) -> Result<Vec<ChangeOp>, MeldError> {
    let mut ops: Vec<ChangeOp> = vec![];

    for (schema_name, fschema) in &from.schemas {
        match to.schemas.get(schema_name) {
            None => ops.push(drop_schema(schema_name)),
            Some(tschema) => ops.extend(diff_schema(fschema, tschema)?),
        }
    }
    for schema_name in to.schemas.keys() {
        if !from.schemas.contains_key(schema_name) {
            ops.push(create_schema(schema_name));
        }
    }

    Ok(ops)
}

fn diff_schema(
    from: &crate::state::PgSchema,
    to: &crate::state::PgSchema,
) -> Result<Vec<ChangeOp>, MeldError> {
    let mut ops: Vec<ChangeOp> = vec![];

    for (table_name, ftable) in &from.tables {
        match to.tables.get(table_name) {
            None => ops.push(drop_table(
                from.name.as_str(),
                schema_table(from.name.as_str(), table_name),
            )),
            Some(ttable) => ops.extend(diff_table(ftable, ttable)?),
        }
    }
    for table_name in to.tables.keys() {
        if !from.tables.contains_key(table_name) {
            ops.push(create_table(
                from.name.as_str(),
                schema_table(from.name.as_str(), table_name),
            ));
        }
    }

    Ok(ops)
}

fn schema_table(schema: &str, table: &str) -> PostgresObjectId {
    PostgresObjectId::Table {
        schema: schema.to_string(),
        table: table.to_string(),
    }
}

fn diff_table(from: &PgTable, to: &PgTable) -> Result<Vec<ChangeOp>, MeldError> {
    let mut ops: Vec<ChangeOp> = vec![];
    let _object = ObjectIdentity::PostgreSql(PostgresObjectId::Table {
        schema: from.schema.clone(),
        table: from.name.clone(),
    });

    // Columns: compare by ordinal order and name.
    // Detect dropped columns.
    for (name, col) in &from.columns {
        if !to.columns.contains_key(name) {
            ops.push(ChangeOp {
                kind: ChangeKind::DropColumn,
                object: ObjectIdentity::PostgreSql(PostgresObjectId::Column {
                    schema: from.schema.clone(),
                    table: from.name.clone(),
                    column: name.clone(),
                }),
                field: Some(name.clone()),
                confidence: 1000,
                before: Some(CanonicalValue::Text(format!("{:?}", col.data_type))),
                after: None,
                reason: format!("column {name} dropped"),
            });
        }
    }
    // Detect added columns.
    for (name, col) in &to.columns {
        if !from.columns.contains_key(name) {
            ops.push(ChangeOp {
                kind: ChangeKind::AddColumn,
                object: ObjectIdentity::PostgreSql(PostgresObjectId::Column {
                    schema: from.schema.clone(),
                    table: from.name.clone(),
                    column: name.clone(),
                }),
                field: Some(name.clone()),
                confidence: 1000,
                before: None,
                after: Some(CanonicalValue::Text(format!("{:?}", col.data_type))),
                reason: format!("column {name} added"),
            });
        }
    }
    // Detect altered / renamed columns.
    for (name, fcol) in &from.columns {
        if let Some(tcol) = to.columns.get(name) {
            if fcol != tcol {
                ops.push(alter_column_op(from, name, fcol, tcol));
            }
        } else {
            // Column present in from but not to: handled by drop above only if
            // not re-appearing under a new name (rename detection).
        }
    }

    // Simple rename detection: a dropped column and an added column with the
    // same data type and no other changes -> treat as rename with medium
    // confidence.
    let dropped: Vec<&PgColumn> = from
        .columns
        .values()
        .filter(|c| !to.columns.contains_key(&c.name))
        .collect();
    let added: Vec<&PgColumn> = to
        .columns
        .values()
        .filter(|c| !from.columns.contains_key(&c.name))
        .collect();
    for dcol in &dropped {
        for acol in &added {
            if dcol.data_type == acol.data_type
                && dcol.is_nullable == acol.is_nullable
                && dcol.is_identity == acol.is_identity
            {
                ops.push(ChangeOp {
                    kind: ChangeKind::RenameColumn { confidence: 700 },
                    object: ObjectIdentity::PostgreSql(PostgresObjectId::Column {
                        schema: from.schema.clone(),
                        table: from.name.clone(),
                        column: dcol.name.clone(),
                    }),
                    field: Some(acol.name.clone()),
                    confidence: 700,
                    before: Some(CanonicalValue::Text(dcol.name.clone())),
                    after: Some(CanonicalValue::Text(acol.name.clone())),
                    reason: format!("column {} renamed to {}", dcol.name, acol.name),
                });
            }
        }
    }

    // Constraints and indexes (create/drop only, by name).
    // Primary key.
    match (&from.primary_key, &to.primary_key) {
        (Some(_), None) => ops.push(drop_constraint_op(from, from.primary_key.as_ref().unwrap())),
        (None, Some(_)) => ops.push(add_constraint_op(from, to.primary_key.as_ref().unwrap())),
        (Some(fp), Some(tp)) if fp != tp => ops.push(alter_constraint_op(from, fp, tp)),
        _ => {}
    }

    for fc in &from.unique_constraints {
        if !to.unique_constraints.iter().any(|c| c.name == fc.name) {
            ops.push(drop_constraint_op(from, fc));
        }
    }
    for tc in &to.unique_constraints {
        if !from.unique_constraints.iter().any(|c| c.name == tc.name) {
            ops.push(add_constraint_op(from, tc));
        }
    }

    for fc in &from.foreign_keys {
        if !to.foreign_keys.iter().any(|c| c.name == fc.name) {
            ops.push(drop_constraint_op(from, fc));
        }
    }
    for tc in &to.foreign_keys {
        if !from.foreign_keys.iter().any(|c| c.name == tc.name) {
            ops.push(add_constraint_op(from, tc));
        }
    }

    for fc in &from.check_constraints {
        if !to.check_constraints.iter().any(|c| c.name == fc.name) {
            ops.push(drop_constraint_op(from, fc));
        }
    }
    for tc in &to.check_constraints {
        if !from.check_constraints.iter().any(|c| c.name == tc.name) {
            ops.push(add_constraint_op(from, tc));
        }
    }

    // Indexes.
    for (name, fi) in &from.indexes {
        if !to.indexes.contains_key(name) {
            ops.push(drop_index_op(from, fi));
        }
    }
    for (name, ti) in &to.indexes {
        if !from.indexes.contains_key(name) {
            ops.push(create_index_op(from, ti));
        } else if from.indexes.get(name).is_some_and(|fi| fi != ti) {
            let fi = from.indexes.get(name).unwrap();
            ops.push(alter_index_op(from, fi, ti));
        }
    }

    // Rows.
    ops.extend(diff_rows(from, to)?);

    Ok(ops)
}

fn diff_rows(from: &PgTable, to: &PgTable) -> Result<Vec<ChangeOp>, MeldError> {
    let mut ops: Vec<ChangeOp> = vec![];

    let mut from_by_id: std::collections::HashMap<
        &crate::state::PgRowIdentity,
        &crate::state::PgRow,
    > = std::collections::HashMap::new();
    for row in &from.rows {
        from_by_id.insert(&row.identity, row);
    }
    let mut to_by_id: std::collections::HashMap<
        &crate::state::PgRowIdentity,
        &crate::state::PgRow,
    > = std::collections::HashMap::new();
    for row in &to.rows {
        to_by_id.insert(&row.identity, row);
    }

    let object = |schema: &str, table: &str| {
        ObjectIdentity::PostgreSql(PostgresObjectId::Table {
            schema: schema.to_string(),
            table: table.to_string(),
        })
    };

    let mut matched: std::collections::HashSet<crate::state::PgRowIdentity> =
        std::collections::HashSet::new();

    for identity in to_by_id.keys() {
        let identity = (*identity).clone();
        match from_by_id.get(&identity) {
            Some(from_row) => {
                matched.insert(identity.clone());
                let to_row = to_by_id[&identity];
                if from_row.values != to_row.values {
                    ops.push(ChangeOp {
                        kind: ChangeKind::UpdateRow,
                        object: object(from.schema.as_str(), from.name.as_str()),
                        field: None,
                        confidence: 1000,
                        before: Some(CanonicalValue::Document(from_row.values.clone())),
                        after: Some(CanonicalValue::Document(to_row.values.clone())),
                        reason: format!("row updated in {}.{}", from.schema, from.name),
                    });
                }
            }
            None => {
                ops.push(ChangeOp {
                    kind: ChangeKind::InsertRow,
                    object: object(from.schema.as_str(), from.name.as_str()),
                    field: None,
                    confidence: 1000,
                    before: None,
                    after: Some(CanonicalValue::Document(to_by_id[&identity].values.clone())),
                    reason: format!("row inserted into {}.{}", from.schema, from.name),
                });
            }
        }
    }

    for identity in from_by_id.keys() {
        let identity = (*identity).clone();
        if !matched.contains(&identity) {
            ops.push(ChangeOp {
                kind: ChangeKind::DeleteRow,
                object: object(from.schema.as_str(), from.name.as_str()),
                field: None,
                confidence: 1000,
                before: Some(CanonicalValue::Document(
                    from_by_id[&identity].values.clone(),
                )),
                after: None,
                reason: format!("row deleted from {}.{}", from.schema, from.name),
            });
        }
    }

    Ok(ops)
}

fn alter_column_op(from: &PgTable, name: &str, fcol: &PgColumn, tcol: &PgColumn) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::AlterColumn,
        object: ObjectIdentity::PostgreSql(PostgresObjectId::Column {
            schema: from.schema.clone(),
            table: from.name.clone(),
            column: name.to_string(),
        }),
        field: Some(name.to_string()),
        confidence: 1000,
        before: Some(CanonicalValue::Text(format!("{:?}", fcol.data_type))),
        after: Some(CanonicalValue::Text(format!("{:?}", tcol.data_type))),
        reason: format!("column {name} altered"),
    }
}

fn drop_constraint_op(from: &PgTable, c: &PgConstraint) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::DropConstraint,
        object: ObjectIdentity::PostgreSql(PostgresObjectId::Constraint {
            schema: from.schema.clone(),
            table: from.name.clone(),
            constraint: c.name.clone(),
        }),
        field: None,
        confidence: 1000,
        before: Some(CanonicalValue::Text(c.definition.clone())),
        after: None,
        reason: format!("constraint {} dropped", c.name),
    }
}

fn add_constraint_op(from: &PgTable, c: &PgConstraint) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::AddConstraint,
        object: ObjectIdentity::PostgreSql(PostgresObjectId::Constraint {
            schema: from.schema.clone(),
            table: from.name.clone(),
            constraint: c.name.clone(),
        }),
        field: None,
        confidence: 1000,
        before: None,
        after: Some(CanonicalValue::Text(c.definition.clone())),
        reason: format!("constraint {} added", c.name),
    }
}

fn alter_constraint_op(from: &PgTable, fp: &PgConstraint, tp: &PgConstraint) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::AlterConstraint,
        object: ObjectIdentity::PostgreSql(PostgresObjectId::Constraint {
            schema: from.schema.clone(),
            table: from.name.clone(),
            constraint: fp.name.clone(),
        }),
        field: None,
        confidence: 1000,
        before: Some(CanonicalValue::Text(fp.definition.clone())),
        after: Some(CanonicalValue::Text(tp.definition.clone())),
        reason: format!("constraint {} altered", fp.name),
    }
}

fn drop_index_op(from: &PgTable, i: &PgIndex) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::DropIndex,
        object: ObjectIdentity::PostgreSql(PostgresObjectId::Index {
            schema: from.schema.clone(),
            table: from.name.clone(),
            index: i.name.clone(),
        }),
        field: None,
        confidence: 1000,
        before: Some(CanonicalValue::Text(i.definition.clone())),
        after: None,
        reason: format!("index {} dropped", i.name),
    }
}

fn create_index_op(from: &PgTable, i: &PgIndex) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::CreateIndex,
        object: ObjectIdentity::PostgreSql(PostgresObjectId::Index {
            schema: from.schema.clone(),
            table: from.name.clone(),
            index: i.name.clone(),
        }),
        field: None,
        confidence: 1000,
        before: None,
        after: Some(CanonicalValue::Text(i.definition.clone())),
        reason: format!("index {} created", i.name),
    }
}

fn alter_index_op(from: &PgTable, fi: &PgIndex, ti: &PgIndex) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::AlterIndex,
        object: ObjectIdentity::PostgreSql(PostgresObjectId::Index {
            schema: from.schema.clone(),
            table: from.name.clone(),
            index: fi.name.clone(),
        }),
        field: None,
        confidence: 1000,
        before: Some(CanonicalValue::Text(fi.definition.clone())),
        after: Some(CanonicalValue::Text(ti.definition.clone())),
        reason: format!("index {} altered", fi.name),
    }
}

fn create_db(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::CreateTable,
        ObjectIdentity::PostgreSql(PostgresObjectId::Schema {
            name: name.to_string(),
        }),
        format!("database {name} created"),
    )
}

fn drop_db(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::DropTable,
        ObjectIdentity::PostgreSql(PostgresObjectId::Schema {
            name: name.to_string(),
        }),
        format!("database {name} dropped"),
    )
}

fn create_schema(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::CreateTable,
        ObjectIdentity::PostgreSql(PostgresObjectId::Schema {
            name: name.to_string(),
        }),
        format!("schema {name} created"),
    )
}

fn drop_schema(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::DropTable,
        ObjectIdentity::PostgreSql(PostgresObjectId::Schema {
            name: name.to_string(),
        }),
        format!("schema {name} dropped"),
    )
}

fn create_table(schema: &str, object: PostgresObjectId) -> ChangeOp {
    let table_name = match &object {
        PostgresObjectId::Table { table, .. } => table.clone(),
        _ => "?".into(),
    };
    ChangeOp::new(
        ChangeKind::CreateTable,
        ObjectIdentity::PostgreSql(object),
        format!("table {schema}.{table_name} created"),
    )
}

fn drop_table(schema: &str, object: PostgresObjectId) -> ChangeOp {
    let table_name = match &object {
        PostgresObjectId::Table { table, .. } => table.clone(),
        _ => "?".into(),
    };
    ChangeOp::new(
        ChangeKind::DropTable,
        ObjectIdentity::PostgreSql(object),
        format!("table {schema}.{table_name} dropped"),
    )
}

// ---------------------------------------------------------------------------
// MongoDB semantic diff
// ---------------------------------------------------------------------------

fn diff_mongodb(from: &EngineState, to: &EngineState) -> Result<Vec<ChangeOp>, MeldError> {
    let EngineState::MongoDb(fs) = from else {
        unreachable!()
    };
    let EngineState::MongoDb(ts) = to else {
        unreachable!()
    };

    let mut ops: Vec<ChangeOp> = vec![];

    // Collections: existence.
    for (name, fcoll) in &fs.collections {
        match ts.collections.get(name) {
            None => ops.push(drop_collection_op(name)),
            Some(tcoll) => ops.extend(diff_collection(fcoll, tcoll)?),
        }
    }
    for name in ts.collections.keys() {
        if !fs.collections.contains_key(name) {
            ops.push(create_collection_op(name));
        }
    }

    Ok(ops)
}

fn diff_collection(
    from: &MongoCollectionState,
    to: &MongoCollectionState,
) -> Result<Vec<ChangeOp>, MeldError> {
    let name = &from.name;
    let mut ops: Vec<ChangeOp> = vec![];

    // Collection options.
    if from.options != to.options {
        ops.push(ChangeOp {
            kind: ChangeKind::AlterCollectionOptions,
            object: ObjectIdentity::MongoDb(MongoObjectId::Collection { name: name.clone() }),
            field: None,
            confidence: 1000,
            before: Some(CanonicalValue::Text(format!("{:?}", from.options))),
            after: Some(CanonicalValue::Text(format!("{:?}", to.options))),
            reason: format!("collection options changed for {name}"),
        });
    }

    // Validator.
    match (&from.validator, &to.validator) {
        (Some(_), None) => ops.push(drop_validator_op(name)),
        (None, Some(_)) => ops.push(add_validator_op(name)),
        (Some(fv), Some(tv)) if fv != tv => ops.push(ChangeOp {
            kind: ChangeKind::ChangeValidator,
            object: ObjectIdentity::MongoDb(MongoObjectId::Validator {
                collection: name.clone(),
            }),
            field: None,
            confidence: 1000,
            before: Some(CanonicalValue::Text(format!("{:?}", fv.definition))),
            after: Some(CanonicalValue::Text(format!("{:?}", tv.definition))),
            reason: format!("validator changed for {name}"),
        }),
        _ => {}
    }

    // Indexes.
    for (idx_name, fidx) in &from.indexes {
        if idx_name == "_id_" {
            continue;
        }
        match to.indexes.get(idx_name) {
            None => ops.push(drop_mongo_index_op(name, idx_name)),
            Some(tidx) if fidx != tidx => {
                ops.push(alter_mongo_index_op(name, idx_name, fidx, tidx))
            }
            _ => {}
        }
    }
    for idx_name in to.indexes.keys() {
        if idx_name == "_id_" {
            continue;
        }
        if !from.indexes.contains_key(idx_name) {
            ops.push(create_mongo_index_op(name, idx_name));
        }
    }

    // Views.
    let fviews: IndexMap<&str, &MongoView> =
        from.views.iter().map(|v| (v.name.as_str(), v)).collect();
    let tviews: IndexMap<&str, &MongoView> =
        to.views.iter().map(|v| (v.name.as_str(), v)).collect();
    for (vname, fview) in &fviews {
        match tviews.get(*vname) {
            None => ops.push(drop_view_op(vname)),
            Some(tview) if fview != tview => ops.push(alter_view_op(vname, fview, tview)),
            _ => {}
        }
    }
    for vname in tviews.keys() {
        if !fviews.contains_key(*vname) {
            ops.push(create_view_op(vname));
        }
    }

    // Documents: diff by _id, preserving BSON type distinctions.
    ops.extend(diff_documents(from, to)?);

    Ok(ops)
}

fn diff_documents(
    from: &MongoCollectionState,
    to: &MongoCollectionState,
) -> Result<Vec<ChangeOp>, MeldError> {
    let mut ops: Vec<ChangeOp> = vec![];

    let mut from_by_id: std::collections::HashMap<&CanonicalValue, &MongoDocument> =
        std::collections::HashMap::new();
    for doc in &from.documents {
        from_by_id.insert(&doc.id, doc);
    }
    let mut to_by_id: std::collections::HashMap<&CanonicalValue, &MongoDocument> =
        std::collections::HashMap::new();
    for doc in &to.documents {
        to_by_id.insert(&doc.id, doc);
    }

    let mut matched: std::collections::HashSet<CanonicalValue> = std::collections::HashSet::new();

    for id in to_by_id.keys() {
        let id = (*id).clone();
        match from_by_id.get(&id) {
            Some(fdoc) => {
                matched.insert(id.clone());
                let tdoc = to_by_id[&id];
                if fdoc.fields != tdoc.fields {
                    ops.extend(doc_field_changes(from.name.as_str(), &id, fdoc, tdoc)?);
                }
            }
            None => {
                let tdoc = to_by_id[&id];
                ops.push(insert_document_op(from.name.as_str(), &id, tdoc));
            }
        }
    }

    for id in from_by_id.keys() {
        let id = (*id).clone();
        if !matched.contains(&id) {
            ops.push(delete_document_op(from.name.as_str(), &id));
        }
    }

    Ok(ops)
}

/// Field-level changes between two versions of the same document. Nested
/// documents and arrays are compared by value; a type change (e.g. string
/// `_id` versus ObjectId `_id`) is already prevented by identity matching on
/// typed `CanonicalValue`.
fn doc_field_changes(
    collection: &str,
    id: &CanonicalValue,
    from: &MongoDocument,
    to: &MongoDocument,
) -> Result<Vec<ChangeOp>, MeldError> {
    let mut ops: Vec<ChangeOp> = vec![];

    for (field, fval) in &from.fields {
        match to.fields.get(field) {
            None => ops.push(remove_field_op(collection, id, field, fval)),
            Some(tval) if fval != tval => {
                ops.push(update_field_op(collection, id, field, fval, tval))
            }
            _ => {}
        }
    }
    for field in to.fields.keys() {
        if !from.fields.contains_key(field) {
            ops.push(add_field_op(
                collection,
                id,
                field,
                to.fields.get(field).unwrap(),
            ));
        }
    }

    Ok(ops)
}

// --- op constructors ---

fn create_collection_op(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::CreateCollection,
        ObjectIdentity::MongoDb(MongoObjectId::Collection {
            name: name.to_string(),
        }),
        format!("collection {name} created"),
    )
}

fn drop_collection_op(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::DropCollection,
        ObjectIdentity::MongoDb(MongoObjectId::Collection {
            name: name.to_string(),
        }),
        format!("collection {name} dropped"),
    )
}

fn add_validator_op(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::ChangeValidator,
        ObjectIdentity::MongoDb(MongoObjectId::Validator {
            collection: name.to_string(),
        }),
        format!("validator added to {name}"),
    )
}

fn drop_validator_op(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::ChangeValidator,
        ObjectIdentity::MongoDb(MongoObjectId::Validator {
            collection: name.to_string(),
        }),
        format!("validator removed from {name}"),
    )
}

fn create_mongo_index_op(collection: &str, index: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::CreateMongoIndex,
        ObjectIdentity::MongoDb(MongoObjectId::Index {
            collection: collection.to_string(),
            index: index.to_string(),
        }),
        format!("index {index} created on {collection}"),
    )
}

fn drop_mongo_index_op(collection: &str, index: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::DropMongoIndex,
        ObjectIdentity::MongoDb(MongoObjectId::Index {
            collection: collection.to_string(),
            index: index.to_string(),
        }),
        format!("index {index} dropped on {collection}"),
    )
}

fn alter_mongo_index_op(
    collection: &str,
    index: &str,
    from: &MongoIndex,
    to: &MongoIndex,
) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::AlterMongoIndex,
        object: ObjectIdentity::MongoDb(MongoObjectId::Index {
            collection: collection.to_string(),
            index: index.to_string(),
        }),
        field: None,
        confidence: 1000,
        before: Some(CanonicalValue::Text(format!("{:?}", from))),
        after: Some(CanonicalValue::Text(format!("{:?}", to))),
        reason: format!("index {index} altered on {collection}"),
    }
}

fn create_view_op(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::CreateView,
        ObjectIdentity::MongoDb(MongoObjectId::View {
            name: name.to_string(),
        }),
        format!("view {name} created"),
    )
}

fn drop_view_op(name: &str) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::DropView,
        ObjectIdentity::MongoDb(MongoObjectId::View {
            name: name.to_string(),
        }),
        format!("view {name} dropped"),
    )
}

fn alter_view_op(name: &str, from: &MongoView, to: &MongoView) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::AlterView,
        object: ObjectIdentity::MongoDb(MongoObjectId::View {
            name: name.to_string(),
        }),
        field: None,
        confidence: 1000,
        before: Some(CanonicalValue::Text(format!("{:?}", from))),
        after: Some(CanonicalValue::Text(format!("{:?}", to))),
        reason: format!("view {name} altered"),
    }
}

fn insert_document_op(collection: &str, id: &CanonicalValue, _doc: &MongoDocument) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::InsertDocument,
        ObjectIdentity::MongoDb(MongoObjectId::Document {
            collection: collection.to_string(),
            id: id.clone(),
        }),
        format!("document inserted into {collection}"),
    )
}

fn delete_document_op(collection: &str, id: &CanonicalValue) -> ChangeOp {
    ChangeOp::new(
        ChangeKind::DeleteDocument,
        ObjectIdentity::MongoDb(MongoObjectId::Document {
            collection: collection.to_string(),
            id: id.clone(),
        }),
        format!("document deleted from {collection}"),
    )
}

fn update_field_op(
    collection: &str,
    id: &CanonicalValue,
    field: &str,
    from: &CanonicalValue,
    to: &CanonicalValue,
) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::UpdateDocument,
        object: ObjectIdentity::MongoDb(MongoObjectId::Document {
            collection: collection.to_string(),
            id: id.clone(),
        }),
        field: Some(field.to_string()),
        confidence: 1000,
        before: Some(from.clone()),
        after: Some(to.clone()),
        reason: format!("field {field} changed in document in {collection}"),
    }
}

fn add_field_op(
    collection: &str,
    id: &CanonicalValue,
    field: &str,
    value: &CanonicalValue,
) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::UpdateDocument,
        object: ObjectIdentity::MongoDb(MongoObjectId::Document {
            collection: collection.to_string(),
            id: id.clone(),
        }),
        field: Some(field.to_string()),
        confidence: 1000,
        before: None,
        after: Some(value.clone()),
        reason: format!("field {field} added to document in {collection}"),
    }
}

fn remove_field_op(
    collection: &str,
    id: &CanonicalValue,
    field: &str,
    value: &CanonicalValue,
) -> ChangeOp {
    ChangeOp {
        kind: ChangeKind::UpdateDocument,
        object: ObjectIdentity::MongoDb(MongoObjectId::Document {
            collection: collection.to_string(),
            id: id.clone(),
        }),
        field: Some(field.to_string()),
        confidence: 1000,
        before: Some(value.clone()),
        after: None,
        reason: format!("field {field} removed from document in {collection}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        CaptureInfo, CollectionOptions, ConsistencyLevel, PgColumn, PgDatabase, PgSchema,
        PostgresState,
    };
    use chrono::Utc;
    use indexmap::IndexMap;

    #[test]
    fn identical_states_produce_empty_diff() {
        let s1 = EngineState::PostgreSql(test_state());
        let s2 = EngineState::PostgreSql(test_state());
        let diff = diff_states(&s1, &s2).unwrap();
        assert!(diff.is_empty());
        assert_eq!(diff.summary.total_changes(), 0);
    }

    #[test]
    fn column_add_produces_add_op() {
        let before = EngineState::PostgreSql(test_state());
        let mut after = EngineState::PostgreSql(test_state());
        if let EngineState::PostgreSql(st) = &mut after {
            let db = st.databases.get_mut("mydb").unwrap();
            let schema = db.schemas.get_mut("public").unwrap();
            let table = schema.tables.get_mut("t").unwrap();
            table.columns.insert(
                "b".into(),
                PgColumn {
                    name: "b".into(),
                    ordinal_position: 2,
                    data_type: pg_type_local("text"),
                    is_nullable: true,
                    column_default: None,
                    is_identity: false,
                    is_generated: false,
                    generated_expression: None,
                    comment: None,
                },
            );
        }
        let diff = diff_states(&before, &after).unwrap();
        assert_eq!(diff.summary.rows_inserted, 0);
        assert!(
            diff.ops
                .iter()
                .any(|o| matches!(o.kind, ChangeKind::AddColumn))
        );
    }

    #[test]
    fn incompatible_engines_rejected() {
        let pg = EngineState::PostgreSql(test_state());
        let mongo = EngineState::MongoDb(crate::state::MongoState {
            engine_version: "8".into(),
            database_name: "mydb".into(),
            collections: Default::default(),
            warnings: vec![],
            capture_info: crate::state::CaptureInfo {
                consistency: ConsistencyLevel::Snapshot,
                strategy: "test".into(),
                captured_at: Utc::now(),
                engine_version: "8".into(),
                scope: "mydb".into(),
                warnings: vec![],
            },
        });
        let err = diff_states(&pg, &mongo).unwrap_err();
        assert!(matches!(err, MeldError::IncompatibleEngine { .. }));
    }

    fn test_state() -> PostgresState {
        let mut columns = IndexMap::new();
        columns.insert(
            "a".into(),
            PgColumn {
                name: "a".into(),
                ordinal_position: 1,
                data_type: pg_type_local("int"),
                is_nullable: false,
                column_default: None,
                is_identity: false,
                is_generated: false,
                generated_expression: None,
                comment: None,
            },
        );
        let mut tables = IndexMap::new();
        tables.insert(
            "public".into(),
            PgSchema {
                name: "public".into(),
                tables: IndexMap::from([("t".into(), pg_table_local("t", columns))]),
            },
        );
        let mut dbs = IndexMap::new();
        dbs.insert(
            "mydb".into(),
            PgDatabase {
                name: "mydb".into(),
                schemas: tables,
                views: IndexMap::new(),
                sequences: IndexMap::new(),
            },
        );
        PostgresState {
            engine_version: "18".into(),
            databases: dbs,
            warnings: vec![],
            capture_info: CaptureInfo {
                consistency: ConsistencyLevel::Snapshot,
                strategy: "test".into(),
                captured_at: Utc::now(),
                engine_version: "18".into(),
                scope: "mydb".into(),
                warnings: vec![],
            },
        }
    }

    fn pg_type_local(name: &str) -> crate::state::PgType {
        crate::state::PgType {
            name: name.into(),
            oid: Some(23),
            internal_type: Some("int4".into()),
            array: false,
            length: None,
            precision: None,
            scale: None,
        }
    }

    fn pg_table_local(name: &str, columns: IndexMap<String, PgColumn>) -> PgTable {
        PgTable {
            name: name.into(),
            schema: "public".into(),
            columns,
            primary_key: None,
            unique_constraints: vec![],
            foreign_keys: vec![],
            check_constraints: vec![],
            indexes: IndexMap::new(),
            comment: None,
            row_count: 0,
            rows: vec![],
        }
    }

    // ----- MongoDB state helpers -----

    fn mongo_id_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn mongo_state(collections: IndexMap<String, MongoCollectionState>) -> EngineState {
        EngineState::MongoDb(crate::state::MongoState {
            engine_version: "8.0".into(),
            database_name: "mydb".into(),
            collections,
            warnings: vec![],
            capture_info: crate::state::CaptureInfo {
                consistency: ConsistencyLevel::Snapshot,
                strategy: "test".into(),
                captured_at: Utc::now(),
                engine_version: "8.0".into(),
                scope: "mydb".into(),
                warnings: vec![],
            },
        })
    }

    fn collection_state(name: &str, documents: Vec<MongoDocument>) -> MongoCollectionState {
        let count = documents.len() as u64;
        MongoCollectionState {
            name: name.to_string(),
            options: CollectionOptions::default(),
            validator: None,
            indexes: IndexMap::new(),
            views: vec![],
            documents,
            document_count: count,
        }
    }

    fn doc(id: CanonicalValue, fields: Vec<(&str, CanonicalValue)>) -> MongoDocument {
        MongoDocument {
            id,
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    #[test]
    fn mongodb_inserted_document_produces_insert_op() {
        let before = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state("users", vec![]),
        )]));
        let mut after = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state(
                "users",
                vec![doc(
                    CanonicalValue::ObjectId(mongo_id_bytes("507f1f77bcf86cd799439011")),
                    vec![("name", CanonicalValue::Text("ada".into()))],
                )],
            ),
        )]));
        if let EngineState::MongoDb(s) = &mut after {
            s.collections.get_mut("users").unwrap().document_count = 1;
        }
        let diff = diff_states(&before, &after).unwrap();
        assert!(
            diff.ops
                .iter()
                .any(|o| matches!(o.kind, ChangeKind::InsertDocument)),
            "expected InsertDocument, got {:?}",
            diff.ops
        );
    }

    #[test]
    fn mongodb_deleted_document_produces_delete_op() {
        let before = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state(
                "users",
                vec![doc(
                    CanonicalValue::ObjectId(mongo_id_bytes("507f1f77bcf86cd799439011")),
                    vec![("name", CanonicalValue::Text("ada".into()))],
                )],
            ),
        )]));
        let after = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state("users", vec![]),
        )]));
        let diff = diff_states(&before, &after).unwrap();
        assert!(
            diff.ops
                .iter()
                .any(|o| matches!(o.kind, ChangeKind::DeleteDocument)),
            "expected DeleteDocument, got {:?}",
            diff.ops
        );
    }

    #[test]
    fn mongodb_updated_field_produces_update_op() {
        let before = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state(
                "users",
                vec![doc(
                    CanonicalValue::ObjectId(mongo_id_bytes("507f1f77bcf86cd799439011")),
                    vec![("name", CanonicalValue::Text("ada".into()))],
                )],
            ),
        )]));
        let mut after = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state(
                "users",
                vec![doc(
                    CanonicalValue::ObjectId(mongo_id_bytes("507f1f77bcf86cd799439011")),
                    vec![("name", CanonicalValue::Text("ada lovelace".into()))],
                )],
            ),
        )]));
        if let EngineState::MongoDb(s) = &mut after {
            s.collections.get_mut("users").unwrap().document_count = 1;
        }
        let diff = diff_states(&before, &after).unwrap();
        assert!(
            diff.ops.iter().any(|o| {
                matches!(o.kind, ChangeKind::UpdateDocument) && o.field.as_deref() == Some("name")
            }),
            "expected UpdateDocument on field name, got {:?}",
            diff.ops
        );
    }

    #[test]
    fn mongodb_string_and_objectid_ids_are_distinct() {
        let str_id = CanonicalValue::Text("507f1f77bcf86cd799439011".into());
        let oid = CanonicalValue::ObjectId(mongo_id_bytes("507f1f77bcf86cd799439011"));
        assert_ne!(str_id, oid);

        let before = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state("users", vec![doc(str_id, vec![])]),
        )]));
        let after = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state("users", vec![doc(oid.clone(), vec![])]),
        )]));
        let diff = diff_states(&before, &after).unwrap();
        // A string _id replaced by an ObjectId _id is a delete + insert, not an
        // in-place update of the same identity.
        let has_delete = diff
            .ops
            .iter()
            .any(|o| matches!(o.kind, ChangeKind::DeleteDocument));
        let has_insert = diff
            .ops
            .iter()
            .any(|o| matches!(o.kind, ChangeKind::InsertDocument));
        assert!(has_delete && has_insert);
    }

    #[test]
    fn mongodb_nested_document_change_detected() {
        let before = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state(
                "users",
                vec![doc(
                    CanonicalValue::ObjectId(mongo_id_bytes("507f1f77bcf86cd799439011")),
                    vec![(
                        "address",
                        CanonicalValue::Document(IndexMap::from([(
                            "city".into(),
                            CanonicalValue::Text("Paris".into()),
                        )])),
                    )],
                )],
            ),
        )]));
        let mut after = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state(
                "users",
                vec![doc(
                    CanonicalValue::ObjectId(mongo_id_bytes("507f1f77bcf86cd799439011")),
                    vec![(
                        "address",
                        CanonicalValue::Document(IndexMap::from([(
                            "city".into(),
                            CanonicalValue::Text("Lyon".into()),
                        )])),
                    )],
                )],
            ),
        )]));
        if let EngineState::MongoDb(s) = &mut after {
            s.collections.get_mut("users").unwrap().document_count = 1;
        }
        let diff = diff_states(&before, &after).unwrap();
        assert!(
            diff.ops.iter().any(|o| {
                matches!(o.kind, ChangeKind::UpdateDocument)
                    && o.field.as_deref() == Some("address")
            }),
            "expected nested document change, got {:?}",
            diff.ops
        );
    }

    #[test]
    fn mongodb_index_change_detected() {
        let mut indexes = IndexMap::new();
        indexes.insert(
            "name_1".into(),
            MongoIndex {
                name: "name_1".into(),
                keys: vec![("name".into(), 1)],
                unique: false,
                partial_filter: None,
                ttl: None,
                sparse: None,
                background: None,
            },
        );
        let before = mongo_state(IndexMap::from([(
            "users".into(),
            MongoCollectionState {
                name: "users".into(),
                options: CollectionOptions::default(),
                validator: None,
                indexes,
                views: vec![],
                documents: vec![],
                document_count: 0,
            },
        )]));
        let after = mongo_state(IndexMap::from([(
            "users".into(),
            MongoCollectionState {
                name: "users".into(),
                options: CollectionOptions::default(),
                validator: None,
                indexes: IndexMap::new(),
                views: vec![],
                documents: vec![],
                document_count: 0,
            },
        )]));
        let diff = diff_states(&before, &after).unwrap();
        assert!(
            diff.ops
                .iter()
                .any(|o| matches!(o.kind, ChangeKind::DropMongoIndex)),
            "expected DropMongoIndex, got {:?}",
            diff.ops
        );
    }

    #[test]
    fn mongodb_collection_created_and_dropped() {
        let before = mongo_state(IndexMap::new());
        let after = mongo_state(IndexMap::from([(
            "orders".into(),
            MongoCollectionState {
                name: "orders".into(),
                options: CollectionOptions::default(),
                validator: None,
                indexes: IndexMap::new(),
                views: vec![],
                documents: vec![],
                document_count: 0,
            },
        )]));
        let diff = diff_states(&before, &after).unwrap();
        assert!(
            diff.ops
                .iter()
                .any(|o| matches!(o.kind, ChangeKind::CreateCollection)),
            "expected CreateCollection, got {:?}",
            diff.ops
        );

        let diff = diff_states(&after, &before).unwrap();
        assert!(
            diff.ops
                .iter()
                .any(|o| matches!(o.kind, ChangeKind::DropCollection)),
            "expected DropCollection, got {:?}",
            diff.ops
        );
    }

    #[test]
    fn mongodb_identical_states_empty_diff() {
        let s = mongo_state(IndexMap::from([(
            "users".into(),
            collection_state("users", vec![]),
        )]));
        let diff = diff_states(
            &s,
            &mongo_state(IndexMap::from([(
                "users".into(),
                collection_state("users", vec![]),
            )])),
        )
        .unwrap();
        assert!(diff.is_empty());
    }
}

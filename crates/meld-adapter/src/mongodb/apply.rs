//! MongoDB restore-plan execution.
//!
//! Executes dependency-aware restore operations against a live MongoDB
//! database. Operations are applied in the order supplied by the plan (the
//! engine guarantees dependency order). Multi-document consistency depends on
//! the deployment (replica set / sharded cluster with WiredTiger); single-node
//! restores are best-effort, which is captured in the snapshot's consistency
//! metadata.

use bson::{Bson, Document};
use meld_core::error::MeldError;
use meld_core::plan::{PlanOp, PlanOpKind, RestorePlan};
use meld_core::state::{
    CanonicalValue, EngineKind, MongoCollectionState, MongoDocument, MongoIndex, MongoView,
    Validator,
};
use mongodb::{Client, IndexModel};
use serde_json;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Execute a MongoDB restore plan against the live database.
///
/// `db_name` is the target database (supplied by the caller/adapter, since the
/// plan itself is engine-agnostic).
pub async fn execute(
    client: &Client,
    db_name: &str,
    plan: &RestorePlan,
) -> Result<crate::adapter::ExecutionResult, MeldError> {
    if plan.engine_kind != EngineKind::MongoDb {
        return Err(MeldError::IncompatibleEngine {
            expected: EngineKind::MongoDb,
            got: plan.engine_kind,
        });
    }

    let db = client.database(db_name);

    let ordered = plan
        .ordered_ops()
        .map_err(|e| MeldError::PlanValidation(e.to_string()))?;

    let mut executed: u64 = 0;
    let mut succeeded: u64 = 0;
    let mut failed: u64 = 0;
    let mut messages: Vec<String> = Vec::new();

    for op in &ordered {
        executed += 1;
        let result = execute_op(&db, op).await;
        match result {
            Ok(msg) => {
                succeeded += 1;
                messages.push(format!("{}: {}", op.id, msg));
                debug!(op = %op.id, risk = ?op.risk, "executed mongodb op");
            }
            Err(e) => {
                failed += 1;
                messages.push(format!("{}: FAILED: {e}", op.id));
                warn!(op = %op.id, error = %e, "mongodb op failed");
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

    info!("mongodb restore plan committed: {succeeded}/{executed} operations succeeded");

    Ok(crate::adapter::ExecutionResult {
        operations_executed: executed,
        operations_succeeded: succeeded,
        operations_failed: failed,
        recovered: failed == 0,
        final_state: None,
        messages,
    })
}

/// Execute a single planned MongoDB operation.
async fn execute_op(db: &mongodb::Database, op: &PlanOp) -> Result<String, MeldError> {
    use meld_core::identity::{MongoObjectId, ObjectIdentity};

    match &op.object {
        ObjectIdentity::MongoDb(mongo_id) => match mongo_id {
            MongoObjectId::Collection { name } => match op.kind {
                PlanOpKind::CreateCollection => {
                    let state = details_collection_state(&op.details)?;
                    create_collection(db, name, &state).await
                }
                PlanOpKind::DropCollection => drop_collection(db, name).await,
                PlanOpKind::AlterCollection => {
                    let state = details_collection_state(&op.details)?;
                    alter_collection(db, name, &state).await
                }
                PlanOpKind::ChangeValidator => {
                    let validator = details_validator(&op.details)?;
                    change_validator(db, name, &validator).await
                }
                PlanOpKind::InsertDocument => {
                    let doc = details_document(&op.details)?;
                    insert_document(db, name, &doc).await
                }
                PlanOpKind::UpdateDocument => {
                    let doc = details_document(&op.details)?;
                    update_document(db, name, &doc).await
                }
                PlanOpKind::DeleteDocument => {
                    let doc = details_document(&op.details)?;
                    delete_document(db, name, &doc).await
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on collection not yet implemented",
                    op.kind
                ))),
            },
            MongoObjectId::Index { collection, index } => match op.kind {
                PlanOpKind::CreateIndex => {
                    let idx = details_index(&op.details)?;
                    create_index(db, collection, &idx).await
                }
                PlanOpKind::DropIndex => drop_index(db, collection, index).await,
                PlanOpKind::AlterIndex => {
                    let idx = details_index(&op.details)?;
                    alter_index(db, collection, index, &idx).await
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on index not yet implemented",
                    op.kind
                ))),
            },
            MongoObjectId::Validator { collection } => match op.kind {
                PlanOpKind::ChangeValidator => {
                    let validator = details_validator(&op.details)?;
                    change_validator(db, collection, &validator).await
                }
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on validator not yet implemented",
                    op.kind
                ))),
            },
            MongoObjectId::View { name } => match op.kind {
                PlanOpKind::CreateObject => {
                    let view = details_view(&op.details)?;
                    create_view(db, name, &view).await
                }
                PlanOpKind::DropObject => drop_view(db, name).await,
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on view not yet implemented",
                    op.kind
                ))),
            },
            MongoObjectId::Database { name } => match op.kind {
                PlanOpKind::CreateObject => {
                    Ok(format!("database {name} ensured (MongoDB creates databases lazily)"))
                }
                PlanOpKind::DropObject => Err(MeldError::Unsupported(
                    "dropping a whole database is not supported by the restore planner".into(),
                )),
                _ => Err(MeldError::Unsupported(format!(
                    "op {:?} on database not yet implemented",
                    op.kind
                ))),
            },
            MongoObjectId::Document { .. } => Err(MeldError::Unsupported(
                "document operations use the collection identity with InsertDocument/UpdateDocument/DeleteDocument".into(),
            )),
        },
        ObjectIdentity::PostgreSql(_) => Err(MeldError::IncompatibleEngine {
            expected: EngineKind::MongoDb,
            got: EngineKind::PostgreSql,
        }),
    }
}

// ---------------------------------------------------------------------------
// Collection operations
// ---------------------------------------------------------------------------

async fn create_collection(
    db: &mongodb::Database,
    name: &str,
    state: &MongoCollectionState,
) -> Result<String, MeldError> {
    let options = &state.options;
    let mut cmd = bson::doc! { "create": name };
    if let Some(capped) = options.capped {
        cmd.insert("capped", capped);
    }
    if let Some(size) = options.size {
        cmd.insert("size", size as i64);
    }
    if let Some(max) = options.max {
        cmd.insert("max", max as i64);
    }
    if let Some(expires) = options.expires_after_seconds {
        cmd.insert("expireAfterSeconds", expires as i64);
    }
    // Additional options (e.g. timeseries, change stream settings) pass through.
    for (k, v) in &options.additional {
        cmd.insert(k, canonical_value_to_bson(v));
    }

    if let Some(validator) = &state.validator {
        let mut options_doc = Document::new();
        options_doc.insert("validationLevel", &validator.validation_level);
        options_doc.insert("validationAction", &validator.validation_action);
        if !validator.definition.is_empty() {
            let mut def_doc = Document::new();
            for (k, v) in &validator.definition {
                def_doc.insert(k, canonical_value_to_bson(v));
            }
            options_doc.insert("validator", def_doc);
        }
        cmd.insert("validator", options_doc);
    }

    db.run_command(cmd, None)
        .await
        .map_err(|e| MeldError::Io(format!("create collection {name}: {e}")))?;

    // Create indexes (excluding the implicit _id index).
    for (_, idx) in &state.indexes {
        if idx.name == "_id_" {
            continue;
        }
        create_index_inner(db, name, idx).await?;
    }

    Ok("created collection".into())
}

async fn drop_collection(db: &mongodb::Database, name: &str) -> Result<String, MeldError> {
    db.collection::<Document>(name)
        .drop(None)
        .await
        .map_err(|e| MeldError::Io(format!("drop collection {name}: {e}")))?;
    Ok("dropped collection".into())
}

async fn alter_collection(
    db: &mongodb::Database,
    name: &str,
    state: &MongoCollectionState,
) -> Result<String, MeldError> {
    let mut changed = false;
    if let Some(validator) = &state.validator {
        apply_validator(db, name, validator).await?;
        changed = true;
    }
    let options = &state.options;
    let mut mod_doc = Document::new();
    if let Some(size) = options.size {
        mod_doc.insert("size", size as i64);
    }
    if let Some(max) = options.max {
        mod_doc.insert("max", max as i64);
    }
    if !mod_doc.is_empty() {
        db.run_command(
            bson::doc! { "collMod": name, "size": mod_doc.get_i64("size").unwrap_or(0), "max": mod_doc.get_i64("max").unwrap_or(0) },
            None,
        )
        .await
        .map_err(|e| MeldError::Io(format!("collMod {name}: {e}")))?;
        changed = true;
    }
    if changed {
        Ok("altered collection options/validator".into())
    } else {
        Ok("collection already matches target".into())
    }
}

async fn change_validator(
    db: &mongodb::Database,
    collection: &str,
    validator: &Validator,
) -> Result<String, MeldError> {
    apply_validator(db, collection, validator).await
}

async fn apply_validator(
    db: &mongodb::Database,
    collection: &str,
    validator: &Validator,
) -> Result<String, MeldError> {
    let mut options_doc = Document::new();
    options_doc.insert("validationLevel", &validator.validation_level);
    options_doc.insert("validationAction", &validator.validation_action);
    if !validator.definition.is_empty() {
        let mut def_doc = Document::new();
        for (k, v) in &validator.definition {
            def_doc.insert(k, canonical_value_to_bson(v));
        }
        options_doc.insert("validator", def_doc);
    }
    db.run_command(
        bson::doc! { "collMod": collection, "validator": options_doc },
        None,
    )
    .await
    .map_err(|e| MeldError::Io(format!("apply validator {collection}: {e}")))?;
    Ok("applied collection validator".into())
}

// ---------------------------------------------------------------------------
// Index operations
// ---------------------------------------------------------------------------

async fn create_index(
    db: &mongodb::Database,
    collection: &str,
    idx: &MongoIndex,
) -> Result<String, MeldError> {
    create_index_inner(db, collection, idx).await
}

async fn create_index_inner(
    db: &mongodb::Database,
    collection: &str,
    idx: &MongoIndex,
) -> Result<String, MeldError> {
    let coll = db.collection::<Document>(collection);
    let mut keys_doc = Document::new();
    for (field, direction) in &idx.keys {
        keys_doc.insert(field, *direction);
    }

    let mut filter_doc: Option<Document> = None;
    if let Some(filter) = &idx.partial_filter {
        let mut fd = Document::new();
        for (k, v) in filter {
            fd.insert(k, canonical_value_to_bson(v));
        }
        filter_doc = Some(fd);
    }

    // `IndexOptions` is `#[non_exhaustive]`, so build it via `default()` + field
    // mutation rather than a struct literal.
    let mut options = mongodb::options::IndexOptions::default();
    options.unique = Some(idx.unique);
    options.sparse = idx.sparse;
    options.background = idx.background;
    options.expire_after = idx.ttl.map(Duration::from_secs);
    options.partial_filter_expression = filter_doc;

    let model = IndexModel::builder()
        .keys(keys_doc)
        .options(options)
        .build();

    coll.create_index(model, None)
        .await
        .map_err(|e| MeldError::Io(format!("create index {} on {collection}: {e}", idx.name)))?;
    Ok(format!("created index {}", idx.name))
}

async fn drop_index(
    db: &mongodb::Database,
    collection: &str,
    index_name: &str,
) -> Result<String, MeldError> {
    if index_name == "_id_" {
        return Err(MeldError::InvalidInput(
            "cannot drop the implicit _id index".into(),
        ));
    }
    db.collection::<Document>(collection)
        .drop_index(index_name, None)
        .await
        .map_err(|e| MeldError::Io(format!("drop index {index_name} on {collection}: {e}")))?;
    Ok(format!("dropped index {index_name}"))
}

async fn alter_index(
    db: &mongodb::Database,
    collection: &str,
    index_name: &str,
    idx: &MongoIndex,
) -> Result<String, MeldError> {
    // MongoDB cannot alter most index options in place; drop and recreate.
    drop_index(db, collection, index_name).await?;
    create_index_inner(db, collection, idx).await?;
    Ok(format!("altered index {index_name}"))
}

// ---------------------------------------------------------------------------
// Document operations
// ---------------------------------------------------------------------------

async fn insert_document(
    db: &mongodb::Database,
    collection: &str,
    doc: &MongoDocument,
) -> Result<String, MeldError> {
    let mut bson_doc = canonical_value_to_bson_document(&doc.fields);
    bson_doc.insert("_id", canonical_value_to_bson(&doc.id));
    db.collection::<Document>(collection)
        .insert_one(bson_doc, None)
        .await
        .map_err(|e| MeldError::Io(format!("insert document into {collection}: {e}")))?;
    Ok("inserted document".into())
}

async fn update_document(
    db: &mongodb::Database,
    collection: &str,
    doc: &MongoDocument,
) -> Result<String, MeldError> {
    let filter = bson::doc! { "_id": canonical_value_to_bson(&doc.id) };
    let mut set_doc = Document::new();
    for (k, v) in &doc.fields {
        set_doc.insert(k, canonical_value_to_bson(v));
    }
    let update_doc = bson::doc! { "$set": set_doc };
    let result = db
        .collection::<Document>(collection)
        .update_one(filter, update_doc, None)
        .await
        .map_err(|e| MeldError::Io(format!("update document in {collection}: {e}")))?;
    if result.matched_count == 0 {
        warn!("update document matched 0 documents in {collection}; _id may not exist");
    }
    Ok("updated document".into())
}

async fn delete_document(
    db: &mongodb::Database,
    collection: &str,
    doc: &MongoDocument,
) -> Result<String, MeldError> {
    let filter = bson::doc! { "_id": canonical_value_to_bson(&doc.id) };
    let result = db
        .collection::<Document>(collection)
        .delete_one(filter, None)
        .await
        .map_err(|e| MeldError::Io(format!("delete document from {collection}: {e}")))?;
    if result.deleted_count == 0 {
        warn!("delete document matched 0 documents in {collection}; _id may not exist");
    }
    Ok("deleted document".into())
}

// ---------------------------------------------------------------------------
// View operations
// ---------------------------------------------------------------------------

async fn create_view(
    db: &mongodb::Database,
    name: &str,
    view: &MongoView,
) -> Result<String, MeldError> {
    let mut pipeline_doc = Document::new();
    for (k, v) in &view.definition {
        pipeline_doc.insert(k, canonical_value_to_bson(v));
    }
    db.run_command(
        bson::doc! { "create": name, "viewOn": name, "pipeline": pipeline_doc },
        None,
    )
    .await
    .map_err(|e| MeldError::Io(format!("create view {name}: {e}")))?;
    Ok("created view".into())
}

async fn drop_view(db: &mongodb::Database, name: &str) -> Result<String, MeldError> {
    db.collection::<Document>(name)
        .drop(None)
        .await
        .map_err(|e| MeldError::Io(format!("drop view {name}: {e}")))?;
    Ok("dropped view".into())
}

// ---------------------------------------------------------------------------
// Detail deserialization helpers
// ---------------------------------------------------------------------------

fn details_collection_state(details: &Option<String>) -> Result<MongoCollectionState, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<MongoCollectionState>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid collection state details: {e}")))
}

fn details_index(details: &Option<String>) -> Result<MongoIndex, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<MongoIndex>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid index details: {e}")))
}

fn details_document(details: &Option<String>) -> Result<MongoDocument, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<MongoDocument>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid document details: {e}")))
}

fn details_validator(details: &Option<String>) -> Result<Validator, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<Validator>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid validator details: {e}")))
}

fn details_view(details: &Option<String>) -> Result<MongoView, MeldError> {
    let d = details
        .as_deref()
        .ok_or_else(|| MeldError::InvalidInput("missing plan op details".into()))?;
    serde_json::from_str::<MongoView>(d)
        .map_err(|e| MeldError::InvalidInput(format!("invalid view details: {e}")))
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Convert a CanonicalValue back into a BSON value for writing to MongoDB.
/// Type distinctions are preserved so a string _id and an ObjectId _id remain
/// distinct.
fn canonical_value_to_bson(v: &CanonicalValue) -> Bson {
    match v {
        CanonicalValue::Null => Bson::Null,
        CanonicalValue::Boolean(b) => Bson::Boolean(*b),
        CanonicalValue::I32(i) => Bson::Int32(*i),
        CanonicalValue::I64(i) => Bson::Int64(*i),
        CanonicalValue::F64(bits) => Bson::Double(f64::from_bits(*bits)),
        CanonicalValue::Numeric(s) => match s.parse::<bson::Decimal128>() {
            Ok(d) => Bson::Decimal128(d),
            Err(_) => Bson::String(s.clone()),
        },
        CanonicalValue::Text(s) => Bson::String(s.clone()),
        CanonicalValue::Bytes(bytes) => Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Generic,
            bytes: bytes.clone(),
        }),
        CanonicalValue::Timestamp(ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
            Ok(dt) => Bson::DateTime(bson::DateTime::from_millis(dt.timestamp_millis())),
            Err(_) => Bson::String(ts.clone()),
        },
        CanonicalValue::ObjectId(bytes) => {
            if bytes.len() == 12 {
                let mut arr = [0u8; 12];
                arr.copy_from_slice(bytes);
                Bson::ObjectId(mongodb::bson::oid::ObjectId::from_bytes(arr))
            } else {
                Bson::Binary(bson::Binary {
                    subtype: bson::spec::BinarySubtype::Generic,
                    bytes: bytes.clone(),
                })
            }
        }
        CanonicalValue::Array(items) => {
            Bson::Array(items.iter().map(canonical_value_to_bson).collect())
        }
        CanonicalValue::Document(fields) => {
            let mut doc = Document::new();
            for (k, v) in fields {
                doc.insert(k.clone(), canonical_value_to_bson(v));
            }
            Bson::Document(doc)
        }
    }
}

/// Convert a field map into a BSON document.
fn canonical_value_to_bson_document(
    fields: &indexmap::IndexMap<String, CanonicalValue>,
) -> Document {
    let mut doc = Document::new();
    for (k, v) in fields {
        doc.insert(k.clone(), canonical_value_to_bson(v));
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_value_to_bson_preserves_type_distinction() {
        let text_id =
            canonical_value_to_bson(&CanonicalValue::Text("507f1f77bcf86cd799439011".into()));
        let bytes: Vec<u8> = (0..24)
            .step_by(2)
            .map(|i| u8::from_str_radix(&"507f1f77bcf86cd799439011"[i..i + 2], 16).unwrap())
            .collect();
        let oid = canonical_value_to_bson(&CanonicalValue::ObjectId(bytes));
        assert_ne!(text_id, oid);
    }

    #[test]
    fn canonical_value_to_bson_round_trip_primitives() {
        assert_eq!(
            canonical_value_to_bson(&CanonicalValue::Text("O'Brien".into())),
            Bson::String("O'Brien".into())
        );
        assert_eq!(
            canonical_value_to_bson(&CanonicalValue::I32(42)),
            Bson::Int32(42)
        );
        assert_eq!(canonical_value_to_bson(&CanonicalValue::Null), Bson::Null);
        assert_eq!(
            canonical_value_to_bson(&CanonicalValue::Boolean(true)),
            Bson::Boolean(true)
        );
    }
}

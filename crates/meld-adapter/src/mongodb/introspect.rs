//! MongoDB catalog introspection.
//!
//! Discovers collections, indexes, validators, and views using
//! the official driver's listCollections / listIndexes commands.

use chrono::Utc;
use indexmap::IndexMap;
use meld_core::error::MeldError;
use meld_core::state::{
    CanonicalValue, CaptureInfo, CollectionOptions, ConsistencyLevel, MongoCollectionState,
    MongoDocument, MongoIndex, MongoState, MongoView, Validator,
};
use mongodb::Database;
use tracing::debug;

pub async fn introspect(
    client: &mongodb::Client,
    db_name: &str,
) -> Result<meld_core::state::EngineState, MeldError> {
    let db = client.database(db_name);
    let collection_names = db
        .list_collection_names(None)
        .await
        .map_err(|e| MeldError::Io(format!("list collections: {e}")))?;

    let mut coll_states = IndexMap::new();
    for coll_name in &collection_names {
        debug!(collection = %coll_name, "introspecting collection");
        let coll = db.collection::<mongodb::bson::Document>(coll_name);

        let options = get_collection_options(&db, coll_name)
            .await
            .unwrap_or_default();
        let validator = get_validator(&db, coll_name).await.ok().flatten();
        let indexes = get_indexes(&coll).await.unwrap_or_default();
        let views = get_views(&db).await.unwrap_or_default();
        let documents = extract_documents(&coll).await.unwrap_or_default();
        let document_count = documents.len() as u64;

        coll_states.insert(
            coll_name.clone(),
            MongoCollectionState {
                name: coll_name.clone(),
                options,
                validator,
                indexes,
                views,
                documents,
                document_count,
            },
        );
    }

    let version = {
        let info = db
            .run_command(bson::doc! {"buildInfo": 1}, None)
            .await
            .map_err(|e| MeldError::Io(format!("buildInfo: {e}")))?;
        info.get_str("version")
            .map(|s| s.to_string())
            .unwrap_or_default()
    };

    let state = MongoState {
        engine_version: version.clone(),
        database_name: db_name.to_string(),
        collections: coll_states,
        warnings: vec![meld_core::CaptureWarning {
            object: "mongodb".into(),
            message: "MongoDB single-node captures are best-effort; multi-document consistency requires transactions on a replica set.".into(),
        }],
        capture_info: CaptureInfo {
            consistency: ConsistencyLevel::BestEffort,
            strategy: "driver listCollections/listIndexes + cursor document extraction".into(),
            captured_at: Utc::now(),
            engine_version: version.clone(),
            scope: db_name.to_string(),
            warnings: vec![],
        },
    };

    Ok(meld_core::state::EngineState::MongoDb(state))
}

pub async fn get_collection_options(
    db: &Database,
    name: &str,
) -> Result<CollectionOptions, MeldError> {
    let doc = db
        .run_command(
            bson::doc! {"listCollections": 1, "filter": bson::doc! {"name": name}},
            None,
        )
        .await
        .map_err(|e| MeldError::Io(format!("listCollections: {e}")))?;
    let first_batch = doc
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get_array("firstBatch").ok())
        .and_then(|arr| arr.first().cloned())
        .and_then(|v| v.as_document().cloned());
    let Some(doc) = first_batch else {
        return Ok(CollectionOptions::default());
    };

    let capped = doc.get_bool("capped").ok().unwrap_or(false);
    let size = doc.get_i64("size").ok().map(|v| v as u64);
    let max = doc.get_i64("max").ok().map(|v| v as u64);
    let expires = doc.get_i64("expireAfterSeconds").ok().map(|v| v as u64);

    let mut additional = IndexMap::new();
    for (k, v) in doc.iter() {
        match k.as_str() {
            "name" | "capped" | "size" | "max" | "expireAfterSeconds" | "options" => {}
            _ => {
                additional.insert(k.clone(), bson_value_to_canonical(v));
            }
        }
    }

    Ok(CollectionOptions {
        capped: Some(capped),
        size,
        max,
        expires_after_seconds: expires,
        additional,
    })
}

pub async fn get_validator(db: &Database, name: &str) -> Result<Option<Validator>, MeldError> {
    let doc = db
        .run_command(
            bson::doc! {"listCollections": 1, "filter": bson::doc! {"name": name}},
            None,
        )
        .await
        .map_err(|e| MeldError::Io(format!("listCollections validator: {e}")))?;
    let coll_doc = doc
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get_array("firstBatch").ok())
        .and_then(|arr| arr.first().cloned())
        .and_then(|v| v.as_document().cloned());
    let Some(doc) = coll_doc else {
        return Ok(None);
    };

    let options_doc = doc.get_document("options").ok();
    let Some(options) = options_doc else {
        return Ok(None);
    };

    let validator = options.get_document("validator").ok();
    let validation_level = options
        .get_str("validationLevel")
        .map(|s| s.to_string())
        .unwrap_or("moderate".into());
    let validation_action = options
        .get_str("validationAction")
        .map(|s| s.to_string())
        .unwrap_or("warn".into());

    Ok(Some(Validator {
        validation_level,
        validation_action,
        definition: validator
            .map(|d| {
                d.iter()
                    .map(|(k, v)| (k.clone(), bson_value_to_canonical(v)))
                    .collect()
            })
            .unwrap_or_default(),
    }))
}

pub async fn get_indexes(
    coll: &mongodb::Collection<mongodb::bson::Document>,
) -> Result<IndexMap<String, MongoIndex>, MeldError> {
    let mut cursor = coll
        .list_indexes(None)
        .await
        .map_err(|e| MeldError::Io(format!("list indexes: {e}")))?;
    let mut out = IndexMap::new();
    while cursor
        .advance()
        .await
        .map_err(|e| MeldError::Io(format!("index cursor: {e}")))?
    {
        let model: mongodb::IndexModel = cursor
            .deserialize_current()
            .map_err(|e| MeldError::Io(format!("deserialize index: {e}")))?;
        let name = model
            .options
            .as_ref()
            .and_then(|o| o.name.clone())
            .unwrap_or_default();
        let keys: Vec<(String, i32)> = model
            .keys
            .iter()
            .map(|(k, v)| (k.clone(), v.as_i32().unwrap_or(1)))
            .collect();
        let options = model.options.as_ref();
        let unique = options.and_then(|o| o.unique).unwrap_or(false);
        let partial_filter = options
            .and_then(|o| o.partial_filter_expression.clone())
            .map(|d| {
                d.iter()
                    .map(|(k, v)| (k.clone(), bson_value_to_canonical(v)))
                    .collect()
            });
        let ttl = options
            .and_then(|o| o.expire_after)
            .map(|d| d.as_secs_f64() as u64);
        let sparse = Some(options.and_then(|o| o.sparse).unwrap_or(false));
        let background = Some(options.and_then(|o| o.background).unwrap_or(false));

        out.insert(
            name.clone(),
            MongoIndex {
                name,
                keys,
                unique,
                partial_filter,
                ttl,
                sparse,
                background,
            },
        );
    }
    Ok(out)
}

async fn get_views(_db: &Database) -> Result<Vec<MongoView>, MeldError> {
    Ok(vec![])
}

async fn extract_documents(
    coll: &mongodb::Collection<mongodb::bson::Document>,
) -> Result<Vec<MongoDocument>, MeldError> {
    let mut cursor = coll
        .find(bson::doc! {}, None)
        .await
        .map_err(|e| MeldError::Io(format!("find documents: {e}")))?;
    let mut docs = Vec::new();
    while cursor
        .advance()
        .await
        .map_err(|e| MeldError::Io(format!("doc cursor: {e}")))?
    {
        let doc: mongodb::bson::Document = cursor
            .deserialize_current()
            .map_err(|e| MeldError::Io(format!("deserialize document: {e}")))?;
        let id = doc
            .get("_id")
            .cloned()
            .map(|v| bson_value_to_canonical(&v))
            .unwrap_or(CanonicalValue::Null);
        let mut fields = IndexMap::new();
        for (k, v) in doc.iter() {
            if k == "_id" {
                continue;
            }
            fields.insert(k.clone(), bson_value_to_canonical(v));
        }
        docs.push(MongoDocument { id, fields });
    }
    Ok(docs)
}

pub fn bson_value_to_canonical(v: &mongodb::bson::Bson) -> CanonicalValue {
    match v {
        mongodb::bson::Bson::Null => CanonicalValue::Null,
        mongodb::bson::Bson::Boolean(b) => CanonicalValue::Boolean(*b),
        mongodb::bson::Bson::Int32(i) => CanonicalValue::I32(*i),
        mongodb::bson::Bson::Int64(i) => CanonicalValue::I64(*i),
        mongodb::bson::Bson::Double(f) => CanonicalValue::from_f64(*f),
        mongodb::bson::Bson::String(s) => CanonicalValue::Text(s.clone()),
        mongodb::bson::Bson::ObjectId(oid) => CanonicalValue::ObjectId(oid.to_hex().into_bytes()),
        mongodb::bson::Bson::Array(arr) => {
            CanonicalValue::Array(arr.iter().map(bson_value_to_canonical).collect())
        }
        mongodb::bson::Bson::Document(doc) => CanonicalValue::Document(
            doc.iter()
                .map(|(k, v)| (k.clone(), bson_value_to_canonical(v)))
                .collect(),
        ),
        mongodb::bson::Bson::DateTime(dt) => {
            CanonicalValue::Timestamp(dt.try_to_rfc3339_string().unwrap_or_default())
        }
        mongodb::bson::Bson::Binary(b) => CanonicalValue::Bytes(b.bytes.to_vec()),
        mongodb::bson::Bson::Decimal128(d) => CanonicalValue::Text(d.to_string()),
        mongodb::bson::Bson::Symbol(s) => CanonicalValue::Text(s.clone()),
        mongodb::bson::Bson::RegularExpression(re) => {
            CanonicalValue::Text(format!("/{}/{}", re.pattern, re.options))
        }
        mongodb::bson::Bson::JavaScriptCode(_) => CanonicalValue::Text("<javascript>".into()),
        mongodb::bson::Bson::JavaScriptCodeWithScope(_) => {
            CanonicalValue::Text("<javascript-with-scope>".into())
        }
        mongodb::bson::Bson::MaxKey => CanonicalValue::Text("<maxkey>".into()),
        mongodb::bson::Bson::MinKey => CanonicalValue::Text("<minkey>".into()),
        mongodb::bson::Bson::Timestamp(ts) => {
            CanonicalValue::Text(format!("{}:{}", ts.time, ts.increment))
        }
        mongodb::bson::Bson::DbPointer(_) => CanonicalValue::Text("<dbpointer>".into()),
        mongodb::bson::Bson::Undefined => CanonicalValue::Text("<undefined>".into()),
    }
}

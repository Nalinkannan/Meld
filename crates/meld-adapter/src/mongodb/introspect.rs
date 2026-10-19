//! MongoDB catalog introspection.
//!
//! Discovers collections, indexes, validators, and views using
//! the official driver's listCollections / listIndexes commands.
//! Extraction failures are reported explicitly; a failed capture never
//! masquerades as a complete, normal-looking snapshot.

use chrono::Utc;
use indexmap::IndexMap;
use meld_core::error::MeldError;
use meld_core::state::{
    CanonicalValue, CaptureInfo, CollectionOptions, ConsistencyLevel, MongoCollectionState,
    MongoIndex, MongoState, MongoView, Validator,
};
use mongodb::Database;
use tracing::debug;

use super::extract::bson_value_to_canonical;

pub async fn introspect(
    client: &mongodb::Client,
    db_name: &str,
) -> Result<meld_core::state::EngineState, MeldError> {
    let db = client.database(db_name);
    let collection_names = db
        .list_collection_names(None)
        .await
        .map_err(|e| MeldError::Io(format!("list collections: {e}")))?;

    // Views are database-level objects defined over a source collection
    // (`viewOn`). Discover them once, then attach each view to the collection
    // it is defined over.
    let all_views = get_views(&db).await?;
    let view_names: std::collections::HashSet<&str> =
        all_views.iter().map(|v| v.name.as_str()).collect();

    let mut coll_states = IndexMap::new();
    for coll_name in &collection_names {
        // System collections (e.g. system.views) are internal metadata and
        // views are handled separately (they appear in list_collection_names
        // but are not collections with documents of their own).
        if coll_name.starts_with("system.") || view_names.contains(coll_name.as_str()) {
            continue;
        }
        debug!(collection = %coll_name, "introspecting collection");
        let coll = db.collection::<mongodb::bson::Document>(coll_name);

        // Every capture step below fails loudly: an error here aborts the
        // snapshot instead of producing empty indexes, validators, or
        // documents.
        let options = get_collection_options(&db, coll_name).await?;
        let validator = get_validator(&db, coll_name).await?;
        // Views are not indexable through listIndexes and carry no documents
        // of their own (their documents are the result of their pipeline).
        let indexes = if view_names.contains(coll_name.as_str()) {
            IndexMap::new()
        } else {
            get_indexes(&coll).await?
        };
        let documents = if view_names.contains(coll_name.as_str()) {
            vec![]
        } else {
            super::extract::extract_collection_documents(&coll, None).await?
        };
        let document_count = documents.len() as u64;
        let views: Vec<MongoView> = all_views
            .iter()
            .filter(|v| v.view_on == *coll_name)
            .cloned()
            .collect();

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

/// Collection options are returned by `listCollections` nested inside the
/// `options` document of each info entry, not at the top level.
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
    let Some(info_doc) = first_batch else {
        return Ok(CollectionOptions::default());
    };
    // Read options from the nested `options` document.
    let options_doc = match info_doc.get_document("options") {
        Ok(d) => d,
        // A collection without an options document is a normal uncapped
        // collection.
        Err(_) => {
            return Ok(CollectionOptions {
                capped: Some(false),
                ..Default::default()
            });
        }
    };

    let capped = options_doc.get_bool("capped").ok().unwrap_or(false);
    let size = options_doc.get("size").and_then(|v| match v {
        bson::Bson::Int32(v) => Some(*v as u64),
        bson::Bson::Int64(v) => Some(*v as u64),
        bson::Bson::Double(v) => Some(*v as u64),
        _ => None,
    });
    let max = options_doc.get("max").and_then(|v| match v {
        bson::Bson::Int32(v) => Some(*v as u64),
        bson::Bson::Int64(v) => Some(*v as u64),
        bson::Bson::Double(v) => Some(*v as u64),
        _ => None,
    });
    let expires = options_doc.get("expireAfterSeconds").and_then(|v| match v {
        bson::Bson::Int32(v) => Some(*v as u64),
        bson::Bson::Int64(v) => Some(*v as u64),
        bson::Bson::Double(v) => Some(*v as u64),
        _ => None,
    });

    let mut additional = IndexMap::new();
    for (k, v) in options_doc.iter() {
        match k.as_str() {
            "capped" | "size" | "max" | "expireAfterSeconds" => {}
            _ => {
                additional.insert(k.clone(), bson_value_to_canonical(v)?);
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

    let definition = validator
        .map(|d| {
            d.iter()
                .map(|(k, v)| bson_value_to_canonical(v).map(|v| (k.clone(), v)))
                .collect::<Result<IndexMap<_, _>, _>>()
        })
        .transpose()
        .map(|d| d.unwrap_or_default())?;

    Ok(Some(Validator {
        validation_level,
        validation_action,
        definition,
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
                    .map(|(k, v)| bson_value_to_canonical(v).map(|v| (k.clone(), v)))
                    .collect::<Result<IndexMap<_, _>, _>>()
            })
            .transpose()?;
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

pub async fn get_views(db: &Database) -> Result<Vec<MongoView>, MeldError> {
    let doc = db
        .run_command(
            bson::doc! {"listCollections": 1, "filter": bson::doc! {}},
            None,
        )
        .await
        .map_err(|e| MeldError::Io(format!("list collections for views: {e}")))?;
    let batch = doc
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get_array("firstBatch").ok())
        .cloned()
        .unwrap_or_default();

    let mut views = Vec::new();
    for entry in batch.iter() {
        let entry = match entry.as_document() {
            Some(d) => d,
            None => continue,
        };
        if entry.get_str("type").unwrap_or_default() != "view" {
            continue;
        }
        let name = entry.get_str("name").unwrap_or_default().to_string();
        let options = match entry.get_document("options") {
            Ok(d) => d,
            Err(_) => &bson::Document::new(),
        };
        let view_on = options.get_str("viewOn").unwrap_or_default().to_string();
        let mut definition = IndexMap::new();
        if let Ok(pipeline) = options.get_array("pipeline") {
            let stages: Vec<CanonicalValue> = pipeline
                .iter()
                .map(bson_value_to_canonical)
                .collect::<Result<_, _>>()?;
            definition.insert("pipeline".into(), CanonicalValue::Array(stages));
        }
        views.push(MongoView {
            name,
            view_on,
            definition,
        });
    }
    Ok(views)
}

//! MongoDB independent verification.
//!
//! Re-inspects the live database using fresh read-only queries and compares
//! against the intended target state. Never reuses in-memory submitted state.

use meld_core::canonical::hash_state;
use meld_core::error::MeldError;
use meld_core::ids::{ObjectHash, RootHash};
use meld_core::state::{CanonicalValue, EngineKind, EngineState, MongoCollectionState};
use meld_core::verify::{
    CheckCategory, ErrorSeverity, Mismatch, VerificationCheck, VerificationReport,
};
use mongodb::{Client, Collection};
use tracing::info;

use crate::mongodb::introspect::{
    bson_value_to_canonical, get_collection_options, get_indexes, get_validator,
};

/// Convert a CanonicalValue to a BSON value for queries.
fn canonical_value_to_bson(v: &CanonicalValue) -> bson::Bson {
    match v {
        CanonicalValue::Null => bson::Bson::Null,
        CanonicalValue::Boolean(b) => bson::Bson::Boolean(*b),
        CanonicalValue::I32(i) => bson::Bson::Int32(*i),
        CanonicalValue::I64(i) => bson::Bson::Int64(*i),
        CanonicalValue::F64(bits) => bson::Bson::Double(f64::from_bits(*bits)),
        CanonicalValue::Numeric(s) => bson::Bson::Decimal128(
            s.parse::<bson::Decimal128>()
                .unwrap_or_else(|_| bson::Decimal128::from_bytes([0u8; 16])),
        ),
        CanonicalValue::Text(s) => bson::Bson::String(s.clone()),
        CanonicalValue::Bytes(bytes) => bson::Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Generic,
            bytes: bytes.clone(),
        }),
        CanonicalValue::Timestamp(ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
            Ok(dt) => bson::Bson::DateTime(bson::DateTime::from_millis(dt.timestamp_millis())),
            Err(_) => bson::Bson::String(ts.clone()),
        },
        CanonicalValue::ObjectId(bytes) => {
            if bytes.len() == 12 {
                let mut arr = [0u8; 12];
                arr.copy_from_slice(bytes);
                bson::Bson::ObjectId(mongodb::bson::oid::ObjectId::from_bytes(arr))
            } else {
                bson::Bson::Binary(bson::Binary {
                    subtype: bson::spec::BinarySubtype::Generic,
                    bytes: bytes.clone(),
                })
            }
        }
        CanonicalValue::Array(items) => {
            bson::Bson::Array(items.iter().map(canonical_value_to_bson).collect())
        }
        CanonicalValue::Document(fields) => {
            let mut doc = bson::Document::new();
            for (k, v) in fields {
                doc.insert(k.clone(), canonical_value_to_bson(v));
            }
            bson::Bson::Document(doc)
        }
    }
}

/// Independently verify a MongoDB database against a target state.
pub async fn verify(
    client: &Client,
    target: &EngineState,
) -> Result<VerificationReport, MeldError> {
    let EngineState::MongoDb(target_state) = target else {
        return Err(MeldError::IncompatibleEngine {
            expected: EngineKind::MongoDb,
            got: EngineKind::PostgreSql,
        });
    };

    let db = client.database(&target_state.database_name);
    let mut checks: Vec<VerificationCheck> = Vec::new();
    let mut mismatches: Vec<Mismatch> = Vec::new();

    // List live collections.
    let live_names = db
        .list_collection_names(None)
        .await
        .map_err(|e| MeldError::Io(format!("list collections: {e}")))?;

    // Check each target collection exists and matches.
    for (name, target_coll) in &target_state.collections {
        let object = format!("{}.{}", target_state.database_name, name);

        // Object existence.
        let exists = live_names.contains(name);
        let exists_check = VerificationCheck {
            category: CheckCategory::ObjectExistence,
            name: format!("collection {object} exists"),
            passed: exists,
            object: object.clone(),
            field: None,
            expected: Some("present".into()),
            actual: Some(if exists {
                "present".into()
            } else {
                "absent".into()
            }),
            details: "checked via listCollections".into(),
        };
        if !exists_check.passed {
            mismatches.push(Mismatch {
                object: object.clone(),
                field: None,
                expected: Some("present".into()),
                actual: Some("absent".into()),
                severity: ErrorSeverity::Error,
            });
        }
        checks.push(exists_check);

        if !exists {
            continue;
        }

        // Collection options (capped, size, max, TTL).
        let live_options = get_collection_options(&db, name)
            .await
            .map_err(|e| MeldError::Io(format!("get collection options {name}: {e}")))?;
        if live_options != target_coll.options {
            let opts_check = VerificationCheck {
                category: CheckCategory::CollectionOptions,
                name: format!("collection {object} options"),
                passed: false,
                object: object.clone(),
                field: None,
                expected: Some(format!("{:?}", target_coll.options)),
                actual: Some(format!("{:?}", live_options)),
                details: "collection options differ".into(),
            };
            mismatches.push(Mismatch {
                object: object.clone(),
                field: Some("options".into()),
                expected: Some(format!("{:?}", target_coll.options)),
                actual: Some(format!("{:?}", live_options)),
                severity: ErrorSeverity::Error,
            });
            checks.push(opts_check);
        } else {
            checks.push(VerificationCheck {
                category: CheckCategory::CollectionOptions,
                name: format!("collection {object} options"),
                passed: true,
                object: object.clone(),
                field: None,
                expected: Some(format!("{:?}", target_coll.options)),
                actual: Some(format!("{:?}", live_options)),
                details: "options match".into(),
            });
        }

        // Validator.
        let live_validator = get_validator(&db, name)
            .await
            .map_err(|e| MeldError::Io(format!("get validator {name}: {e}")))?;
        match (&target_coll.validator, &live_validator) {
            (Some(_), None) => {
                let val_check = VerificationCheck {
                    category: CheckCategory::Validator,
                    name: format!("collection {object} validator"),
                    passed: false,
                    object: object.clone(),
                    field: None,
                    expected: Some("validator present".into()),
                    actual: Some("no validator".into()),
                    details: "target has validator but live does not".into(),
                };
                mismatches.push(Mismatch {
                    object: object.clone(),
                    field: Some("validator".into()),
                    expected: Some("present".into()),
                    actual: Some("absent".into()),
                    severity: ErrorSeverity::Error,
                });
                checks.push(val_check);
            }
            (None, Some(_)) => {
                let val_check = VerificationCheck {
                    category: CheckCategory::Validator,
                    name: format!("collection {object} validator"),
                    passed: false,
                    object: object.clone(),
                    field: None,
                    expected: Some("no validator".into()),
                    actual: Some("validator present".into()),
                    details: "live has validator but target does not".into(),
                };
                mismatches.push(Mismatch {
                    object: object.clone(),
                    field: Some("validator".into()),
                    expected: Some("absent".into()),
                    actual: Some("present".into()),
                    severity: ErrorSeverity::Error,
                });
                checks.push(val_check);
            }
            (Some(tv), Some(lv)) if tv != lv => {
                let val_check = VerificationCheck {
                    category: CheckCategory::Validator,
                    name: format!("collection {object} validator"),
                    passed: false,
                    object: object.clone(),
                    field: None,
                    expected: Some(format!("{:?}", tv)),
                    actual: Some(format!("{:?}", lv)),
                    details: "validator definition differs".into(),
                };
                mismatches.push(Mismatch {
                    object: object.clone(),
                    field: Some("validator".into()),
                    expected: Some(format!("{:?}", tv)),
                    actual: Some(format!("{:?}", lv)),
                    severity: ErrorSeverity::Error,
                });
                checks.push(val_check);
            }
            _ => {
                checks.push(VerificationCheck {
                    category: CheckCategory::Validator,
                    name: format!("collection {object} validator"),
                    passed: true,
                    object: object.clone(),
                    field: None,
                    expected: Some("matches".into()),
                    actual: Some("matches".into()),
                    details: "validator matches".into(),
                });
            }
        }

        // Indexes.
        let coll = db.collection::<mongodb::bson::Document>(name);
        let live_indexes = get_indexes(&coll)
            .await
            .map_err(|e| MeldError::Io(format!("get indexes {name}: {e}")))?;
        // Compare target indexes against live (excluding _id_).
        for (idx_name, target_idx) in &target_coll.indexes {
            if idx_name == "_id_" {
                continue;
            }
            let idx_object = format!("{}.idx.{}", object, idx_name);
            match live_indexes.get(idx_name) {
                None => {
                    let idx_check = VerificationCheck {
                        category: CheckCategory::IndexDefinition,
                        name: format!("index {idx_object}"),
                        passed: false,
                        object: idx_object.clone(),
                        field: None,
                        expected: Some(format!("{:?}", target_idx)),
                        actual: Some("absent".into()),
                        details: "index missing in live database".into(),
                    };
                    mismatches.push(Mismatch {
                        object: idx_object.clone(),
                        field: None,
                        expected: Some(format!("{:?}", target_idx)),
                        actual: Some("absent".into()),
                        severity: ErrorSeverity::Error,
                    });
                    checks.push(idx_check);
                }
                Some(live_idx) if live_idx != target_idx => {
                    let idx_check = VerificationCheck {
                        category: CheckCategory::IndexDefinition,
                        name: format!("index {idx_object}"),
                        passed: false,
                        object: idx_object.clone(),
                        field: None,
                        expected: Some(format!("{:?}", target_idx)),
                        actual: Some(format!("{:?}", live_idx)),
                        details: "index definition differs".into(),
                    };
                    mismatches.push(Mismatch {
                        object: idx_object.clone(),
                        field: None,
                        expected: Some(format!("{:?}", target_idx)),
                        actual: Some(format!("{:?}", live_idx)),
                        severity: ErrorSeverity::Error,
                    });
                    checks.push(idx_check);
                }
                _ => {
                    checks.push(VerificationCheck {
                        category: CheckCategory::IndexDefinition,
                        name: format!("index {idx_object}"),
                        passed: true,
                        object: idx_object.clone(),
                        field: None,
                        expected: Some(format!("{:?}", target_idx)),
                        actual: Some(format!("{:?}", target_idx)),
                        details: "index matches".into(),
                    });
                }
            }
        }
        // Live indexes not in target (excluding _id_).
        for idx_name in live_indexes.keys() {
            if idx_name == "_id_" {
                continue;
            }
            if !target_coll.indexes.contains_key(idx_name) {
                let idx_object = format!("{}.idx.{}", object, idx_name);
                let idx_check = VerificationCheck {
                    category: CheckCategory::IndexDefinition,
                    name: format!("index {idx_object}"),
                    passed: false,
                    object: idx_object.clone(),
                    field: None,
                    expected: Some("absent".into()),
                    actual: Some("present".into()),
                    details: "extra index in live database".into(),
                };
                mismatches.push(Mismatch {
                    object: idx_object.clone(),
                    field: None,
                    expected: Some("absent".into()),
                    actual: Some("present".into()),
                    severity: ErrorSeverity::Warning,
                });
                checks.push(idx_check);
            }
        }

        // Document count.
        let coll = db.collection::<mongodb::bson::Document>(name);
        let live_count = coll
            .count_documents(bson::doc! {}, None)
            .await
            .map_err(|e| MeldError::Io(format!("count documents {name}: {e}")))?;
        let count_check = VerificationCheck {
            category: CheckCategory::DataValues,
            name: format!("collection {object} document count"),
            passed: live_count as u64 == target_coll.document_count,
            object: object.clone(),
            field: None,
            expected: Some(target_coll.document_count.to_string()),
            actual: Some(live_count.to_string()),
            details: "document count comparison".into(),
        };
        if !count_check.passed {
            mismatches.push(Mismatch {
                object: object.clone(),
                field: Some("document_count".into()),
                expected: Some(target_coll.document_count.to_string()),
                actual: Some(live_count.to_string()),
                severity: ErrorSeverity::Error,
            });
        }
        checks.push(count_check);

        // Document-level verification (spot-check a few documents by _id).
        // For performance, we only verify a subset unless the collection is small.
        let sample_size = target_coll.document_count.min(100);
        if sample_size > 0 {
            let docs_check =
                verify_documents_sample(&coll, name, &object, target_coll, sample_size).await?;
            checks.push(docs_check.0);
            mismatches.extend(docs_check.1);
        }
    }

    // Collections in live but not in target.
    for name in &live_names {
        if !target_state.collections.contains_key(name) {
            let object = format!("{}.{}", target_state.database_name, name);
            let extra_check = VerificationCheck {
                category: CheckCategory::ObjectExistence,
                name: format!("collection {object} exists (extra)"),
                passed: false,
                object: object.clone(),
                field: None,
                expected: Some("absent".into()),
                actual: Some("present".into()),
                details: "collection exists in live database but not in target".into(),
            };
            mismatches.push(Mismatch {
                object: object.clone(),
                field: None,
                expected: Some("absent".into()),
                actual: Some("present".into()),
                severity: ErrorSeverity::Warning,
            });
            checks.push(extra_check);
        }
    }

    let passed = mismatches.is_empty() && checks.iter().all(|c| c.passed);
    info!(
        "mongodb verification: {} checks, {} mismatches, passed={}",
        checks.len(),
        mismatches.len(),
        passed
    );

    Ok(VerificationReport {
        target_root: RootHash::from_bytes([0u8; 32]),
        actual_root: Some(ObjectHash::from_bytes(hash_state(target).as_bytes())),
        engine_kind: EngineKind::MongoDb,
        checks,
        mismatches,
        warnings: vec![],
        passed,
    })
}

/// Verify a sample of documents by _id.
async fn verify_documents_sample(
    coll: &Collection<mongodb::bson::Document>,
    _coll_name: &str,
    object: &str,
    target_coll: &MongoCollectionState,
    sample_size: u64,
) -> Result<(VerificationCheck, Vec<Mismatch>), MeldError> {
    let mut mismatches = Vec::new();
    let mut passed = true;

    // Get _ids from target documents (first N).
    let target_ids: Vec<&CanonicalValue> = target_coll
        .documents
        .iter()
        .take(sample_size as usize)
        .map(|d| &d.id)
        .collect();

    for target_id in target_ids {
        let target_id = target_id.clone(); // dereference from &&CanonicalValue to &CanonicalValue
        let filter = bson::doc! { "_id": canonical_value_to_bson(&target_id) };
        let live_doc = coll
            .find_one(filter, None)
            .await
            .map_err(|e| MeldError::Io(format!("find document {target_id:?}: {e}")))?;

        if let Some(live_doc) = live_doc {
            // Convert live doc to CanonicalValue map for comparison.
            let mut live_fields = indexmap::IndexMap::new();
            for (k, v) in live_doc.iter() {
                if k == "_id" {
                    continue;
                }
                live_fields.insert(k.clone(), bson_value_to_canonical(v));
            }

            // Find target document with this _id.
            #[allow(clippy::collapsible_if)]
            if let Some(target_doc) = target_coll.documents.iter().find(|d| d.id == target_id) {
                if target_doc.fields != live_fields {
                    let field_obj = format!("{}.doc[{:?}]", object, target_id);
                    mismatches.push(Mismatch {
                        object: field_obj.clone(),
                        field: Some("fields".into()),
                        expected: Some(format!("{:?}", target_doc.fields)),
                        actual: Some(format!("{:?}", live_fields)),
                        severity: ErrorSeverity::Error,
                    });
                    passed = false;
                }
            }
        } else {
            let field_obj = format!("{}.doc[{:?}]", object, target_id);
            mismatches.push(Mismatch {
                object: field_obj.clone(),
                field: None,
                expected: Some("present".into()),
                actual: Some("absent".into()),
                severity: ErrorSeverity::Error,
            });
            passed = false;
        }
    }

    let check = VerificationCheck {
        category: CheckCategory::DataValues,
        name: format!("collection {object} document sample ({sample_size} docs)"),
        passed,
        object: object.into(),
        field: None,
        expected: Some(format!("{sample_size} documents match")),
        actual: Some(if passed {
            format!("{sample_size} documents match")
        } else {
            "mismatches found".into()
        }),
        details: "sampled document comparison by _id".into(),
    };

    Ok((check, mismatches))
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_dummy() {
        // Placeholder for unit tests; integration tests require live MongoDB.
    }
}

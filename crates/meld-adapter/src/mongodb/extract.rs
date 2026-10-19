//! MongoDB document extraction.
//!
//! Documents are extracted through the official driver with an explicit batch
//! size so large collections are read incrementally. Every failure is
//! reported -- extraction never silently returns an empty or partial document
//! list.

use indexmap::IndexMap;
use meld_core::error::MeldError;
use meld_core::state::{CanonicalValue, MongoDocument};
use mongodb::bson::Document;
use tracing::debug;

/// Extract every document from `coll`, honoring an optional batch size.
pub async fn extract_collection_documents(
    coll: &mongodb::Collection<Document>,
    batch_size: Option<u64>,
) -> Result<Vec<MongoDocument>, MeldError> {
    let filter = bson::doc! {};
    let options = batch_size.map(|size| {
        mongodb::options::FindOptions::builder()
            .batch_size(Some(size as u32))
            .build()
    });
    let mut cursor = coll
        .find(filter, options)
        .await
        .map_err(|e| MeldError::Io(format!("find documents: {e}")))?;
    let mut docs = Vec::new();
    while cursor
        .advance()
        .await
        .map_err(|e| MeldError::Io(format!("document cursor: {e}")))?
    {
        let doc: Document = cursor
            .deserialize_current()
            .map_err(|e| MeldError::Io(format!("deserialize document: {e}")))?;
        let id = match doc.get("_id") {
            Some(v) => bson_value_to_canonical(v)?,
            None => CanonicalValue::Null,
        };
        let mut fields = IndexMap::new();
        for (k, v) in doc.iter() {
            if k == "_id" {
                continue;
            }
            fields.insert(k.clone(), bson_value_to_canonical(v)?);
        }
        docs.push(MongoDocument { id, fields });
    }
    debug!(count = docs.len(), "extracted documents");
    Ok(docs)
}

/// Convert a BSON value into a type-aware canonical value. BSON type
/// distinctions are preserved: a string `_id` and an ObjectId `_id` remain
/// distinct, and extended BSON types (Decimal128, binary subtypes, BSON
/// timestamps, regular expressions, JavaScript with scope, MinKey, MaxKey)
/// round-trip through explicit canonical variants rather than being replaced
/// by lossy text placeholders.
pub fn bson_value_to_canonical(v: &mongodb::bson::Bson) -> Result<CanonicalValue, MeldError> {
    match v {
        mongodb::bson::Bson::Null => Ok(CanonicalValue::Null),
        mongodb::bson::Bson::Boolean(b) => Ok(CanonicalValue::Boolean(*b)),
        mongodb::bson::Bson::Int32(i) => Ok(CanonicalValue::I32(*i)),
        mongodb::bson::Bson::Int64(i) => Ok(CanonicalValue::I64(*i)),
        mongodb::bson::Bson::Double(f) => Ok(CanonicalValue::from_f64(*f)),
        mongodb::bson::Bson::String(s) => Ok(CanonicalValue::Text(s.clone())),
        mongodb::bson::Bson::ObjectId(oid) => {
            // 12 raw bytes, never the ASCII hex encoding.
            Ok(CanonicalValue::ObjectId(oid.bytes().to_vec()))
        }
        mongodb::bson::Bson::Array(arr) => Ok(CanonicalValue::Array(
            arr.iter()
                .map(bson_value_to_canonical)
                .collect::<Result<_, _>>()?,
        )),
        mongodb::bson::Bson::Document(doc) => Ok(CanonicalValue::Document(
            doc.iter()
                .map(|(k, v)| bson_value_to_canonical(v).map(|v| (k.clone(), v)))
                .collect::<Result<IndexMap<_, _>, _>>()?,
        )),
        mongodb::bson::Bson::DateTime(dt) => Ok(CanonicalValue::Timestamp(
            dt.try_to_rfc3339_string()
                .map_err(|e| MeldError::Serialization(format!("invalid BSON datetime: {e}")))?,
        )),
        mongodb::bson::Bson::Binary(b) => {
            let subtype = u8::from(b.subtype);
            // BSON binary subtype 4 (UUID) with exactly 16 bytes is a UUID,
            // not an opaque byte blob. Other subtypes (legacy 3, generic 0,
            // etc.) stay Binary with their subtype preserved.
            if subtype == u8::from(bson::spec::BinarySubtype::Uuid) && b.bytes.len() == 16 {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(&b.bytes);
                Ok(CanonicalValue::Uuid(arr))
            } else {
                Ok(CanonicalValue::Binary {
                    subtype,
                    bytes: b.bytes.to_vec(),
                })
            }
        }
        mongodb::bson::Bson::Decimal128(d) => Ok(CanonicalValue::Decimal128(d.bytes())),
        mongodb::bson::Bson::Symbol(s) => Ok(CanonicalValue::Text(s.clone())),
        mongodb::bson::Bson::RegularExpression(re) => Ok(CanonicalValue::Regex {
            pattern: re.pattern.clone(),
            options: re.options.clone(),
        }),
        mongodb::bson::Bson::JavaScriptCode(code) => Ok(CanonicalValue::JavaScript {
            code: code.clone(),
            scope: None,
        }),
        mongodb::bson::Bson::JavaScriptCodeWithScope(jscs) => Ok(CanonicalValue::JavaScript {
            code: jscs.code.clone(),
            scope: Some(
                jscs.scope
                    .iter()
                    .map(|(k, v)| bson_value_to_canonical(v).map(|v| (k.clone(), v)))
                    .collect::<Result<IndexMap<_, _>, _>>()?,
            ),
        }),
        mongodb::bson::Bson::MaxKey => Ok(CanonicalValue::MaxKey),
        mongodb::bson::Bson::MinKey => Ok(CanonicalValue::MinKey),
        mongodb::bson::Bson::Timestamp(ts) => Ok(CanonicalValue::BsonTimestamp {
            timestamp: ts.time,
            increment: ts.increment,
        }),
        // Deprecated/opaque BSON types cannot be represented losslessly;
        // refuse them explicitly instead of substituting a text placeholder.
        mongodb::bson::Bson::DbPointer(_) => Err(MeldError::Unsupported(
            "BSON DbPointer is not supported in canonical state".into(),
        )),
        mongodb::bson::Bson::Undefined => Err(MeldError::Unsupported(
            "BSON Undefined is not supported in canonical state".into(),
        )),
    }
}

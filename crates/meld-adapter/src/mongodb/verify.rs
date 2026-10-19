//! MongoDB independent verification.
//!
//! Re-inspects the live database using fresh read-only queries and compares
//! against the intended target state. Never reuses in-memory submitted state.
//! Verification is exact: every collection, validator, index, view, and
//! document is compared -- no sampling. A changed document outside any
//! sample is detected.

use meld_core::canonical::hash_state;
use meld_core::diff::diff_states;
use meld_core::error::MeldError;
use meld_core::ids::{ObjectHash, RootHash};
use meld_core::state::{EngineKind, EngineState};
use meld_core::verify::{
    CheckCategory, ErrorSeverity, Mismatch, VerificationCheck, VerificationReport,
};
use mongodb::Client;
use tracing::info;

/// Epoch used to normalize capture metadata before comparing state hashes.
/// Capture timestamps and warnings describe *how* a state was captured, not
/// the logical database state, so they must not influence verification.
const VERIFY_EPOCH_SECS: i64 = 0;

/// Remove capture metadata (timestamps, warnings) so hashes reflect only the
/// logical database state.
fn normalize_mongo_state(state: &mut EngineState) {
    if let EngineState::MongoDb(s) = state {
        s.capture_info.captured_at =
            chrono::DateTime::from_timestamp(VERIFY_EPOCH_SECS, 0).unwrap_or_else(chrono::Utc::now);
        s.warnings.clear();
    }
}

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

    let mut checks: Vec<VerificationCheck> = Vec::new();
    let mut mismatches: Vec<Mismatch> = Vec::new();

    // Fresh, independent read-only inspection of the live database.
    let live_state = super::introspect::introspect(client, &target_state.database_name).await?;
    let EngineState::MongoDb(live_state) = live_state else {
        return Err(MeldError::Internal(
            "introspection returned wrong engine kind".into(),
        ));
    };

    // Structural diff: every reported operation is a real mismatch between
    // target and live (collections, options, validators, indexes, views,
    // and every document -- no sampling).
    match diff_states(target, &EngineState::MongoDb(live_state.clone())) {
        Ok(diff) => {
            if diff.is_empty() {
                checks.push(VerificationCheck {
                    category: CheckCategory::StateHash,
                    name: "catalog and documents match target".into(),
                    passed: true,
                    object: "database".into(),
                    field: None,
                    expected: Some("identical".into()),
                    actual: Some("identical".into()),
                    details: "no differences found in full exact comparison".into(),
                });
            } else {
                for op in &diff.ops {
                    let object = op.object.to_string();
                    checks.push(VerificationCheck {
                        category: CheckCategory::DataValues,
                        name: format!("{} {}", op.kind, op.object),
                        passed: false,
                        object: object.clone(),
                        field: op.field.clone(),
                        expected: op.before.as_ref().map(|v| format!("{v:?}")),
                        actual: op.after.as_ref().map(|v| format!("{v:?}")),
                        details: op.reason.clone(),
                    });
                    mismatches.push(Mismatch {
                        object,
                        field: op.field.clone(),
                        expected: op.before.as_ref().map(|v| format!("{v:?}")),
                        actual: op.after.as_ref().map(|v| format!("{v:?}")),
                        severity: ErrorSeverity::Error,
                    });
                }
            }
        }
        Err(e) => {
            let object = String::from("database");
            checks.push(VerificationCheck {
                category: CheckCategory::SchemaEquivalence,
                name: "target and live states are comparable".into(),
                passed: false,
                object: object.clone(),
                field: None,
                expected: Some("comparable".into()),
                actual: Some(format!("{e}")),
                details: "could not diff target against live state".into(),
            });
            mismatches.push(Mismatch {
                object,
                field: None,
                expected: Some("comparable".into()),
                actual: Some(format!("{e}")),
                severity: ErrorSeverity::Critical,
            });
        }
    }

    // Canonical state-hash comparison on normalized states (capture metadata
    // removed). Compare target and live hashes only when both have the same
    // scope, normalization rules, and canonicalization version.
    let hash_ok = {
        let mut target_norm = target.clone();
        let mut live_norm = EngineState::MongoDb(live_state.clone());
        normalize_mongo_state(&mut target_norm);
        normalize_mongo_state(&mut live_norm);
        hash_state(&target_norm) == hash_state(&live_norm)
    };
    checks.push(VerificationCheck {
        category: CheckCategory::StateHash,
        name: "canonical state hash matches target".into(),
        passed: hash_ok,
        object: "database".into(),
        field: None,
        expected: Some("normalized target hash".into()),
        actual: Some(if hash_ok {
            "matches".into()
        } else {
            "differs".into()
        }),
        details: "compares fresh live inspection against the captured target state".into(),
    });
    if !hash_ok {
        mismatches.push(Mismatch {
            object: "database".into(),
            field: None,
            expected: Some("target canonical hash".into()),
            actual: Some("live canonical hash differs".into()),
            severity: ErrorSeverity::Error,
        });
    }

    let passed = mismatches.is_empty() && checks.iter().all(|c| c.passed);

    // Both roots are computed from their respective states, normalized the
    // same way (capture metadata removed), so they are comparable.
    let mut target_root_state = target.clone();
    let mut actual_root_state = EngineState::MongoDb(live_state.clone());
    normalize_mongo_state(&mut target_root_state);
    normalize_mongo_state(&mut actual_root_state);
    let target_root = RootHash::from_bytes(hash_state(&target_root_state).as_bytes());
    let actual_root = Some(ObjectHash::from_bytes(
        hash_state(&actual_root_state).as_bytes(),
    ));

    info!(
        "mongodb verification: {} checks, {} mismatches, passed={}",
        checks.len(),
        mismatches.len(),
        passed
    );

    Ok(VerificationReport {
        target_root,
        actual_root,
        engine_kind: EngineKind::MongoDb,
        checks,
        mismatches,
        warnings: vec![],
        passed,
    })
}

#[cfg(test)]
mod tests {}

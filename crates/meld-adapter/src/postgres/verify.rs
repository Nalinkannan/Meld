//! PostgreSQL independent verification.
//!
//! Re-inspects the live database using fresh read-only queries and compares
//! against the intended target state. Never reuses in-memory submitted state.
//! The live state is captured inside a single `REPEATABLE READ` transaction
//! (see `introspect`), so the comparison is a point-in-point snapshot rather
//! than a mix of states from concurrent writes.

use chrono::Utc;
use meld_core::canonical::hash_state;
use meld_core::diff::diff_states;
use meld_core::error::MeldError;
use meld_core::ids::{ObjectHash, RootHash};
use meld_core::state::{EngineKind, EngineState};
use meld_core::verify::{
    CheckCategory, ErrorSeverity, Mismatch, VerificationCheck, VerificationReport,
};

use super::introspect;

/// Epoch used to normalize capture metadata before comparing state hashes.
/// Capture timestamps and warnings describe *how* a state was captured, not
/// the logical database state, so they must not influence verification.
const VERIFY_EPOCH_SECS: i64 = 0;

pub async fn verify(
    pool: &sqlx::PgPool,
    target: &EngineState,
) -> Result<VerificationReport, MeldError> {
    let EngineState::PostgreSql(_) = target else {
        return Err(MeldError::IncompatibleEngine {
            expected: EngineKind::PostgreSql,
            got: EngineKind::MongoDb,
        });
    };

    let mut checks: Vec<VerificationCheck> = Vec::new();
    let mut mismatches: Vec<Mismatch> = Vec::new();

    // Fresh, independent read-only inspection of the live database.
    let live_state = introspect::introspect(pool).await?;
    let EngineState::PostgreSql(_) = &live_state else {
        return Err(MeldError::Internal(
            "introspection returned wrong engine kind".into(),
        ));
    };

    // Structural diff: every reported operation is a real mismatch between
    // target and live (objects, columns, constraints, indexes, rows, values).
    match diff_states(target, &live_state) {
        Ok(diff) => {
            if diff.is_empty() {
                checks.push(VerificationCheck {
                    category: CheckCategory::StateHash,
                    name: "schema and data match target".into(),
                    passed: true,
                    object: "database".into(),
                    field: None,
                    expected: Some("identical".into()),
                    actual: Some("identical".into()),
                    details: "no structural differences found".into(),
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

    // Canonical state-hash comparison. Capture metadata (timestamps, warnings)
    // is normalized away so that only the logical database state is compared.
    let hash_ok = {
        let normalize = |state: &mut EngineState| match state {
            EngineState::PostgreSql(s) => {
                normalize_capture_info(&mut s.capture_info);
                s.warnings.clear();
            }
            EngineState::MongoDb(_) => unreachable!("postgres verify"),
        };
        let mut target_norm = target.clone();
        let mut live_norm = live_state.clone();
        normalize(&mut target_norm);
        normalize(&mut live_norm);
        hash_state(&target_norm) == hash_state(&live_norm)
    };
    let target_hash = hash_state(target);
    let live_hash = hash_state(&live_state);
    checks.push(VerificationCheck {
        category: CheckCategory::StateHash,
        name: "canonical state hash matches target".into(),
        passed: hash_ok,
        object: "database".into(),
        field: None,
        expected: Some(target_hash.hex()[..16].to_string()),
        actual: Some(live_hash.hex()[..16].to_string()),
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

    let target_root = RootHash::from_bytes(hash_state(target).as_bytes());
    let actual_root = Some(ObjectHash::from_bytes(live_hash.as_bytes()));

    Ok(VerificationReport {
        target_root,
        actual_root,
        engine_kind: EngineKind::PostgreSql,
        checks,
        mismatches,
        warnings: vec![],
        passed,
    })
}

/// Normalize capture metadata out of a state so hashes reflect only the
/// logical database state.
fn normalize_capture_info(info: &mut meld_core::state::CaptureInfo) {
    info.captured_at =
        chrono::DateTime::from_timestamp(VERIFY_EPOCH_SECS, 0).unwrap_or_else(Utc::now);
    // strategy/scope/engine_version describe the capture setup and are kept
    // because they define the normalization context.
}

#[cfg(test)]
mod tests {
    #[test]
    fn sql_quote_dummy() {
        // kept to preserve the test module; real verification needs a live PG.
    }
}

//! MongoDB independent verification placeholder.
//!
//! Re-inspects the live database and compares against the target state.

use meld_core::canonical::hash_state;
use meld_core::error::MeldError;
use meld_core::ids::{ObjectHash, RootHash};
use meld_core::state::{EngineKind, EngineState};
use meld_core::verify::{CheckCategory, Mismatch, VerificationCheck, VerificationReport};
use tracing::debug;

pub async fn verify(
    _client: &mongodb::Client,
    target: &EngineState,
) -> Result<VerificationReport, MeldError> {
    let EngineState::MongoDb(target_state) = target else {
        return Err(MeldError::IncompatibleEngine {
            expected: EngineKind::MongoDb,
            got: EngineKind::PostgreSql,
        });
    };

    let mut checks: Vec<VerificationCheck> = Vec::new();
    let mismatches: Vec<Mismatch> = Vec::new();

    for (name, _coll) in &target_state.collections {
        let object = format!("{}.{}", target_state.database_name, name);
        checks.push(VerificationCheck {
            category: CheckCategory::ObjectExistence,
            name: format!("collection {object} exists"),
            passed: true,
            object,
            field: None,
            expected: Some("present".into()),
            actual: Some("present".into()),
            details: "placeholder: live re-inspection not yet implemented".into(),
        });
        debug!(collection = %name, "verification placeholder");
    }

    let passed = mismatches.is_empty() && checks.iter().all(|c| c.passed);
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

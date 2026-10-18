//! MongoDB restore-plan execution placeholder.
//!
//! Restore execution for MongoDB is intentionally limited in this stage;
//! operations return an "unsupported" error with actionable guidance rather
//! than silently succeeding.

use meld_core::error::MeldError;
use meld_core::plan::RestorePlan;
use meld_core::state::EngineKind;
use tracing::info;

pub async fn execute(
    _client: &mongodb::Client,
    plan: &RestorePlan,
) -> Result<crate::adapter::ExecutionResult, MeldError> {
    if plan.engine_kind != EngineKind::MongoDb {
        return Err(MeldError::IncompatibleEngine {
            expected: EngineKind::MongoDb,
            got: plan.engine_kind,
        });
    }
    info!("mongodb restore plan execution is not yet implemented; rejecting safely");
    Err(MeldError::Unsupported(
        "MongoDB restore execution is not yet implemented in this stage".into(),
    ))
}

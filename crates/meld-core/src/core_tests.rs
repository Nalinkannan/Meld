//! Cross-cutting core-domain tests.

use crate::conflict::{Conflict, ConflictReport};
use crate::diff::diff_states;
use crate::ids::{CommitHash, ObjectHash, RootHash};
use crate::merge::{MergeInput, analyze_merge};
use crate::plan::{OperationRisk, RestorePlan};
use crate::state::{
    CaptureInfo, ConsistencyLevel, EngineKind, EngineState, MongoState, PostgresState,
};
use crate::transition::{ChangeKind, ChangeOp};
use crate::verify::{CheckCategory, ErrorSeverity, Mismatch, VerificationReport};
use chrono::Utc;
use indexmap::IndexMap;

#[test]
fn cross_engine_diff_rejected() {
    let pg = EngineState::PostgreSql(PostgresState {
        engine_version: "18".into(),
        databases: IndexMap::new(),
        warnings: vec![],
        capture_info: capture_info("pg"),
    });
    let mongo = EngineState::MongoDb(MongoState {
        engine_version: "8".into(),
        database_name: "db".into(),
        collections: IndexMap::new(),
        warnings: vec![],
        capture_info: capture_info("mongo"),
    });
    let err = diff_states(&pg, &mongo).unwrap_err();
    assert!(matches!(
        err,
        crate::error::MeldError::IncompatibleEngine { .. }
    ));
}

#[test]
fn merge_cross_engine_rejected() {
    let pg = EngineState::PostgreSql(PostgresState {
        engine_version: "18".into(),
        databases: IndexMap::new(),
        warnings: vec![],
        capture_info: capture_info("pg"),
    });
    let mongo = EngineState::MongoDb(MongoState {
        engine_version: "8".into(),
        database_name: "db".into(),
        collections: IndexMap::new(),
        warnings: vec![],
        capture_info: capture_info("mongo"),
    });
    let analysis = analyze_merge(&MergeInput {
        ancestor: CommitHash::ZERO,
        left: CommitHash::ZERO,
        right: CommitHash::ZERO,
        ancestor_state: pg,
        left_state: mongo.clone(),
        right_state: mongo,
    });
    assert!(!analysis.conflicts.is_empty());
}

#[test]
fn verification_failure_is_not_reported_as_success() {
    let report = VerificationReport::new(RootHash::from_bytes([1; 32]), EngineKind::PostgreSql)
        .with_checks(vec![crate::verify::VerificationCheck {
            category: CheckCategory::DataValues,
            name: "row count".into(),
            passed: false,
            object: "public.t".into(),
            field: None,
            expected: Some("10".into()),
            actual: Some("9".into()),
            details: "one row missing".into(),
        }]);
    assert!(!report.passed);
    assert!(!report.all_checks_passed());
    assert_eq!(report.mismatches[0].severity, ErrorSeverity::Error);
}

#[test]
fn mismatch_typed_information() {
    let m = Mismatch {
        object: "public.t".into(),
        field: Some("col".into()),
        expected: Some("5".into()),
        actual: Some("7".into()),
        severity: ErrorSeverity::Critical,
    };
    assert_eq!(m.field.as_deref(), Some("col"));
    assert_eq!(m.severity, ErrorSeverity::Critical);
}

#[test]
fn destructive_plan_requires_confirmation() {
    let plan = RestorePlan::new(
        CommitHash::ZERO,
        ObjectHash::from_bytes([1; 32]),
        EngineKind::MongoDb,
    );
    let _ = plan;
    let op = crate::plan::PlanOp {
        id: "drop".into(),
        kind: crate::plan::PlanOpKind::DropObject,
        object: crate::identity::ObjectIdentity::MongoDb(
            crate::identity::MongoObjectId::Collection { name: "c".into() },
        ),
        risk: OperationRisk::PotentiallyIrreversible,
        depends_on: vec![],
        reason: "drop collection".into(),
        requires_confirmation: true,
        details: None,
    };
    assert!(op.requires_confirmation);
    assert!(op.risk.is_destructive());
}

#[test]
fn conflict_report_classification() {
    let conflict = Conflict::SameObjectChangedIncompatible {
        object: crate::identity::ObjectIdentity::PostgreSql(
            crate::identity::PostgresObjectId::Table {
                schema: "public".into(),
                table: "t".into(),
            },
        ),
        left_summary: "add col".into(),
        right_summary: "drop col".into(),
    };
    let report = ConflictReport::new(conflict);
    assert_eq!(report.suggested.len(), 2);
}

#[test]
fn change_op_is_destructive() {
    let drop = ChangeOp::new(
        ChangeKind::DropTable,
        crate::identity::ObjectIdentity::PostgreSql(crate::identity::PostgresObjectId::Table {
            schema: "public".into(),
            table: "t".into(),
        }),
        "drop",
    );
    assert!(drop.is_destructive());
}

fn capture_info(engine: &str) -> CaptureInfo {
    CaptureInfo {
        consistency: ConsistencyLevel::Snapshot,
        strategy: "test".into(),
        captured_at: Utc::now(),
        engine_version: engine.into(),
        scope: "db".into(),
        warnings: vec![],
    }
}

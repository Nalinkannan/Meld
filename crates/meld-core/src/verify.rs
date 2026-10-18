//! Independent verification primitives.
//!
//! Verification compares the actual live database state against an intended
//! target state using fresh read-only inspection, independently of the plan
//! executor.

use crate::ids::{ObjectHash, RootHash};
use crate::state::EngineKind;
use serde::{Deserialize, Serialize};

/// The result of an independent verification pass.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerificationReport {
    pub target_root: RootHash,
    pub actual_root: Option<ObjectHash>,
    pub engine_kind: EngineKind,
    pub checks: Vec<VerificationCheck>,
    pub mismatches: Vec<Mismatch>,
    pub warnings: Vec<String>,
    pub passed: bool,
}

impl VerificationReport {
    pub fn new(target_root: RootHash, engine_kind: EngineKind) -> Self {
        Self {
            target_root,
            actual_root: None,
            engine_kind,
            checks: vec![],
            mismatches: vec![],
            warnings: vec![],
            passed: false,
        }
    }

    pub fn with_checks(mut self, checks: Vec<VerificationCheck>) -> Self {
        let mut passed = true;
        let mut mismatches = vec![];
        for check in &checks {
            if !check.passed {
                passed = false;
                mismatches.push(Mismatch {
                    object: check.object.clone(),
                    field: check.field.clone(),
                    expected: check.expected.clone(),
                    actual: check.actual.clone(),
                    severity: ErrorSeverity::Error,
                });
            }
        }
        self.checks = checks;
        self.mismatches = mismatches;
        self.passed = passed && self.mismatches.is_empty();
        self
    }

    pub fn all_checks_passed(&self) -> bool {
        self.passed && self.mismatches.is_empty() && self.checks.iter().all(|c| c.passed)
    }
}

/// A single verification check.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerificationCheck {
    pub category: CheckCategory,
    pub name: String,
    pub passed: bool,
    pub object: String,
    pub field: Option<String>,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub details: String,
}

/// A mismatch between expected and actual state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mismatch {
    pub object: String,
    pub field: Option<String>,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub severity: ErrorSeverity,
}

/// Categories of verification checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CheckCategory {
    ObjectExistence,
    SchemaEquivalence,
    ColumnDefinition,
    IndexDefinition,
    CollectionOptions,
    Validator,
    DataValues,
    RecordIdentities,
    StateHash,
    CaptureLimitations,
}

impl std::fmt::Display for CheckCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckCategory::ObjectExistence => write!(f, "object-existence"),
            CheckCategory::SchemaEquivalence => write!(f, "schema-equivalence"),
            CheckCategory::ColumnDefinition => write!(f, "column-definition"),
            CheckCategory::IndexDefinition => write!(f, "index-definition"),
            CheckCategory::CollectionOptions => write!(f, "collection-options"),
            CheckCategory::Validator => write!(f, "validator"),
            CheckCategory::DataValues => write!(f, "data-values"),
            CheckCategory::RecordIdentities => write!(f, "record-identities"),
            CheckCategory::StateHash => write!(f, "state-hash"),
            CheckCategory::CaptureLimitations => write!(f, "capture-limitations"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ErrorSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

impl std::fmt::Display for ErrorSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorSeverity::Info => write!(f, "info"),
            ErrorSeverity::Warning => write!(f, "warning"),
            ErrorSeverity::Error => write!(f, "error"),
            ErrorSeverity::Critical => write!(f, "critical"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_passes_when_all_checks_pass() {
        let report = VerificationReport::new(RootHash::from_bytes([1; 32]), EngineKind::PostgreSql)
            .with_checks(vec![VerificationCheck {
                category: CheckCategory::ObjectExistence,
                name: "table t exists".into(),
                passed: true,
                object: "public.t".into(),
                field: None,
                expected: Some("present".into()),
                actual: Some("present".into()),
                details: "ok".into(),
            }]);
        assert!(report.passed);
        assert!(report.all_checks_passed());
        assert!(report.mismatches.is_empty());
    }

    #[test]
    fn report_fails_on_mismatch() {
        let report = VerificationReport::new(RootHash::from_bytes([1; 32]), EngineKind::MongoDb)
            .with_checks(vec![VerificationCheck {
                category: CheckCategory::DataValues,
                name: "document count".into(),
                passed: false,
                object: "users".into(),
                field: None,
                expected: Some("5".into()),
                actual: Some("3".into()),
                details: "2 documents missing".into(),
            }]);
        assert!(!report.passed);
        assert!(!report.all_checks_passed());
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].severity, ErrorSeverity::Error);
    }
}

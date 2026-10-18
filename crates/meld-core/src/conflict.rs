//! Conflict classification and resolution for three-way merge.

use crate::identity::ObjectIdentity;
use crate::state::CanonicalValue;
use serde::{Deserialize, Serialize};

/// A conflict detected during a three-way merge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Conflict {
    /// The same object was changed on both sides in incompatible ways.
    SameObjectChangedIncompatible {
        object: ObjectIdentity,
        left_summary: String,
        right_summary: String,
    },
    /// One side deleted an object the other side modified.
    DeleteVersusModify {
        object: ObjectIdentity,
        deleted_side: Side,
    },
    /// Incompatible schema changes (e.g. column type changed differently).
    IncompatibleSchemaChange {
        object: ObjectIdentity,
        field: Option<String>,
        left: Option<CanonicalValue>,
        right: Option<CanonicalValue>,
    },
    /// A duplicate key / uniqueness constraint would be violated.
    DuplicateKey {
        object: ObjectIdentity,
        key: CanonicalValue,
    },
    /// A constraint or dependency would be violated by the merge.
    ConstraintOrDependencyViolation {
        object: ObjectIdentity,
        constraint: String,
    },
    /// Incompatible index changes.
    IncompatibleIndexChange {
        object: ObjectIdentity,
        index: String,
    },
    /// Incompatible document field changes.
    IncompatibleDocumentField {
        collection: String,
        document_id: CanonicalValue,
        field: String,
    },
    /// Incompatible array changes.
    IncompatibleArrayChange {
        collection: String,
        document_id: CanonicalValue,
        field: String,
    },
    /// Object identity is missing or ambiguous.
    MissingOrAmbiguousIdentity { object: String, reason: String },
    /// The change cannot be merged automatically.
    UnsupportedAutomaticMerge {
        object: ObjectIdentity,
        reason: String,
    },
}

/// Which side of the merge a change originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Left,
    Right,
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Side::Left => write!(f, "left"),
            Side::Right => write!(f, "right"),
        }
    }
}

/// A proposed resolution for a conflict.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConflictResolution {
    /// Take the left side's version.
    TakeLeft,
    /// Take the right side's version.
    TakeRight,
    /// A manually supplied resolved value.
    Manual(CanonicalValue),
    /// Defer the conflict (leave unresolved).
    Deferred,
}

impl ConflictResolution {
    pub fn is_resolved(&self) -> bool {
        matches!(
            self,
            ConflictResolution::TakeLeft
                | ConflictResolution::TakeRight
                | ConflictResolution::Manual(_)
        )
    }
}

/// The outcome of conflict classification for a single object/field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConflictReport {
    pub conflict: Conflict,
    pub suggested: Vec<ConflictResolution>,
    pub resolved: Option<ConflictResolution>,
    pub validated: bool,
}

impl ConflictReport {
    pub fn new(conflict: Conflict) -> Self {
        let suggested = match &conflict {
            Conflict::SameObjectChangedIncompatible { .. } => {
                vec![ConflictResolution::TakeLeft, ConflictResolution::TakeRight]
            }
            Conflict::DeleteVersusModify { .. } => {
                vec![ConflictResolution::TakeLeft, ConflictResolution::TakeRight]
            }
            Conflict::IncompatibleSchemaChange { .. } => {
                vec![ConflictResolution::TakeLeft, ConflictResolution::TakeRight]
            }
            Conflict::DuplicateKey { .. } => {
                vec![ConflictResolution::TakeLeft, ConflictResolution::TakeRight]
            }
            Conflict::IncompatibleIndexChange { .. } => {
                vec![ConflictResolution::TakeLeft, ConflictResolution::TakeRight]
            }
            Conflict::IncompatibleDocumentField { .. } => {
                vec![ConflictResolution::TakeLeft, ConflictResolution::TakeRight]
            }
            Conflict::IncompatibleArrayChange { .. } => {
                vec![ConflictResolution::TakeLeft, ConflictResolution::TakeRight]
            }
            Conflict::MissingOrAmbiguousIdentity { .. } => {
                vec![ConflictResolution::Deferred]
            }
            Conflict::UnsupportedAutomaticMerge { .. } => vec![ConflictResolution::Deferred],
            Conflict::ConstraintOrDependencyViolation { .. } => {
                vec![ConflictResolution::Deferred]
            }
        };
        Self {
            conflict,
            suggested,
            resolved: None,
            validated: false,
        }
    }

    pub fn is_resolved(&self) -> bool {
        matches!(self.resolved, Some(ref r) if r.is_resolved())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{MongoObjectId, PostgresObjectId};

    #[test]
    fn conflict_report_resolution() {
        let conflict = Conflict::SameObjectChangedIncompatible {
            object: ObjectIdentity::PostgreSql(PostgresObjectId::Table {
                schema: "public".into(),
                table: "t".into(),
            }),
            left_summary: "add column a".into(),
            right_summary: "drop column a".into(),
        };
        let mut report = ConflictReport::new(conflict);
        assert!(!report.is_resolved());
        assert_eq!(report.suggested.len(), 2);
        report.resolved = Some(ConflictResolution::TakeLeft);
        report.validated = true;
        assert!(report.is_resolved());
        assert!(report.validated);
    }

    #[test]
    fn deferred_conflict_not_resolved() {
        let conflict = Conflict::MissingOrAmbiguousIdentity {
            object: "row".into(),
            reason: "no primary key".into(),
        };
        let report = ConflictReport::new(conflict);
        assert_eq!(report.suggested, vec![ConflictResolution::Deferred]);
        assert!(!report.is_resolved());
    }

    #[test]
    fn string_and_objectid_mongo_ids_distinct() {
        let text = MongoObjectId::Document {
            collection: "c".into(),
            id: CanonicalValue::Text("abc".into()),
        };
        let oid = MongoObjectId::Document {
            collection: "c".into(),
            id: CanonicalValue::ObjectId(hex_decode("507f1f77bcf86cd799439011")),
        };
        assert_ne!(text, oid);
    }

    fn hex_decode(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }
}

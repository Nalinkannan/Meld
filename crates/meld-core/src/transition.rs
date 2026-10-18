//! Semantic change operations.
//!
//! These are typed, engine-scoped operations describing the difference
//! between two snapshots. They are the basis for diffs, restore planning,
//! merge conflict detection, and verification.

use crate::canonical::Canonicalize;
use crate::identity::ObjectIdentity;
use crate::state::{CanonicalValue, EngineKind};
use serde::{Deserialize, Serialize};

/// A single semantic change between two states.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangeOp {
    pub kind: ChangeKind,
    pub object: ObjectIdentity,
    /// The field or sub-object affected (e.g. a column name, index name,
    /// or document field path), when applicable.
    pub field: Option<String>,
    /// Confidence in the change classification (e.g. rename detection),
    /// expressed as parts-per-thousand (0..=1000).
    pub confidence: u32,
    /// Before value, when known.
    pub before: Option<CanonicalValue>,
    /// After value, when known.
    pub after: Option<CanonicalValue>,
    /// Human-readable reason for the change.
    pub reason: String,
}

impl ChangeOp {
    pub fn new(kind: ChangeKind, object: ObjectIdentity, reason: impl Into<String>) -> Self {
        Self {
            kind,
            object,
            field: None,
            confidence: 1000,
            before: None,
            after: None,
            reason: reason.into(),
        }
    }

    pub fn is_destructive(&self) -> bool {
        matches!(
            self.kind,
            ChangeKind::DropTable
                | ChangeKind::DropColumn
                | ChangeKind::DropIndex
                | ChangeKind::DropConstraint
                | ChangeKind::DropCollection
                | ChangeKind::DeleteRow
                | ChangeKind::DeleteDocument
        )
    }

    pub fn engine_kind(&self) -> EngineKind {
        self.object.engine_kind()
    }
}

/// The kind of semantic change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeKind {
    // --- PostgreSQL schema ---
    CreateTable,
    DropTable,
    RenameTable { confidence: u32 },
    AddColumn,
    DropColumn,
    RenameColumn { confidence: u32 },
    AlterColumn,
    CreateIndex,
    DropIndex,
    AlterIndex,
    AddConstraint,
    DropConstraint,
    AlterConstraint,
    CreateView,
    DropView,
    AlterView,
    CreateSequence,
    DropSequence,
    AlterSequence,
    // --- PostgreSQL data ---
    InsertRow,
    UpdateRow,
    DeleteRow,
    // --- MongoDB catalog ---
    CreateCollection,
    DropCollection,
    AlterCollectionOptions,
    ChangeValidator,
    CreateMongoIndex,
    DropMongoIndex,
    AlterMongoIndex,
    // --- MongoDB data ---
    InsertDocument,
    UpdateDocument,
    DeleteDocument,
    // --- Generic ---
    Unsupported { reason: String },
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeKind::CreateTable => write!(f, "create table"),
            ChangeKind::DropTable => write!(f, "drop table"),
            ChangeKind::RenameTable { .. } => write!(f, "rename table"),
            ChangeKind::AddColumn => write!(f, "add column"),
            ChangeKind::DropColumn => write!(f, "drop column"),
            ChangeKind::RenameColumn { .. } => write!(f, "rename column"),
            ChangeKind::AlterColumn => write!(f, "alter column"),
            ChangeKind::CreateIndex => write!(f, "create index"),
            ChangeKind::DropIndex => write!(f, "drop index"),
            ChangeKind::AlterIndex => write!(f, "alter index"),
            ChangeKind::AddConstraint => write!(f, "add constraint"),
            ChangeKind::DropConstraint => write!(f, "drop constraint"),
            ChangeKind::AlterConstraint => write!(f, "alter constraint"),
            ChangeKind::CreateSequence => write!(f, "create sequence"),
            ChangeKind::DropSequence => write!(f, "drop sequence"),
            ChangeKind::AlterSequence => write!(f, "alter sequence"),
            ChangeKind::CreateView => write!(f, "create view"),
            ChangeKind::DropView => write!(f, "drop view"),
            ChangeKind::AlterView => write!(f, "alter view"),
            ChangeKind::InsertRow => write!(f, "insert row"),
            ChangeKind::UpdateRow => write!(f, "update row"),
            ChangeKind::DeleteRow => write!(f, "delete row"),
            ChangeKind::CreateCollection => write!(f, "create collection"),
            ChangeKind::DropCollection => write!(f, "drop collection"),
            ChangeKind::AlterCollectionOptions => write!(f, "alter collection options"),
            ChangeKind::ChangeValidator => write!(f, "change validator"),
            ChangeKind::CreateMongoIndex => write!(f, "create index"),
            ChangeKind::DropMongoIndex => write!(f, "drop index"),
            ChangeKind::AlterMongoIndex => write!(f, "alter index"),
            ChangeKind::InsertDocument => write!(f, "insert document"),
            ChangeKind::UpdateDocument => write!(f, "update document"),
            ChangeKind::DeleteDocument => write!(f, "delete document"),
            ChangeKind::Unsupported { reason } => write!(f, "unsupported: {reason}"),
        }
    }
}

/// Aggregated summary of a set of change operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ChangeSummary {
    pub objects_created: u64,
    pub objects_dropped: u64,
    pub objects_altered: u64,
    pub rows_inserted: u64,
    pub rows_updated: u64,
    pub rows_deleted: u64,
    pub documents_inserted: u64,
    pub documents_updated: u64,
    pub documents_deleted: u64,
    pub warnings: Vec<String>,
}

impl ChangeSummary {
    pub fn from_ops(ops: &[ChangeOp]) -> Self {
        let mut s = ChangeSummary::default();
        for op in ops {
            match op.kind {
                ChangeKind::CreateTable | ChangeKind::CreateCollection => {
                    s.objects_created += 1;
                }
                ChangeKind::DropTable | ChangeKind::DropCollection => {
                    s.objects_dropped += 1;
                }
                ChangeKind::InsertRow => s.rows_inserted += 1,
                ChangeKind::UpdateRow => s.rows_updated += 1,
                ChangeKind::DeleteRow => s.rows_deleted += 1,
                ChangeKind::InsertDocument => s.documents_inserted += 1,
                ChangeKind::UpdateDocument => s.documents_updated += 1,
                ChangeKind::DeleteDocument => s.documents_deleted += 1,
                _ => {
                    s.objects_altered += 1;
                }
            }
        }
        s
    }

    pub fn total_changes(&self) -> u64 {
        self.objects_created
            + self.objects_dropped
            + self.objects_altered
            + self.rows_inserted
            + self.rows_updated
            + self.rows_deleted
            + self.documents_inserted
            + self.documents_updated
            + self.documents_deleted
    }
}

impl Canonicalize for ChangeSummary {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        use crate::canonical::{write_sequence, write_u64};
        out.push(0xc2);
        write_u64(out, self.objects_created);
        write_u64(out, self.objects_dropped);
        write_u64(out, self.objects_altered);
        write_u64(out, self.rows_inserted);
        write_u64(out, self.rows_updated);
        write_u64(out, self.rows_deleted);
        write_u64(out, self.documents_inserted);
        write_u64(out, self.documents_updated);
        write_u64(out, self.documents_deleted);
        write_sequence(
            out,
            &self
                .warnings
                .iter()
                .map(|w| CanonicalValue::Text(w.clone()))
                .collect::<Vec<_>>(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::PostgresObjectId;

    #[test]
    fn summary_from_ops_counts_correctly() {
        let ops = vec![
            ChangeOp::new(
                ChangeKind::CreateTable,
                ObjectIdentity::PostgreSql(PostgresObjectId::Table {
                    schema: "public".into(),
                    table: "t".into(),
                }),
                "created",
            ),
            ChangeOp::new(
                ChangeKind::InsertRow,
                ObjectIdentity::PostgreSql(PostgresObjectId::Row {
                    schema: "public".into(),
                    table: "t".into(),
                    identity: vec![],
                }),
                "inserted",
            ),
            ChangeOp::new(
                ChangeKind::DropTable,
                ObjectIdentity::PostgreSql(PostgresObjectId::Table {
                    schema: "public".into(),
                    table: "old".into(),
                }),
                "dropped",
            ),
        ];
        let s = ChangeSummary::from_ops(&ops);
        assert_eq!(s.objects_created, 1);
        assert_eq!(s.objects_dropped, 1);
        assert_eq!(s.rows_inserted, 1);
        assert_eq!(s.total_changes(), 3);
    }

    #[test]
    fn destructive_detection() {
        let drop = ChangeOp::new(
            ChangeKind::DropTable,
            ObjectIdentity::PostgreSql(PostgresObjectId::Table {
                schema: "public".into(),
                table: "t".into(),
            }),
            "dropped",
        );
        assert!(drop.is_destructive());
        let add = ChangeOp::new(
            ChangeKind::AddColumn,
            ObjectIdentity::PostgreSql(PostgresObjectId::Column {
                schema: "public".into(),
                table: "t".into(),
                column: "c".into(),
            }),
            "added",
        );
        assert!(!add.is_destructive());
    }
}

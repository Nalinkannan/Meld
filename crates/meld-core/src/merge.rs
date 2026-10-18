//! Three-way merge analysis.

use crate::conflict::{Conflict, ConflictReport, ConflictResolution};
use crate::ids::CommitHash;
use crate::state::EngineState;
use crate::transition::{ChangeOp, ChangeSummary};

/// Input to a three-way merge: the common ancestor and the two divergent tips.
#[derive(Debug, Clone)]
pub struct MergeInput {
    pub ancestor: CommitHash,
    pub left: CommitHash,
    pub right: CommitHash,
    pub ancestor_state: EngineState,
    pub left_state: EngineState,
    pub right_state: EngineState,
}

/// The result of merge analysis, before resolution and application.
#[derive(Debug, Clone)]
pub struct MergeAnalysis {
    pub ancestor: CommitHash,
    pub left: CommitHash,
    pub right: CommitHash,
    pub left_changes: Vec<ChangeOp>,
    pub right_changes: Vec<ChangeOp>,
    pub conflicts: Vec<ConflictReport>,
    pub auto_mergeable: Vec<ChangeOp>,
    pub engine_kind: crate::state::EngineKind,
}

/// A proposed merged state, produced only after all conflicts are resolved.
#[derive(Debug, Clone)]
pub struct ProposedMerge {
    pub state: EngineState,
    pub ops: Vec<ChangeOp>,
    pub summary: ChangeSummary,
}

/// Perform conflict detection between the left and right change sets relative
/// to the ancestor.
pub fn analyze_merge(input: &MergeInput) -> MergeAnalysis {
    if input.ancestor_state.engine_kind() != input.left_state.engine_kind()
        || input.ancestor_state.engine_kind() != input.right_state.engine_kind()
    {
        // Cross-engine merge: every change is unsupported.
        let mut conflicts = vec![ConflictReport::new(Conflict::UnsupportedAutomaticMerge {
            object: crate::identity::ObjectIdentity::PostgreSql(
                crate::identity::PostgresObjectId::Schema {
                    name: "cross-engine".into(),
                },
            ),
            reason: "cannot merge PostgreSQL and MongoDB states".into(),
        })];
        conflicts[0].resolved = Some(ConflictResolution::Deferred);
        return MergeAnalysis {
            ancestor: input.ancestor,
            left: input.left,
            right: input.right,
            left_changes: vec![],
            right_changes: vec![],
            conflicts,
            auto_mergeable: vec![],
            engine_kind: input.ancestor_state.engine_kind(),
        };
    }

    let engine_kind = input.ancestor_state.engine_kind();

    // Compute semantic changes from ancestor to each side.
    let left_changes = crate::diff::diff_states(&input.ancestor_state, &input.left_state)
        .map(|d| d.ops)
        .unwrap_or_default();
    let right_changes = crate::diff::diff_states(&input.ancestor_state, &input.right_state)
        .map(|d| d.ops)
        .unwrap_or_default();

    let mut conflicts: Vec<ConflictReport> = vec![];
    let mut auto_mergeable: Vec<ChangeOp> = vec![];

    let right_by_object = index_by_object(&right_changes);
    let left_by_object = index_by_object(&left_changes);

    // Left-side ops: compatible with right -> auto-mergeable, otherwise conflict.
    for op in &left_changes {
        if let Some(right_ops) = right_by_object.get(&op.object) {
            let conflicting = right_ops.iter().any(|ro| changes_conflict(op, ro));
            if conflicting {
                conflicts.push(ConflictReport::new(
                    Conflict::SameObjectChangedIncompatible {
                        object: op.object.clone(),
                        left_summary: op.kind.to_string(),
                        right_summary: right_ops
                            .iter()
                            .map(|o| o.kind.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                    },
                ));
            } else {
                auto_mergeable.push(op.clone());
                for ro in right_ops {
                    auto_mergeable.push((*ro).clone());
                }
            }
        } else {
            auto_mergeable.push(op.clone());
        }
    }

    // Right-only ops (not touched by left).
    for op in &right_changes {
        if !left_by_object.contains_key(&op.object) {
            auto_mergeable.push(op.clone());
        }
    }

    MergeAnalysis {
        ancestor: input.ancestor,
        left: input.left,
        right: input.right,
        left_changes,
        right_changes,
        conflicts,
        auto_mergeable,
        engine_kind,
    }
}

fn index_by_object(
    ops: &[ChangeOp],
) -> std::collections::HashMap<crate::identity::ObjectIdentity, Vec<&ChangeOp>> {
    let mut map: std::collections::HashMap<crate::identity::ObjectIdentity, Vec<&ChangeOp>> =
        std::collections::HashMap::new();
    for op in ops {
        map.entry(op.object.clone()).or_default().push(op);
    }
    map
}

fn changes_conflict(left: &ChangeOp, right: &ChangeOp) -> bool {
    // Destructive vs destructive on the same object/field -> conflict.
    if left.is_destructive() && right.is_destructive() {
        return true;
    }
    // Delete vs modify -> conflict.
    if (left.is_destructive() && !right.is_destructive())
        || (!left.is_destructive() && right.is_destructive())
    {
        return true;
    }
    // Same field altered differently -> conflict.
    if left.field == right.field && left.field.is_some() && left.before != right.before {
        return true;
    }
    false
}

/// Apply a resolution to produce a proposed merged state. Validation of the
/// result is left to the caller/engine.
pub fn apply_resolution(
    _analysis: &MergeAnalysis,
    _resolutions: &[crate::identity::ObjectIdentity],
) -> Result<ProposedMerge, crate::error::MeldError> {
    Err(crate::error::MeldError::Unsupported(
        "proposed merge application must be executed by the engine".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        ConsistencyLevel, EngineState, PgColumn, PgDatabase, PgSchema, PgTable, PostgresState,
    };
    use chrono::Utc;
    use indexmap::IndexMap;

    fn pg_state() -> EngineState {
        let mut columns = IndexMap::new();
        columns.insert(
            "a".into(),
            PgColumn {
                name: "a".into(),
                ordinal_position: 1,
                data_type: crate::state::PgType {
                    name: "int".into(),
                    oid: Some(23),
                    internal_type: Some("int4".into()),
                    array: false,
                    length: None,
                    precision: None,
                    scale: None,
                },
                is_nullable: false,
                column_default: None,
                is_identity: false,
                is_generated: false,
                generated_expression: None,
                comment: None,
            },
        );
        let mut tables = IndexMap::new();
        tables.insert(
            "public".into(),
            PgSchema {
                name: "public".into(),
                tables: IndexMap::from([("t".into(), pg_table("t", columns))]),
            },
        );
        let mut dbs = IndexMap::new();
        dbs.insert(
            "mydb".into(),
            PgDatabase {
                name: "mydb".into(),
                schemas: tables,
                views: IndexMap::new(),
                sequences: IndexMap::new(),
            },
        );
        EngineState::PostgreSql(PostgresState {
            engine_version: "18".into(),
            databases: dbs,
            warnings: vec![],
            capture_info: crate::state::CaptureInfo {
                consistency: ConsistencyLevel::Snapshot,
                strategy: "test".into(),
                captured_at: Utc::now(),
                engine_version: "18".into(),
                scope: "mydb".into(),
                warnings: vec![],
            },
        })
    }

    fn pg_table(name: &str, columns: IndexMap<String, PgColumn>) -> PgTable {
        PgTable {
            name: name.into(),
            schema: "public".into(),
            columns,
            primary_key: None,
            unique_constraints: vec![],
            foreign_keys: vec![],
            check_constraints: vec![],
            indexes: IndexMap::new(),
            comment: None,
            row_count: 0,
        }
    }

    #[test]
    fn non_conflicting_changes_are_auto_mergeable() {
        let ancestor = pg_state();
        let left = pg_state();
        let right = pg_state();
        let input = MergeInput {
            ancestor: CommitHash::ZERO,
            left: CommitHash::ZERO,
            right: CommitHash::ZERO,
            ancestor_state: ancestor.clone(),
            left_state: left.clone(),
            right_state: right.clone(),
        };
        let analysis = analyze_merge(&input);
        assert!(analysis.conflicts.is_empty());
    }

    #[test]
    fn cross_engine_merge_rejected() {
        let ancestor = pg_state();
        let mongo = EngineState::MongoDb(crate::state::MongoState {
            engine_version: "8".into(),
            database_name: "mydb".into(),
            collections: Default::default(),
            warnings: vec![],
            capture_info: crate::state::CaptureInfo {
                consistency: ConsistencyLevel::Snapshot,
                strategy: "test".into(),
                captured_at: Utc::now(),
                engine_version: "8".into(),
                scope: "mydb".into(),
                warnings: vec![],
            },
        });
        let input = MergeInput {
            ancestor: CommitHash::ZERO,
            left: CommitHash::ZERO,
            right: CommitHash::ZERO,
            ancestor_state: ancestor,
            left_state: mongo.clone(),
            right_state: mongo,
        };
        let analysis = analyze_merge(&input);
        assert!(!analysis.conflicts.is_empty());
    }

    #[test]
    fn same_object_destructive_change_conflict_detected() {
        // Ancestor has table "t"; both sides drop it -> same-object conflict.
        let ancestor = table_state_with_table();
        let without_table = table_state_without_table();

        let input = MergeInput {
            ancestor: CommitHash::ZERO,
            left: CommitHash::ZERO,
            right: CommitHash::ZERO,
            ancestor_state: ancestor.clone(),
            left_state: without_table.clone(),
            right_state: without_table,
        };
        let analysis = analyze_merge(&input);
        assert!(
            analysis
                .conflicts
                .iter()
                .any(|c| matches!(c.conflict, Conflict::SameObjectChangedIncompatible { .. })),
            "expected same-object conflict, got {:?}",
            analysis.conflicts
        );
    }

    fn table_state_with_table() -> EngineState {
        let mut columns = IndexMap::new();
        columns.insert(
            "a".into(),
            PgColumn {
                name: "a".into(),
                ordinal_position: 1,
                data_type: pg_type_local2("int"),
                is_nullable: false,
                column_default: None,
                is_identity: false,
                is_generated: false,
                generated_expression: None,
                comment: None,
            },
        );
        let mut tables = IndexMap::new();
        tables.insert(
            "public".into(),
            PgSchema {
                name: "public".into(),
                tables: IndexMap::from([("t".into(), pg_table_local2("t", columns))]),
            },
        );
        let mut dbs = IndexMap::new();
        dbs.insert(
            "mydb".into(),
            PgDatabase {
                name: "mydb".into(),
                schemas: tables,
                views: IndexMap::new(),
                sequences: IndexMap::new(),
            },
        );
        EngineState::PostgreSql(PostgresState {
            engine_version: "18".into(),
            databases: dbs,
            warnings: vec![],
            capture_info: capture_info_local(),
        })
    }

    fn table_state_without_table() -> EngineState {
        let mut schemas = IndexMap::new();
        schemas.insert(
            "public".into(),
            PgSchema {
                name: "public".into(),
                tables: IndexMap::new(),
            },
        );
        let mut dbs = IndexMap::new();
        dbs.insert(
            "mydb".into(),
            PgDatabase {
                name: "mydb".into(),
                schemas,
                views: IndexMap::new(),
                sequences: IndexMap::new(),
            },
        );
        EngineState::PostgreSql(PostgresState {
            engine_version: "18".into(),
            databases: dbs,
            warnings: vec![],
            capture_info: capture_info_local(),
        })
    }

    fn pg_type_local2(name: &str) -> crate::state::PgType {
        crate::state::PgType {
            name: name.into(),
            oid: Some(23),
            internal_type: Some("int4".into()),
            array: false,
            length: None,
            precision: None,
            scale: None,
        }
    }

    fn pg_table_local2(name: &str, columns: IndexMap<String, PgColumn>) -> PgTable {
        PgTable {
            name: name.into(),
            schema: "public".into(),
            columns,
            primary_key: None,
            unique_constraints: vec![],
            foreign_keys: vec![],
            check_constraints: vec![],
            indexes: IndexMap::new(),
            comment: None,
            row_count: 0,
        }
    }

    fn capture_info_local() -> crate::state::CaptureInfo {
        crate::state::CaptureInfo {
            consistency: ConsistencyLevel::Snapshot,
            strategy: "test".into(),
            captured_at: Utc::now(),
            engine_version: "18".into(),
            scope: "mydb".into(),
            warnings: vec![],
        }
    }
}

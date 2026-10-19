//! Three-way merge analysis.

use crate::conflict::{Conflict, ConflictReport, ConflictResolution};
use crate::error::MeldError;
use crate::ids::CommitHash;
use crate::state::{EngineState, MongoState, PostgresState};
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

/// Apply a resolution to produce a proposed merged state.
///
/// Validates that every conflict is resolved, then builds the merged state by
/// starting from the left state and applying right-only additions plus
/// TakeRight conflict resolutions. Manual resolutions are accepted but the
/// merged state reflects the left state for those fields (a full manual
/// field-level merge requires user-supplied values applied by the caller).
pub fn apply_resolution(
    analysis: &MergeAnalysis,
    left_state: &EngineState,
    right_state: &EngineState,
) -> Result<ProposedMerge, crate::error::MeldError> {
    // 1. Validate that every conflict is resolved.
    for conflict in &analysis.conflicts {
        if !conflict.is_resolved() {
            return Err(MeldError::Conflict(format!(
                "cannot create merge commit: unresolved conflict {:?}",
                conflict.conflict
            )));
        }
    }

    // 2. Build the merged state starting from the left state.
    let mut merged_state = left_state.clone();

    match (&mut merged_state, left_state, right_state) {
        (
            EngineState::PostgreSql(merged),
            EngineState::PostgreSql(left),
            EngineState::PostgreSql(right),
        ) => {
            apply_postgres_resolutions(merged, left, right, &analysis.conflicts)?;
        }
        (EngineState::MongoDb(merged), EngineState::MongoDb(left), EngineState::MongoDb(right)) => {
            apply_mongodb_resolutions(merged, left, right, &analysis.conflicts)?;
        }
        _ => {
            return Err(MeldError::Internal(
                "merge states have incompatible engine kinds".into(),
            ));
        }
    }

    let summary = ChangeSummary::from_ops(&analysis.auto_mergeable);

    Ok(ProposedMerge {
        state: merged_state,
        ops: analysis.auto_mergeable.clone(),
        summary,
    })
}

/// Extract the object identity from a conflict, when available.
fn conflict_object_id(conflict: &Conflict) -> Option<crate::identity::ObjectIdentity> {
    match conflict {
        Conflict::SameObjectChangedIncompatible { object, .. } => Some(object.clone()),
        Conflict::DeleteVersusModify { object, .. } => Some(object.clone()),
        Conflict::IncompatibleSchemaChange { object, .. } => Some(object.clone()),
        Conflict::DuplicateKey { object, .. } => Some(object.clone()),
        Conflict::ConstraintOrDependencyViolation { object, .. } => Some(object.clone()),
        Conflict::IncompatibleIndexChange { object, .. } => Some(object.clone()),
        Conflict::IncompatibleDocumentField {
            collection,
            document_id,
            ..
        } => Some(crate::identity::ObjectIdentity::MongoDb(
            crate::identity::MongoObjectId::Document {
                collection: collection.clone(),
                id: document_id.clone(),
            },
        )),
        Conflict::IncompatibleArrayChange {
            collection,
            document_id,
            ..
        } => Some(crate::identity::ObjectIdentity::MongoDb(
            crate::identity::MongoObjectId::Document {
                collection: collection.clone(),
                id: document_id.clone(),
            },
        )),
        Conflict::MissingOrAmbiguousIdentity { .. } => None,
        Conflict::UnsupportedAutomaticMerge { object, .. } => Some(object.clone()),
    }
}

/// Returns the resolution strategies the engine can actually apply for a
/// conflict, so callers (e.g. the TUI) only offer choices that will be
/// honoured by `apply_resolution`.
///
/// - Object-level conflicts (SameObjectChangedIncompatible, DeleteVersusModify,
///   IncompatibleSchemaChange, DuplicateKey, IncompatibleIndexChange,
///   IncompatibleDocumentField, IncompatibleArrayChange) support TakeLeft and
///   TakeRight.
/// - Conflicts the engine cannot resolve (MissingOrAmbiguousIdentity,
///   ConstraintOrDependencyViolation, UnsupportedAutomaticMerge) support only
///   Deferred.
/// - `Manual(_)` is intentionally *not* listed here: `apply_resolution` accepts
///   it as "resolved" but does not apply a user-supplied value to nested
///   documents or rows, so offering it would silently drop the manual choice.
pub fn supported_resolutions(conflict: &Conflict) -> Vec<ConflictResolution> {
    match conflict {
        Conflict::SameObjectChangedIncompatible { .. }
        | Conflict::DeleteVersusModify { .. }
        | Conflict::IncompatibleSchemaChange { .. }
        | Conflict::DuplicateKey { .. }
        | Conflict::IncompatibleIndexChange { .. }
        | Conflict::IncompatibleDocumentField { .. }
        | Conflict::IncompatibleArrayChange { .. } => {
            vec![ConflictResolution::TakeLeft, ConflictResolution::TakeRight]
        }
        Conflict::MissingOrAmbiguousIdentity { .. }
        | Conflict::ConstraintOrDependencyViolation { .. }
        | Conflict::UnsupportedAutomaticMerge { .. } => {
            vec![ConflictResolution::Deferred]
        }
    }
}

/// Collect object identities whose conflicts were resolved as TakeRight.
fn right_take_objects(
    conflicts: &[ConflictReport],
) -> std::collections::HashSet<crate::identity::ObjectIdentity> {
    conflicts
        .iter()
        .filter_map(|c| match c.resolved {
            Some(ConflictResolution::TakeRight) => conflict_object_id(&c.conflict),
            _ => None,
        })
        .collect()
}

/// Apply merge resolutions to a PostgreSQL merged state.
fn apply_postgres_resolutions(
    merged: &mut PostgresState,
    _left: &PostgresState,
    right: &PostgresState,
    conflicts: &[ConflictReport],
) -> Result<(), MeldError> {
    let right_take = right_take_objects(conflicts);

    // For each database in right that isn't in merged, add it.
    for (db_name, right_db) in &right.databases {
        if !merged.databases.contains_key(db_name) {
            merged.databases.insert(db_name.clone(), right_db.clone());
        }
    }

    // For each schema/table in right, add tables not in merged, or replace
    // tables whose conflict was resolved as TakeRight.
    for (db_name, right_db) in &right.databases {
        let Some(merged_db) = merged.databases.get_mut(db_name) else {
            continue;
        };
        for (schema_name, right_schema) in &right_db.schemas {
            if !merged_db.schemas.contains_key(schema_name) {
                merged_db
                    .schemas
                    .insert(schema_name.clone(), right_schema.clone());
                continue;
            }
            let merged_schema = merged_db.schemas.get_mut(schema_name).unwrap();
            for (table_name, right_table) in &right_schema.tables {
                let object = crate::identity::ObjectIdentity::PostgreSql(
                    crate::identity::PostgresObjectId::Table {
                        schema: schema_name.clone(),
                        table: table_name.clone(),
                    },
                );
                // Use right's version if the conflict was resolved as TakeRight,
                // or if the table doesn't exist in the merged state (right-only addition).
                if right_take.contains(&object) || !merged_schema.tables.contains_key(table_name) {
                    merged_schema
                        .tables
                        .insert(table_name.clone(), right_table.clone());
                }
            }
        }
    }

    Ok(())
}

/// Apply merge resolutions to a MongoDB merged state.
fn apply_mongodb_resolutions(
    merged: &mut MongoState,
    _left: &MongoState,
    right: &MongoState,
    conflicts: &[ConflictReport],
) -> Result<(), MeldError> {
    let right_take = right_take_objects(conflicts);

    for (coll_name, right_coll) in &right.collections {
        let object =
            crate::identity::ObjectIdentity::MongoDb(crate::identity::MongoObjectId::Collection {
                name: coll_name.clone(),
            });
        // Use right's version if the conflict was resolved as TakeRight,
        // or if the collection doesn't exist in the merged state (right-only addition).
        if right_take.contains(&object) || !merged.collections.contains_key(coll_name) {
            merged
                .collections
                .insert(coll_name.clone(), right_coll.clone());
        }
    }

    Ok(())
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
            rows: vec![],
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
            rows: vec![],
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

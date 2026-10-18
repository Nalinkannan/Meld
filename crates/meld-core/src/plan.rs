//! Dependency-aware restore plans and operation risk classification.

use crate::identity::ObjectIdentity;
use crate::ids::{CommitHash, ObjectHash};
use crate::state::EngineKind;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Risk classification of a planned operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OperationRisk {
    /// Reversible with no data loss (e.g. creating an index).
    Reversible,
    /// Destructive but the prior state is captured and can be restored.
    Destructive,
    /// Potentially irreversible (e.g. dropping a table without a backup).
    PotentiallyIrreversible,
}

impl std::fmt::Display for OperationRisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationRisk::Reversible => write!(f, "reversible"),
            OperationRisk::Destructive => write!(f, "destructive"),
            OperationRisk::PotentiallyIrreversible => {
                write!(f, "potentially-irreversible")
            }
        }
    }
}

/// A single planned operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlanOp {
    pub id: String,
    pub kind: PlanOpKind,
    pub object: ObjectIdentity,
    pub risk: OperationRisk,
    pub depends_on: Vec<String>,
    pub reason: String,
    pub requires_confirmation: bool,
}

/// The kind of planned operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlanOpKind {
    CreateObject,
    DropObject,
    AlterObject,
    InsertData,
    UpdateData,
    DeleteData,
    CreateIndex,
    DropIndex,
    AlterIndex,
    CreateConstraint,
    DropConstraint,
    CreateCollection,
    DropCollection,
    AlterCollection,
    ChangeValidator,
    InsertDocument,
    UpdateDocument,
    DeleteDocument,
    Other(String),
}

/// A dependency-aware restore plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RestorePlan {
    pub target_commit: CommitHash,
    pub root_hash: ObjectHash,
    pub engine_kind: EngineKind,
    pub ops: Vec<PlanOp>,
    pub destructive_count: u64,
    pub irreversible_count: u64,
    pub requires_confirmation: bool,
    pub validation_warnings: Vec<String>,
}

impl RestorePlan {
    pub fn new(target_commit: CommitHash, root_hash: ObjectHash, engine_kind: EngineKind) -> Self {
        Self {
            target_commit,
            root_hash,
            engine_kind,
            ops: vec![],
            destructive_count: 0,
            irreversible_count: 0,
            requires_confirmation: false,
            validation_warnings: vec![],
        }
    }

    pub fn with_ops(mut self, ops: Vec<PlanOp>) -> Self {
        let mut destructive_count = 0;
        let mut irreversible_count = 0;
        let mut requires_confirmation = false;
        for op in &ops {
            match op.risk {
                OperationRisk::Destructive => destructive_count += 1,
                OperationRisk::PotentiallyIrreversible => irreversible_count += 1,
                OperationRisk::Reversible => {}
            }
            if op.requires_confirmation {
                requires_confirmation = true;
            }
        }
        self.ops = ops;
        self.destructive_count = destructive_count;
        self.irreversible_count = irreversible_count;
        self.requires_confirmation = requires_confirmation;
        self
    }

    /// Operations in dependency-resolved execution order (parents before
    /// dependents). Returns an error if a dependency cycle is detected.
    pub fn ordered_ops(&self) -> Result<Vec<PlanOp>, MeldPlanError> {
        let ids: Vec<String> = self.ops.iter().map(|o| o.id.clone()).collect();
        let mut visited = std::collections::HashSet::new();
        let mut order: Vec<PlanOp> = Vec::new();

        for start in &ids {
            if visited.contains(start.as_str()) {
                continue;
            }
            let mut stack: Vec<String> = vec![start.clone()];
            let mut on_stack: std::collections::HashSet<String> = std::collections::HashSet::new();
            on_stack.insert(start.clone());

            while let Some(id) = stack.last().cloned() {
                if visited.contains(&id) {
                    stack.pop();
                    on_stack.remove(&id);
                    continue;
                }
                let op = match self.ops.iter().find(|o| o.id == id) {
                    Some(o) => o.clone(),
                    None => {
                        return Err(MeldPlanError::MissingDependency(format!("unknown op {id}")));
                    }
                };
                let mut next_dep: Option<String> = None;
                for dep in &op.depends_on {
                    if visited.contains(dep.as_str()) {
                        continue;
                    }
                    if on_stack.contains(dep.as_str()) {
                        return Err(MeldPlanError::Cycle(format!("dependency cycle at {dep}")));
                    }
                    next_dep = Some(dep.clone());
                    break;
                }
                if let Some(dep) = next_dep {
                    stack.push(dep.clone());
                    on_stack.insert(dep);
                } else {
                    visited.insert(id.clone());
                    on_stack.remove(&id);
                    stack.pop();
                    order.push(op);
                }
            }
        }
        Ok(order)
    }

    pub fn destructive_ops(&self) -> Vec<&PlanOp> {
        self.ops
            .iter()
            .filter(|o| o.risk.is_destructive())
            .collect()
    }
}

impl OperationRisk {
    pub fn is_destructive(self) -> bool {
        matches!(
            self,
            OperationRisk::Destructive | OperationRisk::PotentiallyIrreversible
        )
    }
}

#[derive(Debug, Error, Clone)]
pub enum MeldPlanError {
    #[error("dependency cycle: {0}")]
    Cycle(String),
    #[error("missing dependency: {0}")]
    MissingDependency(String),
    #[error("plan validation failed: {0}")]
    Validation(String),
}

/// Validate a plan before execution.
pub fn validate_plan(plan: &RestorePlan) -> Result<(), MeldPlanError> {
    let ids: std::collections::HashSet<&str> = plan.ops.iter().map(|o| o.id.as_str()).collect();
    for op in &plan.ops {
        for dep in &op.depends_on {
            if !ids.contains(dep.as_str()) {
                return Err(MeldPlanError::MissingDependency(format!(
                    "op {id} depends on unknown {dep}",
                    id = op.id
                )));
            }
        }
    }
    // Cycle detection via ordered_ops.
    plan.ordered_ops()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{MongoObjectId, PostgresObjectId};

    fn op(id: &str, risk: OperationRisk, depends: Vec<&str>) -> PlanOp {
        PlanOp {
            id: id.to_string(),
            kind: PlanOpKind::DropObject,
            object: ObjectIdentity::PostgreSql(PostgresObjectId::Table {
                schema: "public".into(),
                table: id.into(),
            }),
            risk,
            depends_on: depends.iter().map(|s| s.to_string()).collect(),
            reason: "test".into(),
            requires_confirmation: matches!(risk, OperationRisk::Destructive),
        }
    }

    #[test]
    fn ordered_ops_respects_dependencies() {
        let plan = RestorePlan::new(
            CommitHash::ZERO,
            ObjectHash::from_bytes([1; 32]),
            EngineKind::PostgreSql,
        )
        .with_ops(vec![
            op("b", OperationRisk::Reversible, vec!["a"]),
            op("a", OperationRisk::Reversible, vec![]),
            op("c", OperationRisk::Destructive, vec!["b"]),
        ]);
        let order = plan.ordered_ops().unwrap();
        let order_ids: Vec<&str> = order.iter().map(|o| o.id.as_str()).collect();
        let a_pos = order_ids.iter().position(|&i| i == "a").unwrap();
        let b_pos = order_ids.iter().position(|&i| i == "b").unwrap();
        let c_pos = order_ids.iter().position(|&i| i == "c").unwrap();
        assert!(a_pos < b_pos && b_pos < c_pos);
        assert_eq!(plan.destructive_count, 1);
        assert!(plan.requires_confirmation);
    }

    #[test]
    fn cycle_detected() {
        let plan = RestorePlan::new(
            CommitHash::ZERO,
            ObjectHash::from_bytes([1; 32]),
            EngineKind::MongoDb,
        )
        .with_ops(vec![
            PlanOp {
                id: "a".into(),
                kind: PlanOpKind::DropObject,
                object: ObjectIdentity::MongoDb(MongoObjectId::Collection { name: "a".into() }),
                risk: OperationRisk::Reversible,
                depends_on: vec!["b".into()],
                reason: "t".into(),
                requires_confirmation: false,
            },
            PlanOp {
                id: "b".into(),
                kind: PlanOpKind::DropObject,
                object: ObjectIdentity::MongoDb(MongoObjectId::Collection { name: "b".into() }),
                risk: OperationRisk::Reversible,
                depends_on: vec!["a".into()],
                reason: "t".into(),
                requires_confirmation: false,
            },
        ]);
        let err = plan.ordered_ops().unwrap_err();
        assert!(matches!(err, MeldPlanError::Cycle { .. }));
    }

    #[test]
    fn missing_dependency_rejected() {
        let plan = RestorePlan::new(
            CommitHash::ZERO,
            ObjectHash::from_bytes([1; 32]),
            EngineKind::PostgreSql,
        )
        .with_ops(vec![op("a", OperationRisk::Reversible, vec!["missing"])]);
        let err = validate_plan(&plan).unwrap_err();
        assert!(matches!(err, MeldPlanError::MissingDependency { .. }));
    }
}

# Meld — Restore Safety

Restore is destructive. Meld implements it as a staged workflow:

1. Resolve the target commit; validate engine and scope.
2. Capture a fresh pre-restore snapshot when possible.
3. Compare the live database against the intended target state.
4. Build a dependency-aware restore plan.
5. Validate privileges, unsupported objects, constraint/index conflicts, and
   preconditions.
6. Display the complete plan and risk warnings in the TUI.
7. Require explicit confirmation for destructive changes.
8. Execute operations in safe order using supported transactions or
   compensating recovery actions.
9. Independently re-inspect the resulting database.
10. Compare actual state against the target state via canonicalization.
11. Report the final result, discrepancies, and recovery status.

## Operation risk classification

- **Reversible**: no data loss (e.g. creating an index).
- **Destructive**: prior state captured and restorable.
- **Potentially-irreversible**: may not be fully rollbackable.

Destructive actions require explicit confirmation; potentially-irreversible
actions are highlighted and require particular care.

## Honesty about atomicity

Meld does not claim every multi-operation restore is atomic. Where complete
rollback cannot be guaranteed, the limitation is identified before execution.
If an operation fails, the original error is preserved, completed operations
are reported, the appropriate recovery strategy is attempted, and the final
state is verified. A failed rollback is never concealed behind the original
error. If verification fails, the restore is not marked successful.

## PostgreSQL

Dependencies are respected via catalog metadata; transactions are used where
supported.

## MongoDB

Supported transactions are used where available; otherwise carefully ordered,
recoverable operations are used. Multi-document consistency depends on the
deployment (replica set / sharded cluster with WiredTiger).

# Meld — Testing

## Strategy

Tests are built throughout implementation, not only at the end.

### Pure domain tests (`meld-core`)

Canonical serialization determinism, BSON type preservation, state hash
stability, object identity preservation, commit immutability, commit DAG
traversal and common ancestors, semantic diff correctness, rename detection,
three-way merge, conflict classification, dependency ordering and cycle
detection, restore-plan validation, verification mismatches, incompatible
engine rejection.

### Storage tests (`meld-store`)

Store initialization, commit persistence, object retrieval with hash
verification, branch/HEAD updates, reopening after shutdown, atomic reference
updates, corruption detection, missing-object handling, incompatible store
version handling.

### Integration tests (PostgreSQL / MongoDB)

Run against isolated disposable databases (containers). Cover snapshot → modify
→ snapshot → semantic diff → restore → independent verification, plus
constraints/indexes/nulls/types (PostgreSQL) and BSON types/validators/duplicates
(MongoDB), and partial-failure recovery.

## Running tests

```bash
# Unit + storage tests (no database services required)
cargo test --workspace

# PostgreSQL / MongoDB integration tests (require services)
cargo test --test postgres_snapshot_restore --test mongodb_snapshot_restore \
  --test diff_scenarios --test merge_scenarios --test failure_recovery
```

## When services are unavailable

Integration tests that require PostgreSQL or MongoDB are skipped and clearly
reported; the suite does not fail the build. Full integration validation is
only claimed when the services are present and the tests pass.

## Definition of done (testing)

- `cargo test --workspace` passes.
- Database integration tests pass against isolated disposable instances.
- No test depends on the user's real databases or risks deleting user data.

# Meld — Canonical Data Model

## Design principle

PostgreSQL and MongoDB share one version-control engine but are never forced
into an inaccurate common schema. `EngineState` is an enum carrying a typed,
serializable, recoverable state per engine:

```rust
pub enum EngineState {
    PostgreSql(PostgresState),
    MongoDb(MongoState),
}
```

Cross-engine merge/restore is rejected by engine identity and snapshot scope.

## Canonical value

`CanonicalValue` preserves type distinctions so that, e.g., a string `_id` and
an ObjectId `_id` never collide. Arrays preserve order (they are not treated as
sets). Document maps are key-sorted at canonicalization time for determinism.

## Deterministic canonicalization contract

The same logical state must produce the same hash when extracted independently,
given the same scope, consistency rules, normalization rules, and
canonicalization version. Rules:

- Stable ordering for object/field/map keys whose order is semantically
  irrelevant.
- Semantically significant order preserved: PostgreSQL column ordinal order,
  index key order, array order.
- BSON type information preserved.
- Timestamps normalized to RFC3339 UTC; numerics normalized per documented
  rules.
- Credentials and secrets never hashed into committed state.
- Separate state hashes, object hashes, and commit hashes, with explicit
  format/version tags and domain separation (`meld-state-v1`,
  `meld-object-v1`, `meld-commit-v1`).

## PostgreSQL representation

Schemas, tables, columns (ordinal position, normalized type, nullability,
defaults, identity/generated), primary/unique/foreign/check constraints, index
definitions (with key order), views, sequences, comments, and table rows with
stable primary-key identities. Objects are fully schema-qualified; nothing is
assumed to be in `public`.

## MongoDB representation

Database and collection names, collection options, document validators
(validation level/action), index definitions (name, keys, unique, partial
filter, TTL, sparse, background), views, and documents (nested objects, arrays,
BSON-specific types, `_id`). MongoDB has no enforced foreign keys; inferred
document references are represented separately from enforced constraints.

## Warnings

Capture limitations and unsupported features are reported explicitly as
capture warnings; a snapshot that excludes supported objects is not reported as
fully successful.

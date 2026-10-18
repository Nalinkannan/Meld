# Meld — Architecture

## Overview

Meld is a semantic version-control and state-reconciliation system for
databases. It is built as a Cargo workspace with five crates and a strict,
one-directional dependency flow:

```
meld-tui  →  meld-engine  →  meld-adapter  →  meld-store  →  meld-core
   UI        orchestration    adapters      local store      pure domain
```

`meld-core` is pure: no database, terminal, filesystem, or network I/O. Every
other crate depends only on crates to its right in the arrow above.

## Crate responsibilities

### meld-core
Pure domain logic. Defines:
- Engine-specific state model (`EngineKind` / `EngineState` with typed
  `PostgresState` and `MongoState`).
- Canonical deterministic serialization and BLAKE3 content addressing
  (separate state/object/commit hashes, domain separation, format versions).
- Stable object identities (including BSON-type-preserving document IDs).
- Semantic change operations (`ChangeOp` / `ChangeKind`).
- Commit DAG types, three-way merge and conflict types.
- Dependency-aware restore plans (`RestorePlan`, `PlanOp`, `OperationRisk`).
- Independent verification primitives (`VerificationReport`, `Mismatch`).
- Typed errors (`thiserror`).

### meld-store
Durable local version-history storage backed by `redb` and BLAKE3:
- Content-addressed immutable object storage with integrity check on every
  read.
- Commit persistence and DAG references (branch / HEAD).
- Atomic compare-and-swap reference updates (prevents lost updates).
- Saved connection profiles **without plaintext passwords**.
- Integrity checks, orphan/dangling-reference detection, schema versioning.

### meld-adapter
Asynchronous `DatabaseAdapter` trait plus PostgreSQL and MongoDB
implementations. Driver types never leak into `meld-core`.
- SQLx (PostgreSQL, native-tls) with identifier quoting and bound parameters.
- Official MongoDB Rust driver with BSON type preservation.
- Capability model: an adapter only advertises operations it implements and
  tests; unsupported operations return actionable errors.

### meld-engine
Application orchestration and transaction-like workflows:
- Sessions, snapshot creation, history/diff, branch management.
- Checkout and restore (staged, with pre-restore snapshot and independent
  verification).
- Merge and conflict resolution.
- Plan validation and failure recovery.

### meld-tui
Polished full-screen Ratatui terminal UI (the primary interface). Launching
Meld opens the TUI directly by default.

## Key architectural rules

- **No cross-engine merge/restore.** Engine identity is checked before any
  destructive operation; a PostgreSQL state can never be merged with or
  restored onto MongoDB, and vice versa.
- **No plaintext passwords.** Connection profiles are persisted without
  passwords; a reconnect workflow is used.
- **No fake success.** Operations verify postconditions independently before
  reporting success.
- **No in-memory store as the normal backend.** History survives restart via
  `redb`.
- **No SQLite/MariaDB, no web UI, no HTTP API, no daemon, no fake engine.**

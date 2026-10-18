# Meld — Semantic Version-Control for Database States

Meld is a semantic version-control and state-reconciliation system for databases,
written in Rust. It provides Git-like functionality for actual database states
rather than merely tracking migration scripts.

## Target databases

- **PostgreSQL** (via SQLx, native-tls)
- **MongoDB** (via the official MongoDB Rust driver, BSON)

SQLite and MariaDB are intentionally not supported.

## Core guarantees

- Immutable, content-addressed snapshots (BLAKE3).
- Persistent commit DAG with parent relationships, branches, and HEAD.
- Semantic (typed) diffs between snapshots — schema, constraints, indexes, rows,
  documents, and BSON-type-preserving field-level changes.
- Dependency-aware restore plans with pre-execution previews and destructive
  operation confirmations.
- Independent verification after restore and merge (fresh read-only inspection,
  never trusting the executor's claim).
- Real three-way merge with explicit conflict classification and interactive
  resolution.
- A polished, full-screen Ratatui TUI as the primary interface.

**Central invariant:** a successful Meld operation corresponds to a real,
validated state transition. Meld never reports success merely because a command
was submitted or a driver returned without an immediate error.

## Repository layout (Cargo workspace)

```
meld/
  crates/meld-core      Pure domain model (no I/O): state, canonical hashing,
                        diffs, commits, merge, conflicts, plans, verification.
  crates/meld-store     Durable local version store (redb + BLAKE3).
  crates/meld-adapter   Database abstraction + PostgreSQL and MongoDB adapters.
  crates/meld-engine    Application orchestration / transaction-like workflows.
  crates/meld-tui       Ratatui full-screen terminal UI (primary interface).
```

## Build and run

```bash
# Build everything
cargo build --workspace

# Run unit + storage + domain tests (no database services required)
cargo test --workspace

# Format, lint, and clippy (warnings are denied)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Run the application (launches the full-screen TUI by default)
cargo run --bin meld
```

## Configuration

See `.env.example` for example connection values. Meld stores connection
profiles locally **without plaintext passwords**. Passwords are held only in
memory for the duration of a session; use the reconnect workflow to supply
credentials again after restart.

Default repository location: the user's application data directory
(overridable via the `MELD_REPO` environment variable).

## Supported scope (this release)

| Capability                  | PostgreSQL | MongoDB |
|-----------------------------|:----------:|:-------:|
| Connection & capabilities   | yes        | yes     |
| Schema/catalog introspection| yes        | yes     |
| Data extraction             | yes        | yes     |
| Snapshot capture            | yes        | yes*    |
| Semantic diff               | partial    | partial |
| Restore planning/execution  | partial    | no\*\*  |
| Independent verification    | partial    | partial |
| Three-way merge             | analysis   | analysis|

\* MongoDB single-node captures are best-effort; multi-document consistency
requires transactions on a replica set. This is explicitly captured in the
snapshot's consistency metadata.
\*\* MongoDB restore execution is not yet implemented in this release.

## Safety

Restore and merge are destructive. Meld previews the full dependency-aware plan,
classifies each operation as reversible/destructive/potentially-irreversible,
and requires explicit confirmation before executing anything destructive.
Independent verification runs after restore and merge; a failed verification
means the operation is not reported as successful.

## Documentation

- `ARCHITECTURE.md` — design decisions and component boundaries.
- `SECURITY.md` — trust boundaries and credential handling.
- `docs/DATA_MODEL.md` — canonical state model.
- `docs/RESTORE_SAFETY.md` — restore safety contract.
- `docs/TESTING.md` — testing strategy and results.
- `docs/UI_DESIGN.md` — TUI design system.
- `PLAN.md` — implementation plan and acceptance checklist.

## License

See the repository for license information.

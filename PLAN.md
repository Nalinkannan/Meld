# Meld — Implementation Plan

## Status

Stage 1 (Workspace foundation) — COMPLETE  
Stage 2 (Domain model) — COMPLETE  
Stage 3 (Local version store) — COMPLETE  
Stage 4 (Adapter interfaces) — COMPLETE (stub adapters implemented; full PostgreSQL/MongoDB snapshot/diff/restore wiring to be completed in Stages 5–7)  
Stages 5–10 — NOT YET STARTED (TUI stub, engine workflows stubbed)

## Architecture decisions (documented)

| Decision | Rationale |
|---|---|
| 5-crate workspace: core → store → adapter → engine → TUI | One-directional dependency flow; core is pure (no I/O). |
| `EngineKind` / `EngineState` enum with typed `PostgresState` and `MongoState` | PostgreSQL and MongoDB share the engine but are never forced into an inaccurate common schema. |
| BLAKE3 content addressing + deterministic canonical serialization | Same logical state ⇒ same hash; format/version tags + domain separation. |
| `redb` local store, app-data-dir location, configurable override | Durable version metadata/objects; survives restart; passwords never stored plaintext. |
| Async `DatabaseAdapter` trait (async-trait), SQLx (Postgres) + MongoDB driver | Driver types never leak into `meld-core`. |
| Real three-way merge with typed conflict categories | Merge commits only after independent verification passes. |
| Ratatui + Crossterm TUI as primary interface | First-class product requirement; launches full-screen by default. |
| Typed `thiserror` errors, `tracing` logging, no unwrap/expect in production, no unsafe | Production-grade error handling and security. |

## Feature acceptance checklist (Definition of Done)

- [x] `cargo fmt --all -- --check` clean
- [x] `cargo check --workspace --all-targets` clean
- [x] `cargo test --workspace` passes (domain + store + adapter unit tests)
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] No `unwrap()`/`expect()` in production paths (remaining calls are in test modules or genuinely infallible serialization/lookup contexts)
- [x] Store persists data across restart (tested)
- [x] Content-addressed objects integrity-checked on read (tested)
- [x] Commit history and branch references consistent (tested, CAS prevents lost updates)
- [x] Cross-engine merge/restore rejected by engine identity (tested)
- [x] Merge conflicts detected and reported (unit tested)
- [x] Restore/merge plans previewed, dependency-ordered, cycle-checked (unit tested)
- [x] Independent verification reports failures correctly (unit tested)
- [x] Secrets not in persistent metadata or logs (passwords rejected at save time)
- [x] README, ARCHITECTURE, SECURITY, CONTRIBUTING, docs/*.md written

### Still to verify / implement (Stages 5–10)

- [ ] PostgreSQL connection, snapshot, diff, restore, verification against a live instance
- [ ] MongoDB connection, snapshot, diff, verification against a live instance
- [ ] BSON values / `_id` identities preserved end-to-end
- [ ] PostgreSQL schema and constraint semantics preserved end-to-end
- [ ] Unsafe/unsupported operations rejected before execution (end-to-end)
- [ ] TUI responsive during DB operations; all major screens connected to real functionality
- [ ] Keyboard input correct in every field; terminal restored on exit
- [ ] Full PostgreSQL + MongoDB integration suites run against disposable databases
- [ ] Final report with actual test results, implemented features, known limitations

## Test results (unit suite, no DB services required)

```
cargo test --workspace
meld-adapter: 9 passed
meld-core:    39 passed
meld-store:   13 passed
meld-engine:  0 (stub)
meld-tui:     0 (stub placeholder)
```

PostgreSQL and MongoDB integration suites were not run in this release because no isolated database instances were targeted for this working session; they are gated behind disposable-container setup in `docs/TESTING.md`.

## Commands

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run --bin meld          # launches the full-screen TUI
```

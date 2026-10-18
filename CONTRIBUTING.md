# Meld — Contributing

## Development workflow

1. Create a worktree or feature branch.
2. Implement one stage at a time (see `PLAN.md`), running the appropriate
   checks after each stage:
   ```bash
   cargo fmt --all -- --check
   cargo check --workspace --all-targets
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   ```
3. Warnings are denied by Clang CI; fix the underlying issue or document a
   narrowly justified exception.
4. No `unwrap()`/`expect()` in production paths; no `unsafe` unless strictly
   necessary and justified.
5. Update `ARCHITECTURE.md`, `SECURITY.md`, and the relevant `docs/` files when
   behavior changes.
6. Add tests alongside features, not only at the end.

## Code style

- Rust 2024 edition, stable toolchain.
- `cargo fmt` with `max_width = 100`.
- Clippy with `-D warnings`.
- Typed errors via `thiserror`; diagnostics via `tracing`.

## Testing

- Pure domain tests must not depend on real databases.
- PostgreSQL/MongoDB integration tests use isolated disposable databases and
  are gracefully skipped (and reported) when services are unavailable.
- Never run tests against a user's real databases.

## Reporting issues

Use the project's issue tracker; include `cargo test` output, the database
engine versions, and the Meld version.

# Meld — Security

## Trust boundaries

Meld operates on database states it reads from and writes to target databases,
and on a local version store holding version metadata and content-addressed
objects. The trust model:

- The local version store is trusted to hold immutable, content-addressed
  objects and commits. Its integrity is verified via BLAKE3 content addressing
  on every read.
- Target databases (PostgreSQL / MongoDB) are treated as sources of truth for
  introspection and as destinations for restore. Meld never trusts a restore's
  claimed result; it independently re-inspects and verifies.
- The terminal UI and the operator are the authority for confirming destructive
  operations.

## Credential handling

- Database passwords are **never written to the local version store**.
- Connection profiles store only non-secret parameters (host, port, user,
  database name, URI without credentials).
- Passwords are held only in memory for the duration of a session.
- On restart, the operator must supply credentials again via the reconnect
  workflow.
- Passwords and credential-bearing URIs are never logged or shown in status
  bars. Driver error messages are surfaced without exposing secrets.

## Safety guarantees

- Destructive operations (restore, merge) are never executed without plan
  preview and explicit confirmation.
- Operations are classified as reversible / destructive /
  potentially-irreversible; potentially-irreversible operations require
  additional care.
- Restore and merge are verified independently after execution using fresh
  read-only inspection. A failed verification means the operation is not
  reported as successful.
- Cross-engine operations (PostgreSQL ↔ MongoDB) are rejected by engine
  identity before any destructive planning.

## Limitations

- Meld does not provide a credential store integration in this release;
  passwords are not persisted. Use the reconnect workflow after restart.
- Multi-document transactional consistency is not guaranteed on single-node
  MongoDB deployments; such snapshots are explicitly marked best-effort.
- Restore execution for MongoDB is not implemented in this release.

//! Pure domain layer for Meld.
//!
//! This crate contains no database, terminal, filesystem, or network I/O. It
//! defines the canonical state model, content addressing, semantic diffs, the
//! commit DAG, three-way merge and conflict types, and verification primitives.

pub mod error;

pub use error::MeldError;

pub use commit::Commit;
pub use identity::ObjectIdentity;
pub use ids::{CommitHash, ObjectHash, RootHash};
pub use state::CanonicalValue;
pub use state::CaptureWarning;
pub use state::EngineState;

pub mod canonical;
pub mod commit;
pub mod conflict;
pub mod diff;
pub mod graph;
pub mod identity;
pub mod ids;
pub mod merge;
pub mod plan;
pub mod state;
pub mod transition;
pub mod verify;

#[cfg(test)]
mod core_tests;

//! Application orchestration and transaction-like workflows for Meld.
//!
//! The engine coordinates the domain layer (`meld-core`), the local
//! version store (`meld-store`), and the database adapters (`meld-adapter`).
//! It owns the unit-of-work style workflows: snapshot, checkout/restore,
//! merge, branch management, and independent verification.

pub mod checkout;
pub mod diff_cmd;
pub mod drift;
pub mod error;
pub mod history;
pub mod merge_cmd;
pub mod restore;
pub mod session;
pub mod snapshot;
pub mod verify_cmd;

pub use error::EngineError;
pub use session::Session;

//! Local persistence adapters for Evidra's append-only observation ledger.
//!
//! The crate exposes [`SqliteObservationStore`], a repository-local SQLite implementation of
//! [`evidra_core::ObservationStore`]. It validates schema and observation integrity when opening
//! a store and records agent-harness observations together with their idempotency receipts.

mod sqlite;

pub use sqlite::{SqliteObservationStore, StoreError};

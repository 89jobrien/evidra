//! External boundaries for validated event sources and append-only observation persistence.
//!
//! Adapters implement these traits so domain callers can read harness events and persist ordinary
//! or identity-tracked observations without depending on storage or transport details.

use std::error::Error;

use crate::{AgentHarnessEvent, AgentHarnessObservation, Observation};

/// Result of attempting to append an idempotent harness observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentHarnessAppendOutcome {
    /// A new observation and receipt were committed.
    Recorded,
    /// The identity and semantic digest were already recorded.
    Duplicate,
    /// The identity was already associated with different semantic content.
    IdentityConflict,
}

/// Pull-based inbound boundary for validated agent-harness events.
pub trait AgentHarnessEventSource {
    /// Adapter-specific error returned while reading source events.
    type Error: Error + Send + Sync + 'static;

    /// Returns the next validated event, or `None` when the source is exhausted.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when source bytes cannot be read or validated.
    fn next_event(&mut self) -> Result<Option<AgentHarnessEvent>, Self::Error>;
}

/// Append-only persistence boundary for observations.
pub trait ObservationStore {
    /// Adapter-specific error returned by persistence operations.
    type Error: Error + Send + Sync + 'static;

    /// Appends an observation without replacing any existing record.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when the observation cannot be appended.
    fn append(&mut self, observation: &Observation) -> Result<(), Self::Error>;

    /// Lists at most `limit` observations, newest first.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when persisted observations cannot be read or validated.
    fn list(&self, limit: usize) -> Result<Vec<Observation>, Self::Error>;

    /// Appends a harness observation and its idempotency receipt atomically.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when receipt validation or persistence fails.
    fn append_harness_observation(
        &mut self,
        value: &AgentHarnessObservation,
    ) -> Result<AgentHarnessAppendOutcome, Self::Error>;
}

//! External boundaries for validated event sources and append-only persistence.
//!
//! Adapters implement these traits so domain callers can read harness events, persist ordinary
//! or identity-tracked observations, and store derived records and relationships without depending
//! on storage or transport details.

use std::error::Error;

use crate::derivation::{
    CurrentFacet, Derivation, DerivationScope, EvidenceTarget, FacetCount, RelationKind,
    Relationship,
};
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
    //
    // TODO(CRITICAL): add `get(&ObservationId)` and `select(&DerivationScope)` to this trait.
    //
    // `list(limit)` is currently the only read path — a display query, not a selection primitive. No
    // command can read one record, and nothing can execute a `DerivationScope` against observations,
    // so `derive` cannot select its evidence set and a user-supplied window would silently derive from
    // whatever `list` returned. Specified in
    // `docs/designs/2026-10-03-derived-cli-surface-design.md`; see A-06 in `docs/AUDIT.md`.
    //
    // `select` must delegate to the same scope predicate `DerivationStore::current_facets` uses, not
    // to a second SQL statement that happens to agree. If they diverge, a derivation's evidence set and
    // the rows its scope considers current disagree, and the derivation cites evidence the current view
    // no longer recognises — making it unfalsifiable. Both queries would be individually well-formed, so
    // `validate_v3` cannot see it.

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

/// Persistence for derived records and their projected facets.
///
/// Implementations append only. A revision is a new derivation plus a
/// [`RelationKind::Supersedes`] relationship, never a mutation of a stored row (ADR-008).
//
// TODO(CRITICAL): implement this trait in `evidra-store`; it has no implementor and no consumer.
//
// Schema v3 already carries `derivations`, `derivation_facets`, and `derivation_evidence`, but only
// raw-SQL helpers inside `evidra-store`'s test module ever populate them. The derived layer — the
// entire reason schema v3 exists — is persisted yet unreachable through its own declared interface.
//
// Specified in `docs/designs/2026-10-03-derived-cli-surface-design.md`; see A-04 in
// `docs/AUDIT.md`. Either add the implementation or record explicitly that this is ports-first.
pub trait DerivationStore {
    /// Adapter-specific error returned by persistence operations.
    type Error: Error + Send + Sync + 'static;

    /// Appends a derivation and its indexed facet projection atomically.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when the record cannot be appended or a facet namespace is not
    /// registered.
    fn append(&mut self, derivation: &Derivation) -> Result<(), Self::Error>;

    /// Returns facet values for a scope, resolving the supersedes chain so each superseded
    /// derivation is represented exactly once by its current successor.
    ///
    /// This is a read-through view. Superseded rows remain stored and remain individually
    /// retrievable; this method decides which ones count as current.
    ///
    /// TODO(HIGH): correct this doc — superseded rows are not "individually retrievable" by any of
    /// this trait's methods.
    ///
    /// There is no `get(&DerivationId)`, so a caller holding an id cannot retrieve a single derivation
    /// at all. `current_facets` and `aggregate` are the only read paths and both are collection-shaped.
    /// Either add the accessor or drop the claim; see A-07 in `docs/AUDIT.md`.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when the projection cannot be read or the chain cannot be resolved.
    fn current_facets(&self, scope: &DerivationScope) -> Result<Vec<CurrentFacet>, Self::Error>;

    /// Aggregates indexed facet values within a scope.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when the projection cannot be queried.
    fn aggregate(
        &self,
        namespace: &str,
        name: &str,
        scope: &DerivationScope,
    ) -> Result<Vec<FacetCount>, Self::Error>;
}

/// Persistence for the typed edges connecting records.
//
// TODO(CRITICAL): implement this trait in `evidra-store`; it has no implementor and no consumer.
//
// The `relationships` table exists in schema v3 and is populated only by a raw-SQL helper in
// `evidra-store`'s test module. The `derive`, `relate`, and `explain` CLI work is blocked on this port
// as well as on `DerivationStore`. See A-05 in `docs/AUDIT.md`.
pub trait RelationshipStore {
    /// Adapter-specific error returned by persistence operations.
    type Error: Error + Send + Sync + 'static;

    /// Appends a relationship without replacing any existing record.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when the relationship cannot be appended.
    fn append(&mut self, relationship: &Relationship) -> Result<(), Self::Error>;

    /// Returns relationships touching `target` in either direction.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when relationships cannot be read.
    fn neighbors(
        &self,
        target: EvidenceTarget,
        relation: Option<RelationKind>,
    ) -> Result<Vec<Relationship>, Self::Error>;
}

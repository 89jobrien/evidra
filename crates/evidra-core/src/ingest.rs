//! Deterministic policy and ports for ingesting agent-harness inbox claims.
//!
//! This module coordinates claim lifecycle, observation persistence, duplicate handling, and
//! quarantine accounting without exposing source evidence through operational errors.

use std::collections::BTreeMap;
use std::fmt;

use miette::Diagnostic;
use serde::Serialize;
use thiserror::Error;

use crate::{
    AgentHarnessAppendOutcome, AgentHarnessEvent, AgentHarnessObservation, ObservationStore,
};

/// Fixed reason for retaining an inbox event outside the observation ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuarantineReason {
    /// Blank input exceeded its aggregate bound.
    BlankInputLimit,
    /// The claimed file contained no event.
    EmptyEvent,
    /// The source identity was already associated with different content.
    IdentityConflict,
    /// A legacy quarantine item was missing its reason sidecar.
    IncompleteQuarantine,
    /// The normalized harness event was invalid.
    InvalidEvent,
    /// The record was not valid JSON.
    InvalidJson,
    /// A source or subject reference was invalid.
    InvalidReference,
    /// The claimed file contained more than one event.
    MultipleEvents,
    /// The event record exceeded its byte bound.
    RecordTooLarge,
    /// The record used an unsupported schema discriminator.
    UnsupportedSchema,
}

impl fmt::Display for QuarantineReason {
    /// Writes the stable kebab-case code used in summaries and quarantine sidecars.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidJson => "invalid-json",
            Self::UnsupportedSchema => "unsupported-schema",
            Self::InvalidReference => "invalid-reference",
            Self::InvalidEvent => "invalid-event",
            Self::RecordTooLarge => "record-too-large",
            Self::BlankInputLimit => "blank-input-limit",
            Self::EmptyEvent => "empty-event",
            Self::MultipleEvents => "multiple-events",
            Self::IdentityConflict => "identity-conflict",
            Self::IncompleteQuarantine => "incomplete-quarantine",
        })
    }
}

/// Content obtained from one claimed inbox item.
//
// TODO(HIGH): replace this derived `Debug` with a redacted one — it carries evidence.
//
// The `Event` variant wraps a `Box<AgentHarnessEvent>`, and AGENTS.md requires a redacted `Debug` on
// anything evidence-bearing. It prints only field types today because the inner event's own `Debug`
// redacts, but that safety is incidental rather than declared: nothing here states the requirement, so
// it breaks the moment a third field is added.
//
// Use `impl_redacted_debug!`. See A-10 in `docs/AUDIT.md`, which pairs this with `evidra_engine`'s
// `Evidence<'a>` — same accident, same fix, and the engine case additionally needs the macro exported
// or a hand-written impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimedHarnessContent {
    /// One validated harness event.
    Event(Box<AgentHarnessEvent>),
    /// A deterministic content failure that should be quarantined.
    Quarantine(QuarantineReason),
}

/// External boundary for claiming and resolving agent-harness inbox items.
pub trait AgentHarnessInbox {
    /// Opaque, single-owner claim type.
    type Claim;
    /// Adapter-specific operational error.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Returns the next claim, or `None` when the bounded snapshot is exhausted.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when claim acquisition fails.
    fn next_claim(&mut self) -> Result<Option<Self::Claim>, Self::Error>;

    /// Reads and classifies one claim without consuming it.
    ///
    /// # Errors
    ///
    /// Returns an adapter error for operational read failures.
    fn read_claim(&mut self, claim: &Self::Claim) -> Result<ClaimedHarnessContent, Self::Error>;

    /// Removes a successfully persisted or duplicate claim.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when completion fails.
    fn complete(&mut self, claim: Self::Claim) -> Result<(), Self::Error>;

    /// Retains a rejected claim with a fixed reason.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when quarantine fails.
    fn quarantine(
        &mut self,
        claim: Self::Claim,
        reason: QuarantineReason,
    ) -> Result<(), Self::Error>;
}

/// Aggregate result of one bounded inbox ingestion invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct AgentHarnessIngestSummary {
    recorded: u64,
    duplicate: u64,
    quarantined: u64,
    reasons: BTreeMap<QuarantineReason, u64>,
}

impl AgentHarnessIngestSummary {
    /// Returns the number of observations newly recorded by this ingestion run.
    #[must_use]
    pub fn recorded(&self) -> u64 {
        self.recorded
    }

    /// Returns the number of duplicate events completed by this ingestion run.
    #[must_use]
    pub fn duplicate(&self) -> u64 {
        self.duplicate
    }

    /// Returns the number of claims quarantined by this ingestion run.
    #[must_use]
    pub fn quarantined(&self) -> u64 {
        self.quarantined
    }

    /// Returns nonzero quarantine counts ordered by their fixed reason values.
    #[must_use]
    pub fn reasons(&self) -> &BTreeMap<QuarantineReason, u64> {
        &self.reasons
    }

    /// Increments the total and reason-specific counters for a quarantined claim.
    fn record_quarantine(&mut self, reason: QuarantineReason) {
        self.quarantined += 1;
        *self.reasons.entry(reason).or_insert(0) += 1;
    }
}

/// Source-safe operational failure category for inbox ingestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Diagnostic)]
pub enum AgentHarnessIngestError {
    /// Inbox lifecycle operation failed.
    #[error("ingest failed: inbox")]
    #[diagnostic(code(evidra::agent_harness_ingest::inbox))]
    Inbox,
    /// Validated event conversion failed.
    #[error("ingest failed: conversion")]
    #[diagnostic(code(evidra::agent_harness_ingest::conversion))]
    Conversion,
    /// Observation persistence failed.
    #[error("ingest failed: store")]
    #[diagnostic(code(evidra::agent_harness_ingest::store))]
    Store,
}

/// Ingests one bounded inbox snapshot using deterministic lifecycle policy.
///
/// # Errors
///
/// Returns a fixed operational category while leaving the active claim retryable whenever its
/// lifecycle operation did not complete.
pub fn ingest_agent_harness<I, S>(
    inbox: &mut I,
    store: &mut S,
    collector: &str,
) -> Result<AgentHarnessIngestSummary, AgentHarnessIngestError>
where
    I: AgentHarnessInbox,
    S: ObservationStore,
{
    let mut summary = AgentHarnessIngestSummary::default();
    loop {
        let Some(claim) = inbox
            .next_claim()
            .map_err(|_| AgentHarnessIngestError::Inbox)?
        else {
            return Ok(summary);
        };
        match inbox
            .read_claim(&claim)
            .map_err(|_| AgentHarnessIngestError::Inbox)?
        {
            ClaimedHarnessContent::Quarantine(reason) => {
                inbox
                    .quarantine(claim, reason)
                    .map_err(|_| AgentHarnessIngestError::Inbox)?;
                summary.record_quarantine(reason);
            }
            ClaimedHarnessContent::Event(event) => {
                let value = AgentHarnessObservation::record(*event, collector)
                    .map_err(|_| AgentHarnessIngestError::Conversion)?;
                match store
                    .append_harness_observation(&value)
                    .map_err(|_| AgentHarnessIngestError::Store)?
                {
                    AgentHarnessAppendOutcome::Recorded => {
                        inbox
                            .complete(claim)
                            .map_err(|_| AgentHarnessIngestError::Inbox)?;
                        summary.recorded += 1;
                    }
                    AgentHarnessAppendOutcome::Duplicate => {
                        inbox
                            .complete(claim)
                            .map_err(|_| AgentHarnessIngestError::Inbox)?;
                        summary.duplicate += 1;
                    }
                    AgentHarnessAppendOutcome::IdentityConflict => {
                        inbox
                            .quarantine(claim, QuarantineReason::IdentityConflict)
                            .map_err(|_| AgentHarnessIngestError::Inbox)?;
                        summary.record_quarantine(QuarantineReason::IdentityConflict);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, VecDeque};

    use chrono::Utc;
    use serde_json::json;

    use crate::{
        AgentHarnessAppendOutcome, AgentHarnessEvent, AgentHarnessEventDraft,
        AgentHarnessObservation, FacetValue, HarnessEventType, HarnessRef, HarnessSessionId,
        Observation, ObservationFacet, ObservationStore, RedactionRecord, SourceEventId, SourceRef,
        SubjectRef,
    };

    use super::{
        AgentHarnessInbox, AgentHarnessIngestError, AgentHarnessIngestSummary,
        ClaimedHarnessContent, QuarantineReason, ingest_agent_harness,
    };

    /// Builds a valid harness event for ingestion policy tests.
    fn event() -> AgentHarnessEvent {
        AgentHarnessEvent::new(AgentHarnessEventDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("agent-harness", "session#event")
                .expect("source should be valid"),
            subject: SubjectRef::new("repository", "/tmp/example")
                .expect("subject should be valid"),
            source_event_id: SourceEventId::new("event-1").expect("event ID should be valid"),
            harness: HarnessRef::new("claude-code", None).expect("harness should be valid"),
            session_id: HarnessSessionId::new("session-1").expect("session should be valid"),
            event_type: HarnessEventType::new("tool-completed")
                .expect("event type should be valid"),
            redaction: RedactionRecord::new("test", "1", Vec::new())
                .expect("redaction should be valid"),
            excerpts: Vec::new(),
            facets: vec![
                ObservationFacet::new("success", FacetValue::Boolean(true))
                    .expect("facet should be valid"),
            ],
        })
        .expect("event should be valid")
    }

    #[test]
    /// Verifies that quarantine reasons retain their stable kebab-case representation.
    fn quarantine_reasons_have_stable_kebab_case() {
        let cases = [
            (QuarantineReason::InvalidJson, "invalid-json"),
            (QuarantineReason::UnsupportedSchema, "unsupported-schema"),
            (QuarantineReason::InvalidReference, "invalid-reference"),
            (QuarantineReason::InvalidEvent, "invalid-event"),
            (QuarantineReason::RecordTooLarge, "record-too-large"),
            (QuarantineReason::BlankInputLimit, "blank-input-limit"),
            (QuarantineReason::EmptyEvent, "empty-event"),
            (QuarantineReason::MultipleEvents, "multiple-events"),
            (QuarantineReason::IdentityConflict, "identity-conflict"),
            (
                QuarantineReason::IncompleteQuarantine,
                "incomplete-quarantine",
            ),
        ];

        for (reason, expected) in cases {
            assert_eq!(reason.to_string(), expected);
            assert_eq!(
                serde_json::to_value(reason).expect("reason should serialize"),
                json!(expected)
            );
        }
    }

    #[test]
    /// Verifies that summaries serialize only supplied reasons in stable key order.
    fn ingest_summary_serializes_with_sorted_nonzero_reasons() {
        let summary = AgentHarnessIngestSummary {
            recorded: 3,
            duplicate: 2,
            quarantined: 2,
            reasons: BTreeMap::from([
                (QuarantineReason::InvalidJson, 1),
                (QuarantineReason::IdentityConflict, 1),
            ]),
        };

        assert_eq!(
            serde_json::to_value(summary).expect("summary should serialize"),
            json!({
                "recorded": 3,
                "duplicate": 2,
                "quarantined": 2,
                "reasons": {
                    "identity-conflict": 1,
                    "invalid-json": 1
                }
            })
        );
    }

    struct OpaqueClaim(String);

    #[derive(Debug, thiserror::Error)]
    #[error("fake inbox error")]
    struct FakeError;

    struct FakeInbox;

    impl AgentHarnessInbox for FakeInbox {
        type Claim = OpaqueClaim;
        type Error = FakeError;

        /// Returns an opaque, non-clone claim for the port contract test.
        fn next_claim(&mut self) -> Result<Option<Self::Claim>, Self::Error> {
            Ok(Some(OpaqueClaim("claim".to_owned())))
        }

        /// Classifies the fake claim as invalid JSON without consuming it.
        fn read_claim(
            &mut self,
            _claim: &Self::Claim,
        ) -> Result<super::ClaimedHarnessContent, Self::Error> {
            Ok(super::ClaimedHarnessContent::Quarantine(
                QuarantineReason::InvalidJson,
            ))
        }

        /// Consumes the expected opaque claim.
        fn complete(&mut self, claim: Self::Claim) -> Result<(), Self::Error> {
            assert_eq!(claim.0, "claim");
            Ok(())
        }

        /// Accepts the expected opaque claim for quarantine.
        fn quarantine(
            &mut self,
            claim: Self::Claim,
            _reason: QuarantineReason,
        ) -> Result<(), Self::Error> {
            assert_eq!(claim.0, "claim");
            Ok(())
        }
    }

    #[test]
    /// Verifies that inbox implementations may use opaque claims without `Clone`.
    fn inbox_port_supports_opaque_non_clone_claims() {
        let mut inbox = FakeInbox;
        let claim = inbox
            .next_claim()
            .expect("claim lookup should succeed")
            .expect("claim should exist");

        inbox.complete(claim).expect("claim should be consumed");
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeInboxError {
        Failed,
    }

    impl std::fmt::Display for FakeInboxError {
        /// Writes the fixed inbox failure message used by policy tests.
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("fake inbox failure")
        }
    }

    impl std::error::Error for FakeInboxError {}

    struct PolicyInbox {
        pending: VecDeque<(u64, Result<ClaimedHarnessContent, FakeInboxError>)>,
        active: HashMap<u64, Result<ClaimedHarnessContent, FakeInboxError>>,
        completed: Vec<u64>,
        quarantined: Vec<(u64, QuarantineReason)>,
        fail_next: bool,
        fail_complete: bool,
        fail_quarantine: bool,
    }

    impl PolicyInbox {
        /// Creates an inbox whose supplied contents receive sequential claim identifiers.
        fn new(contents: Vec<ClaimedHarnessContent>) -> Self {
            Self {
                pending: contents
                    .into_iter()
                    .enumerate()
                    .map(|(index, content)| (index as u64, Ok(content)))
                    .collect(),
                active: HashMap::new(),
                completed: Vec::new(),
                quarantined: Vec::new(),
                fail_next: false,
                fail_complete: false,
                fail_quarantine: false,
            }
        }
    }

    impl AgentHarnessInbox for PolicyInbox {
        type Claim = u64;
        type Error = FakeInboxError;

        /// Claims the next pending test item unless claim acquisition is configured to fail.
        fn next_claim(&mut self) -> Result<Option<Self::Claim>, Self::Error> {
            if self.fail_next {
                return Err(FakeInboxError::Failed);
            }
            let Some((claim, content)) = self.pending.pop_front() else {
                return Ok(None);
            };
            self.active.insert(claim, content);
            Ok(Some(claim))
        }

        /// Returns the configured classification for an active claim.
        fn read_claim(
            &mut self,
            claim: &Self::Claim,
        ) -> Result<ClaimedHarnessContent, Self::Error> {
            self.active
                .get(claim)
                .cloned()
                .unwrap_or(Err(FakeInboxError::Failed))
        }

        /// Removes an active claim and records its successful completion.
        fn complete(&mut self, claim: Self::Claim) -> Result<(), Self::Error> {
            if self.fail_complete {
                return Err(FakeInboxError::Failed);
            }
            self.active.remove(&claim);
            self.completed.push(claim);
            Ok(())
        }

        /// Removes an active claim and records its quarantine reason.
        fn quarantine(
            &mut self,
            claim: Self::Claim,
            reason: QuarantineReason,
        ) -> Result<(), Self::Error> {
            if self.fail_quarantine {
                return Err(FakeInboxError::Failed);
            }
            self.active.remove(&claim);
            self.quarantined.push((claim, reason));
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct FakeStoreError;

    impl std::fmt::Display for FakeStoreError {
        /// Writes the fixed store failure message used by policy tests.
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("fake store failure")
        }
    }

    impl std::error::Error for FakeStoreError {}

    struct FakeStore {
        outcomes: VecDeque<Result<AgentHarnessAppendOutcome, FakeStoreError>>,
    }

    impl ObservationStore for FakeStore {
        type Error = FakeStoreError;

        /// Accepts generic observations for conformance with the store port.
        fn append(&mut self, _observation: &Observation) -> Result<(), Self::Error> {
            Ok(())
        }

        /// Returns an empty observation list because ingestion tests do not query history.
        fn list(&self, _limit: usize) -> Result<Vec<Observation>, Self::Error> {
            Ok(Vec::new())
        }

        /// Returns the next configured atomic harness-append outcome.
        fn append_harness_observation(
            &mut self,
            _value: &AgentHarnessObservation,
        ) -> Result<AgentHarnessAppendOutcome, Self::Error> {
            self.outcomes
                .pop_front()
                .unwrap_or(Ok(AgentHarnessAppendOutcome::Recorded))
        }
    }

    #[test]
    /// Verifies that a newly recorded event completes its inbox claim.
    fn ingest_records_and_completes_new_event() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Event(Box::new(event()))]);
        let mut store = FakeStore {
            outcomes: VecDeque::from([Ok(AgentHarnessAppendOutcome::Recorded)]),
        };

        let summary =
            ingest_agent_harness(&mut inbox, &mut store, "test").expect("ingestion should succeed");

        assert_eq!(summary.recorded(), 1);
        assert_eq!(inbox.completed, [0]);
    }

    #[test]
    /// Verifies that a content failure is quarantined before ingestion continues.
    fn ingest_quarantines_content_failure_and_continues() {
        let mut inbox = PolicyInbox::new(vec![
            ClaimedHarnessContent::Quarantine(QuarantineReason::InvalidJson),
            ClaimedHarnessContent::Event(Box::new(event())),
        ]);
        let mut store = FakeStore {
            outcomes: VecDeque::from([Ok(AgentHarnessAppendOutcome::Recorded)]),
        };

        let summary = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect("ingestion should continue");

        assert_eq!(summary.recorded(), 1);
        assert_eq!(summary.quarantined(), 1);
        assert_eq!(inbox.quarantined, [(0, QuarantineReason::InvalidJson)]);
    }

    #[test]
    /// Verifies that conversion failure leaves the active claim unconsumed.
    fn conversion_failure_stops_without_consuming_claim() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Event(Box::new(event()))]);
        let mut store = FakeStore {
            outcomes: VecDeque::new(),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, " ")
            .expect_err("blank collector should fail conversion");

        assert_eq!(error, AgentHarnessIngestError::Conversion);
        assert!(inbox.completed.is_empty());
        assert!(inbox.quarantined.is_empty());
    }

    #[test]
    /// Verifies that persistence failure leaves the active claim unconsumed.
    fn store_failure_stops_without_consuming_claim() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Event(Box::new(event()))]);
        let mut store = FakeStore {
            outcomes: VecDeque::from([Err(FakeStoreError)]),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect_err("store failure should stop");

        assert_eq!(error, AgentHarnessIngestError::Store);
        assert!(inbox.completed.is_empty());
    }

    #[test]
    /// Verifies that a duplicate event completes its inbox claim.
    fn ingest_completes_duplicate() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Event(Box::new(event()))]);
        let mut store = FakeStore {
            outcomes: VecDeque::from([Ok(AgentHarnessAppendOutcome::Duplicate)]),
        };

        let summary = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect("duplicate should complete");

        assert_eq!(summary.duplicate(), 1);
        assert_eq!(inbox.completed, [0]);
    }

    #[test]
    /// Verifies that an identity conflict quarantines its inbox claim.
    fn ingest_quarantines_identity_conflict() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Event(Box::new(event()))]);
        let mut store = FakeStore {
            outcomes: VecDeque::from([Ok(AgentHarnessAppendOutcome::IdentityConflict)]),
        };

        let summary = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect("conflict should quarantine");

        assert_eq!(summary.quarantined(), 1);
        assert_eq!(inbox.quarantined, [(0, QuarantineReason::IdentityConflict)]);
    }

    #[test]
    /// Verifies that claim acquisition failure maps to the fixed inbox error category.
    fn next_claim_failure_stops_with_fixed_error() {
        let mut inbox = PolicyInbox::new(Vec::new());
        inbox.fail_next = true;
        let mut store = FakeStore {
            outcomes: VecDeque::new(),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect_err("claim failure should stop");

        assert_eq!(error, AgentHarnessIngestError::Inbox);
    }

    #[test]
    /// Verifies that completion failure stops ingestion before incrementing the summary.
    fn complete_failure_stops_before_counting() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Event(Box::new(event()))]);
        inbox.fail_complete = true;
        let mut store = FakeStore {
            outcomes: VecDeque::from([Ok(AgentHarnessAppendOutcome::Recorded)]),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect_err("complete failure should stop");

        assert_eq!(error, AgentHarnessIngestError::Inbox);
        assert!(inbox.completed.is_empty());
    }

    #[test]
    /// Verifies that quarantine failure stops ingestion before incrementing the summary.
    fn quarantine_failure_stops_before_counting() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Quarantine(
            QuarantineReason::InvalidJson,
        )]);
        inbox.fail_quarantine = true;
        let mut store = FakeStore {
            outcomes: VecDeque::new(),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect_err("quarantine failure should stop");

        assert_eq!(error, AgentHarnessIngestError::Inbox);
        assert!(inbox.quarantined.is_empty());
    }

    #[test]
    /// Verifies that claim-read failure leaves the active claim unconsumed.
    fn read_claim_failure_stops_without_consuming_claim() {
        let mut inbox = PolicyInbox::new(Vec::new());
        inbox.pending.push_back((0, Err(FakeInboxError::Failed)));
        let mut store = FakeStore {
            outcomes: VecDeque::new(),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect_err("read failure should stop");

        assert_eq!(error, AgentHarnessIngestError::Inbox);
        assert!(inbox.completed.is_empty());
        assert!(inbox.quarantined.is_empty());
    }

    #[test]
    /// Verifies that duplicate completion failure occurs before duplicate counting.
    fn duplicate_complete_failure_stops_before_counting() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Event(Box::new(event()))]);
        inbox.fail_complete = true;
        let mut store = FakeStore {
            outcomes: VecDeque::from([Ok(AgentHarnessAppendOutcome::Duplicate)]),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect_err("duplicate completion failure should stop");

        assert_eq!(error, AgentHarnessIngestError::Inbox);
        assert!(inbox.completed.is_empty());
    }

    #[test]
    /// Verifies that conflict quarantine failure occurs before quarantine counting.
    fn conflict_quarantine_failure_stops_before_counting() {
        let mut inbox = PolicyInbox::new(vec![ClaimedHarnessContent::Event(Box::new(event()))]);
        inbox.fail_quarantine = true;
        let mut store = FakeStore {
            outcomes: VecDeque::from([Ok(AgentHarnessAppendOutcome::IdentityConflict)]),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect_err("conflict quarantine failure should stop");

        assert_eq!(error, AgentHarnessIngestError::Inbox);
        assert!(inbox.quarantined.is_empty());
    }

    #[test]
    /// Verifies that a later store failure returns an error rather than a partial summary.
    fn later_store_failure_returns_no_partial_summary() {
        let mut inbox = PolicyInbox::new(vec![
            ClaimedHarnessContent::Event(Box::new(event())),
            ClaimedHarnessContent::Event(Box::new(event())),
        ]);
        let mut store = FakeStore {
            outcomes: VecDeque::from([
                Ok(AgentHarnessAppendOutcome::Recorded),
                Err(FakeStoreError),
            ]),
        };

        let error = ingest_agent_harness(&mut inbox, &mut store, "test")
            .expect_err("later store failure should discard the summary");

        assert_eq!(error, AgentHarnessIngestError::Store);
        assert_eq!(inbox.completed, [0]);
        assert!(inbox.active.contains_key(&1));
    }
}

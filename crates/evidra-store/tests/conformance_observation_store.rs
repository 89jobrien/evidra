//! §12 — `ObservationStore` port substitutability.
//!
//! One shared assertion body, run against every implementation of the port. The body is written
//! against the clauses in [`docs/conformance.md`](../../../docs/conformance.md) and never against a
//! concrete store, so a second implementation cannot quietly diverge from the first.
//!
//! `MemoryStore` is the reference implementation: it is written from the clauses alone and exists so
//! that a failing §12 clause can be attributed to a wrong clause rather than to a wrong store.

use std::collections::HashMap;
use std::fmt;

use chrono::Utc;
use evidra_core::{
    AgentHarnessAppendOutcome, AgentHarnessEvent, AgentHarnessEventDraft, AgentHarnessObservation,
    FacetValue, HarnessEventType, HarnessRef, HarnessSessionId, Observation, ObservationDraft,
    ObservationFacet, ObservationKind, ObservationStore, Provenance, RedactedExcerpt,
    RedactionRecord, SourceEventId, SourceRef, SubjectRef,
};
use serde_json::json;
use tempfile::TempDir;

use evidra_store::SqliteObservationStore;

/// Builds a valid manual-intervention observation fixture.
fn observation() -> Observation {
    Observation::record(ObservationDraft {
        occurred_at: Utc::now(),
        source: SourceRef::new("manual", "cli").expect("source should be valid"),
        kind: ObservationKind::ManualIntervention,
        subject: SubjectRef::new("repository", "/tmp/example").expect("subject should be valid"),
        payload: json!({ "summary": "Removed stale generated artifacts" }),
        provenance: Provenance::direct("evidra-cli").expect("provenance should be valid"),
    })
    .expect("observation should be valid")
}

/// Builds a valid agent-harness observation fixture with the requested verification result.
///
/// The identity triple is fixed, so two fixtures built with different results share an identity and
/// differ only in semantic content. That is exactly the §12.6 conflict case.
fn harness_observation(result: &str) -> AgentHarnessObservation {
    let event = AgentHarnessEvent::new(AgentHarnessEventDraft {
        occurred_at: "2026-09-19T08:00:00Z"
            .parse()
            .expect("timestamp should be valid"),
        source: SourceRef::new("agent-harness", "session.jsonl#event-17")
            .expect("source should be valid"),
        subject: SubjectRef::new("repository", "/tmp/example").expect("subject should be valid"),
        source_event_id: SourceEventId::new("event-17").expect("source event ID should be valid"),
        harness: HarnessRef::new("claude-code", Some("1.0".to_owned()))
            .expect("harness should be valid"),
        session_id: HarnessSessionId::new("session-1").expect("session should be valid"),
        event_type: HarnessEventType::new("tool-completed").expect("event type should be valid"),
        redaction: RedactionRecord::new("obfsck", "1", vec!["obfuscate:secret".to_owned()])
            .expect("redaction should be valid"),
        excerpts: vec![
            RedactedExcerpt::new("tool-output", "tests passed").expect("excerpt should be valid"),
        ],
        facets: vec![
            ObservationFacet::new("verification.result", FacetValue::Text(result.to_owned()))
                .expect("facet should be valid"),
        ],
    })
    .expect("event should be valid");
    AgentHarnessObservation::record(event, "evidra-cli").expect("harness observation should record")
}

/// Returns every stored record, ignoring the read bound.
fn stored<S: ObservationStore>(store: &S) -> Vec<Observation> {
    store.list(usize::MAX).expect("list should succeed")
}

/// Returns the stored record identities in the order `list` reports them.
fn stored_ids<S: ObservationStore>(store: &S) -> Vec<String> {
    stored(store)
        .into_iter()
        .map(|value| value.id().to_string())
        .collect()
}

/// Asserts every §12 clause against one `ObservationStore` implementation.
///
/// The clauses run in order and share the store, because several of them are about what a *rejected*
/// append left behind; isolating each into a fresh store would test less than the clause claims.
fn assert_observation_store_contract<S: ObservationStore>(mut store: S) {
    assert!(
        stored_ids(&store).is_empty(),
        "§12.1: a store must start empty for this contract to mean anything"
    );

    let manual = observation();
    assert_append_is_never_a_replacement(&mut store, &manual);

    let harness = harness_observation("passed");
    assert_harness_append_requires_a_receipt(&mut store, &harness);
    assert_harness_append_is_idempotent_and_atomic(&mut store, &harness);

    assert_retrieval_contract(&store, &[manual, harness.observation().clone()]);
}

/// §12.1 — appending the same observation twice never changes what is stored.
fn assert_append_is_never_a_replacement<S: ObservationStore>(store: &mut S, value: &Observation) {
    store.append(value).expect("first append should succeed");
    let before = stored_ids(store);
    assert_eq!(
        before.len(),
        1,
        "§12.1: the first append must store exactly one observation"
    );

    // Whether the store rejects the second append or reports success, the stored set must not move.
    let _ = store.append(value);
    assert_eq!(
        before,
        stored_ids(store),
        "§12.1: appending an existing identity must not replace or mutate the stored record"
    );

    let survivor = &stored(store)[0];
    assert_eq!(
        survivor.id(),
        value.id(),
        "§12.1: the surviving record must be the one originally appended"
    );
    assert_eq!(
        survivor.integrity().digest(),
        value.integrity().digest(),
        "§12.1: the surviving record must keep its original integrity digest"
    );
}

/// §12.4 — `append` rejects harness observations so the receipt commits atomically.
fn assert_harness_append_requires_a_receipt<S: ObservationStore>(
    store: &mut S,
    value: &AgentHarnessObservation,
) {
    let before = stored_ids(store);

    store
        .append(value.observation())
        .expect_err("§12.4: append must reject a harness observation");

    assert_eq!(
        before,
        stored_ids(store),
        "§12.4: a rejected harness append must write nothing"
    );
}

/// §12.5 and §12.6 — duplicate versus conflict, and the atomicity of the rejected path.
fn assert_harness_append_is_idempotent_and_atomic<S: ObservationStore>(
    store: &mut S,
    value: &AgentHarnessObservation,
) {
    let before = stored_ids(store);

    assert_eq!(
        store
            .append_harness_observation(value)
            .expect("first harness append should succeed"),
        AgentHarnessAppendOutcome::Recorded,
        "§12.5: the first harness append must record"
    );
    let after_first = stored_ids(store);
    assert_eq!(
        after_first.len(),
        before.len() + 1,
        "§12.5: a recorded harness append must add exactly one observation"
    );

    assert_eq!(
        store
            .append_harness_observation(value)
            .expect("duplicate harness append should succeed"),
        AgentHarnessAppendOutcome::Duplicate,
        "§12.5: an identical harness append must be a duplicate, not a second record"
    );
    assert_eq!(
        after_first,
        stored_ids(store),
        "§12.5: a duplicate must not add or replace a record"
    );

    let conflicting = harness_observation("failed");
    assert_eq!(
        conflicting.identity(),
        value.identity(),
        "§12.6: the conflicting fixture must reuse the recorded identity"
    );
    assert_eq!(
        store
            .append_harness_observation(&conflicting)
            .expect("conflicting harness append should succeed"),
        AgentHarnessAppendOutcome::IdentityConflict,
        "§12.6: a reused identity with different content must be a conflict, not a duplicate"
    );
    assert_eq!(
        after_first,
        stored_ids(store),
        "§12.6: a conflict must commit no observation without its receipt"
    );
}

/// §12.2 and §12.3 — round-trip fidelity, bounded reads, and newest-first ordering.
fn assert_retrieval_contract<S: ObservationStore>(store: &S, expected: &[Observation]) {
    let records = stored(store);
    assert_eq!(
        records.len(),
        expected.len(),
        "§12.2: every appended observation must be retrievable and nothing else may appear"
    );

    for original in expected {
        let retrieved = records
            .iter()
            .find(|candidate| candidate.id() == original.id())
            .unwrap_or_else(|| panic!("§12.2: observation {} was not retrievable", original.id()));
        assert_eq!(
            retrieved, original,
            "§12.2: a retrieved observation must equal the appended record"
        );
        assert!(
            retrieved
                .verify_integrity()
                .expect("§12.2: integrity verification should complete"),
            "§12.2: a retrieved observation must still verify against its digest"
        );
    }

    assert!(
        records
            .windows(2)
            .all(|pair| pair[0].observed_at() >= pair[1].observed_at()),
        "§12.3: list must be ordered by non-increasing observed_at"
    );
    assert!(
        store
            .list(0)
            .expect("§12.3: a zero limit should succeed")
            .is_empty(),
        "§12.3: list(0) must be empty"
    );
    assert!(
        store
            .list(1)
            .expect("§12.3: a unit limit should succeed")
            .len()
            <= 1,
        "§12.3: list(1) must return at most one record"
    );
}

// ---------------------------------------------------------------------------
// Production implementation
// ---------------------------------------------------------------------------

#[test]
fn sqlite_observation_store_satisfies_contract() {
    let directory = TempDir::new().expect("tempdir should create");
    let store = SqliteObservationStore::initialize(&directory.path().join("evidra.db"))
        .expect("store should initialize");

    assert_observation_store_contract(store);
}

// ---------------------------------------------------------------------------
// Reference implementation
// ---------------------------------------------------------------------------

/// Error returned by the reference in-memory store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryStoreError {
    /// A harness observation was submitted through the generic append path.
    HarnessObservationRequiresReceipt,
    /// An identity was appended twice.
    DuplicateObservation,
}

impl fmt::Display for MemoryStoreError {
    /// Writes a fixed failure message that names no stored value.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::HarnessObservationRequiresReceipt => {
                "agent harness observations must be appended with an idempotency receipt"
            }
            Self::DuplicateObservation => "observation already exists",
        })
    }
}

impl std::error::Error for MemoryStoreError {}

/// Reference `ObservationStore` written from the §12 clauses alone.
struct MemoryStore {
    observations: Vec<Observation>,
    receipts: HashMap<(String, String, String), String>,
}

impl MemoryStore {
    /// Creates an empty reference store.
    fn new() -> Self {
        Self {
            observations: Vec::new(),
            receipts: HashMap::new(),
        }
    }
}

impl ObservationStore for MemoryStore {
    type Error = MemoryStoreError;

    /// §12.4, §12.1 — rejects harness observations, then refuses to replace an existing identity.
    fn append(&mut self, observation: &Observation) -> Result<(), Self::Error> {
        if observation.kind() == &ObservationKind::AgentHarnessEvent {
            return Err(MemoryStoreError::HarnessObservationRequiresReceipt);
        }
        if self
            .observations
            .iter()
            .any(|stored| stored.id() == observation.id())
        {
            return Err(MemoryStoreError::DuplicateObservation);
        }
        self.observations.push(observation.clone());
        Ok(())
    }

    /// §12.3 — bounded, newest-first listing.
    fn list(&self, limit: usize) -> Result<Vec<Observation>, Self::Error> {
        let mut ordered = self.observations.clone();
        ordered.sort_by(|left, right| {
            right
                .observed_at()
                .cmp(&left.observed_at())
                .then_with(|| right.id().to_string().cmp(&left.id().to_string()))
        });
        ordered.truncate(limit);
        Ok(ordered)
    }

    /// §12.5, §12.6 — the observation and its receipt commit together or not at all.
    fn append_harness_observation(
        &mut self,
        value: &AgentHarnessObservation,
    ) -> Result<AgentHarnessAppendOutcome, Self::Error> {
        let identity = value.identity();
        let key = (
            identity.harness().to_owned(),
            identity.session_id().to_owned(),
            identity.source_event_id().to_owned(),
        );
        if let Some(stored) = self.receipts.get(&key) {
            return Ok(if stored == value.event_digest() {
                AgentHarnessAppendOutcome::Duplicate
            } else {
                AgentHarnessAppendOutcome::IdentityConflict
            });
        }

        self.observations.push(value.observation().clone());
        self.receipts.insert(key, value.event_digest().to_owned());
        Ok(AgentHarnessAppendOutcome::Recorded)
    }
}

#[test]
fn memory_observation_store_satisfies_contract() {
    assert_observation_store_contract(MemoryStore::new());
}

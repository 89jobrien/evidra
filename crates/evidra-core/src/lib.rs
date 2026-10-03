//! Domain model, ingestion policy, and ports for Evidra's append-only evidence ledger.
//!
//! The crate owns immutable observations, validated agent-harness events, deterministic inbox
//! lifecycle policy, and persistence contracts implemented by adapters. It has no dependency on a
//! database, CLI framework, network service, or model provider.
//!
//! # Example
//!
//! ```
//! use chrono::Utc;
//! use evidra_core::{
//!     Observation, ObservationDraft, ObservationKind, Provenance, SourceRef, SubjectRef,
//! };
//! use serde_json::json;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let observation = Observation::record(ObservationDraft {
//!     occurred_at: Utc::now(),
//!     source: SourceRef::new("manual", "evidra note")?,
//!     kind: ObservationKind::ManualIntervention,
//!     subject: SubjectRef::new("repository", "/workspace")?,
//!     payload: json!({"summary": "Updated recovery instructions"}),
//!     provenance: Provenance::direct("evidra-cli")?,
//! })?;
//!
//! assert!(observation.verify_integrity()?);
//! # Ok(())
//! # }
//! ```

mod derivation;
mod harness;
mod ingest;
mod observation;
mod ports;

pub use derivation::{
    ConfidenceBand, Contradiction, CurrentFacet, Derivation, DerivationDraft, DerivationError,
    DerivationId, DerivationKind, DerivationMethod, DerivationScope, EvidenceRole, EvidenceTarget,
    FacetCount, FacetFilter, FacetProjection, FacetValueSlot, Freshness, RelationKind,
    Relationship, RelationshipId, ScopeFidelity, UncertaintyProfile,
};
pub use harness::{
    AgentHarnessEvent, AgentHarnessEventDraft, AgentHarnessEventError, AgentHarnessEventIdentity,
    AgentHarnessObservation, AgentHarnessObservationError, FacetValue, HarnessEventType,
    HarnessRef, HarnessSessionId, ObservationFacet, RedactedExcerpt, RedactionRecord,
    SourceEventId,
};
pub use ingest::{
    AgentHarnessInbox, AgentHarnessIngestError, AgentHarnessIngestSummary, ClaimedHarnessContent,
    QuarantineReason, ingest_agent_harness,
};
pub use observation::{
    IntegrityRecord, Observation, ObservationDraft, ObservationError, ObservationId,
    ObservationKind, Provenance, SourceRef, SubjectRef,
};
pub use ports::{
    AgentHarnessAppendOutcome, AgentHarnessEventSource, DerivationStore, ObservationStore,
    RelationshipStore,
};

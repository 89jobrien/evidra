//! Validated, redacted evidence reported by local engineering-agent harnesses.
//!
//! This module defines the bounded event contract accepted from trusted local producers, converts
//! accepted events into append-only [`Observation`] values, and derives stable identity and
//! semantic-digest metadata for idempotent persistence. Custom `Debug` implementations deliberately
//! omit event contents so diagnostics cannot disclose retained evidence.

use std::fmt;

use chrono::{DateTime, Utc};
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    Observation, ObservationDraft, ObservationError, ObservationKind, Provenance, SourceRef,
    SubjectRef,
};

const AGENT_HARNESS_SOURCE_KIND: &str = "agent-harness";
const MAX_EXCERPTS: usize = 8;
const MAX_EXCERPT_BYTES: usize = 8 * 1024;
const MAX_FACETS: usize = 64;
const HARNESS_OBSERVATION_SCHEMA: &str = "evidra.agent-harness-observation/v1";
const HARNESS_DIGEST_SCHEMA: &str = "evidra.agent-harness-event-digest/v1";

/// Stable identity assigned to an event by its source harness.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SourceEventId {
    value: String,
}

/// Validates and exposes a source harness's event identifier.
impl SourceEventId {
    /// Preserves `value` exactly after rejecting identifiers that contain only whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessEventError::BlankField`] when `value` is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, AgentHarnessEventError> {
        Ok(Self {
            value: validated("source_event_id", value.into())?,
        })
    }

    /// Borrows the source-provided identifier without normalization.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Identifies the harness that reported an event.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct HarnessRef {
    name: String,
    version: Option<String>,
}

/// Validates and exposes the producer name and its optional reported version.
impl HarnessRef {
    /// Preserves the supplied name and version after rejecting blank values.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessEventError::BlankField`] when a supplied value is blank.
    pub fn new(
        name: impl Into<String>,
        version: Option<String>,
    ) -> Result<Self, AgentHarnessEventError> {
        Ok(Self {
            name: validated("harness.name", name.into())?,
            version: version
                .map(|value| validated("harness.version", value))
                .transpose()?,
        })
    }

    /// Borrows the producer's harness name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Borrows the producer-reported version, or returns `None` when it was omitted.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

/// Stable identity for one harness session.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct HarnessSessionId {
    value: String,
}

/// Validates and exposes a source harness's session identifier.
impl HarnessSessionId {
    /// Preserves `value` exactly after rejecting identifiers that contain only whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessEventError::BlankField`] when `value` is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, AgentHarnessEventError> {
        Ok(Self {
            value: validated("session_id", value.into())?,
        })
    }

    /// Borrows the source-provided session identifier without normalization.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Open vocabulary describing a harness lifecycle event.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct HarnessEventType {
    value: String,
}

/// Validates and exposes the producer-defined lifecycle classification.
impl HarnessEventType {
    /// Preserves the open-vocabulary event type after rejecting blank values.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessEventError::BlankField`] when `value` is blank.
    pub fn new(value: impl Into<String>) -> Result<Self, AgentHarnessEventError> {
        Ok(Self {
            value: validated("event_type", value.into())?,
        })
    }

    /// Borrows the producer-defined event type without interpreting it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Scalar value reported as an observation facet.
//
// TODO(LOW): document `FacetValue` as a deliberate exemption from the Raw-shadow rule.
//
// `#[serde(untagged)]` structurally cannot honour `deny_unknown_fields`, making this the one place in
// the workspace where the `CONVENTIONS.md` §2 guarantee is impossible by construction. Harmless today
// because all three variants are scalars; a future non-scalar variant would begin silently accepting
// documents that every other persisted type rejects.
//
// The honest resolution is probably to record the exemption rather than fight the derive — the same
// reasoning ADR-010 already accepts for its coverage cost. See A-15 in `docs/AUDIT.md`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FacetValue {
    /// Text facet value.
    Text(String),
    /// Integer facet value.
    Integer(i64),
    /// Boolean facet value.
    Boolean(bool),
}

/// Named scalar metadata reported directly by a harness.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ObservationFacet {
    name: String,
    value: FacetValue,
}

/// Validates and exposes one named scalar attached to an event.
impl ObservationFacet {
    /// Pairs `value` with `name`, preserving both after rejecting a blank name.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessEventError::BlankField`] when `name` is blank.
    pub fn new(name: impl Into<String>, value: FacetValue) -> Result<Self, AgentHarnessEventError> {
        Ok(Self {
            name: validated("facet.name", name.into())?,
            value,
        })
    }

    /// Borrows the producer-defined facet name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Borrows the scalar exactly as reported by the producer.
    #[must_use]
    pub fn value(&self) -> &FacetValue {
        &self.value
    }
}

/// Records the redaction policy applied before Evidra received an event.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RedactionRecord {
    policy: String,
    version: String,
    transformations: Vec<String>,
}

/// Validates and exposes the producer's redaction attestation.
impl RedactionRecord {
    /// Records the policy, version, and transformation names after rejecting any blank entry.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessEventError::BlankField`] when a supplied value is blank.
    pub fn new(
        policy: impl Into<String>,
        version: impl Into<String>,
        transformations: Vec<String>,
    ) -> Result<Self, AgentHarnessEventError> {
        if transformations
            .iter()
            .any(|transformation| transformation.trim().is_empty())
        {
            return Err(AgentHarnessEventError::BlankField {
                field: "redaction.transformations",
            });
        }

        Ok(Self {
            policy: validated("redaction.policy", policy.into())?,
            version: validated("redaction.version", version.into())?,
            transformations,
        })
    }

    /// Borrows the attested redaction policy name.
    #[must_use]
    pub fn policy(&self) -> &str {
        &self.policy
    }

    /// Borrows the attested redaction policy version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Borrows the ordered transformation names attested by the producer.
    #[must_use]
    pub fn transformations(&self) -> &[String] {
        &self.transformations
    }
}

/// Selected redacted text retained from a harness event.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RedactedExcerpt {
    kind: String,
    text: String,
}

/// Validates and exposes one producer-redacted evidence excerpt.
impl RedactedExcerpt {
    /// Preserves the excerpt category and text after rejecting blank values.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessEventError::BlankField`] when either value is blank.
    pub fn new(
        kind: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self, AgentHarnessEventError> {
        Ok(Self {
            kind: validated("excerpt.kind", kind.into())?,
            text: validated("excerpt.text", text.into())?,
        })
    }

    /// Borrows the producer-defined excerpt category.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Borrows the already-redacted text retained as evidence.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Source-provided fields needed to validate an agent-harness event.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentHarnessEventDraft {
    /// Time at which the underlying harness event occurred.
    pub occurred_at: DateTime<Utc>,
    /// Harness transcript or lifecycle event that supplied the evidence.
    pub source: SourceRef,
    /// Entity affected by the harness action.
    pub subject: SubjectRef,
    /// Stable event identity assigned by the source harness.
    pub source_event_id: SourceEventId,
    /// Harness name and optional version.
    pub harness: HarnessRef,
    /// Stable identity for the containing harness session.
    pub session_id: HarnessSessionId,
    /// Open lifecycle event classification.
    pub event_type: HarnessEventType,
    /// Producer-attested redaction metadata.
    pub redaction: RedactionRecord,
    /// Selected bounded redacted excerpts.
    pub excerpts: Vec<RedactedExcerpt>,
    /// Scalar metadata reported directly by the harness.
    pub facets: Vec<ObservationFacet>,
}

/// Validated evidence event reported by an engineering-agent harness.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct AgentHarnessEvent {
    occurred_at: DateTime<Utc>,
    source: SourceRef,
    subject: SubjectRef,
    source_event_id: SourceEventId,
    harness: HarnessRef,
    session_id: HarnessSessionId,
    event_type: HarnessEventType,
    redaction: RedactionRecord,
    excerpts: Vec<RedactedExcerpt>,
    facets: Vec<ObservationFacet>,
}

/// Enforces the normalized harness-event contract and exposes accepted evidence.
impl AgentHarnessEvent {
    /// Accepts a draft only when its source is `agent-harness` and its excerpt and facet
    /// collections satisfy the contract's count and byte limits.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessEventError`] when the source category or collection bounds violate
    /// the harness evidence contract.
    pub fn new(draft: AgentHarnessEventDraft) -> Result<Self, AgentHarnessEventError> {
        if draft.source.kind() != AGENT_HARNESS_SOURCE_KIND {
            return Err(AgentHarnessEventError::UnexpectedSourceKind);
        }
        if draft.excerpts.len() > MAX_EXCERPTS {
            return Err(AgentHarnessEventError::TooManyExcerpts {
                count: draft.excerpts.len(),
                maximum: MAX_EXCERPTS,
            });
        }
        for excerpt in &draft.excerpts {
            if excerpt.text().len() > MAX_EXCERPT_BYTES {
                return Err(AgentHarnessEventError::ExcerptTooLarge {
                    bytes: excerpt.text().len(),
                    maximum: MAX_EXCERPT_BYTES,
                });
            }
        }
        if draft.facets.len() > MAX_FACETS {
            return Err(AgentHarnessEventError::TooManyFacets {
                count: draft.facets.len(),
                maximum: MAX_FACETS,
            });
        }

        Ok(Self {
            occurred_at: draft.occurred_at,
            source: draft.source,
            subject: draft.subject,
            source_event_id: draft.source_event_id,
            harness: draft.harness,
            session_id: draft.session_id,
            event_type: draft.event_type,
            redaction: draft.redaction,
            excerpts: draft.excerpts,
            facets: draft.facets,
        })
    }

    /// Returns the producer-reported time of the underlying harness event.
    #[must_use]
    pub fn occurred_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }

    /// Borrows the source locator, whose kind is guaranteed to be `agent-harness`.
    #[must_use]
    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Borrows the entity that the harness action affected.
    #[must_use]
    pub fn subject(&self) -> &SubjectRef {
        &self.subject
    }

    /// Borrows the event identifier used as part of the persistence identity.
    #[must_use]
    pub fn source_event_id(&self) -> &SourceEventId {
        &self.source_event_id
    }

    /// Borrows the reporting harness name and optional version.
    #[must_use]
    pub fn harness(&self) -> &HarnessRef {
        &self.harness
    }

    /// Borrows the identifier of the session containing this event.
    #[must_use]
    pub fn session_id(&self) -> &HarnessSessionId {
        &self.session_id
    }

    /// Borrows the producer-defined lifecycle classification.
    #[must_use]
    pub fn event_type(&self) -> &HarnessEventType {
        &self.event_type
    }

    /// Borrows the producer's redaction policy attestation.
    #[must_use]
    pub fn redaction(&self) -> &RedactionRecord {
        &self.redaction
    }

    /// Borrows the bounded, already-redacted excerpts in producer order.
    #[must_use]
    pub fn excerpts(&self) -> &[RedactedExcerpt] {
        &self.excerpts
    }

    /// Borrows the bounded scalar facets in producer order.
    #[must_use]
    pub fn facets(&self) -> &[ObservationFacet] {
        &self.facets
    }
}

/// Stable identity used to make harness-event persistence idempotent.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AgentHarnessEventIdentity {
    harness: String,
    session_id: String,
    source_event_id: String,
}

/// Validates and exposes the three-part key used for idempotent persistence.
impl AgentHarnessEventIdentity {
    /// Constructs an identity from harness, session, and source-event components without
    /// normalizing them, while rejecting any component that is blank.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessObservationError::BlankIdentityField`] when a field is blank.
    pub fn new(
        harness: impl Into<String>,
        session_id: impl Into<String>,
        source_event_id: impl Into<String>,
    ) -> Result<Self, AgentHarnessObservationError> {
        Ok(Self {
            harness: validated_identity("identity.harness", harness.into())?,
            session_id: validated_identity("identity.session_id", session_id.into())?,
            source_event_id: validated_identity(
                "identity.source_event_id",
                source_event_id.into(),
            )?,
        })
    }

    /// Borrows the harness-name component of the identity.
    #[must_use]
    pub fn harness(&self) -> &str {
        &self.harness
    }

    /// Borrows the session component of the identity.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Borrows the source-event component of the identity.
    #[must_use]
    pub fn source_event_id(&self) -> &str {
        &self.source_event_id
    }
}

/// Derives the persistence identity from an already validated harness event.
impl From<&AgentHarnessEvent> for AgentHarnessEventIdentity {
    /// Copies the harness name, session identifier, and source event identifier into the key.
    fn from(event: &AgentHarnessEvent) -> Self {
        Self {
            harness: event.harness().name().to_owned(),
            session_id: event.session_id().as_str().to_owned(),
            source_event_id: event.source_event_id().as_str().to_owned(),
        }
    }
}

/// Redacts all identity components from diagnostic output.
impl fmt::Debug for AgentHarnessEventIdentity {
    /// Emits only the type name and a non-exhaustive marker, never the identity values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentHarnessEventIdentity")
            .finish_non_exhaustive()
    }
}

/// Immutable observation paired with its harness idempotency metadata.
#[derive(Clone, PartialEq)]
pub struct AgentHarnessObservation {
    identity: AgentHarnessEventIdentity,
    event_digest: String,
    observation: Observation,
}

/// Creates and verifies append-only observations with their idempotency receipts.
impl AgentHarnessObservation {
    /// Serializes a validated event into the versioned persistence payload, computes its semantic
    /// digest, and records it as an immutable `AgentHarnessEvent` observation.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessObservationError`] when payload or observation construction fails.
    pub fn record(
        event: AgentHarnessEvent,
        collector: impl Into<String>,
    ) -> Result<Self, AgentHarnessObservationError> {
        let identity = AgentHarnessEventIdentity::from(&event);
        let payload = PersistedAgentHarnessPayload::from_event(&event);
        let event_digest = semantic_digest(
            event.occurred_at(),
            event.source(),
            event.subject(),
            &payload,
        )?;
        let payload_value = serde_json::to_value(&payload)
            .map_err(|_| AgentHarnessObservationError::PayloadSerialization)?;
        let provenance =
            Provenance::transformed(collector, vec!["agent-harness-observation/v1".to_owned()])?;
        let observation = Observation::record(ObservationDraft {
            occurred_at: event.occurred_at,
            source: event.source,
            kind: ObservationKind::AgentHarnessEvent,
            subject: event.subject,
            payload: payload_value,
            provenance,
        })?;

        Ok(Self {
            identity,
            event_digest,
            observation,
        })
    }

    /// Borrows the stable three-part source identity used for duplicate detection.
    #[must_use]
    pub fn identity(&self) -> &AgentHarnessEventIdentity {
        &self.identity
    }

    /// Borrows the lowercase SHA-256 digest of the versioned semantic envelope.
    #[must_use]
    pub fn event_digest(&self) -> &str {
        &self.event_digest
    }

    /// Borrows the append-only observation containing the versioned harness payload.
    #[must_use]
    pub fn observation(&self) -> &Observation {
        &self.observation
    }

    /// Reconstructs the persisted event and returns whether both its identity and semantic digest
    /// match the supplied receipt metadata.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessObservationError`] when the digest is not lowercase SHA-256 or the
    /// observation is not a valid versioned harness payload. A well-formed identity or digest
    /// mismatch returns `Ok(false)`.
    pub fn verify_receipt(
        identity: &AgentHarnessEventIdentity,
        event_digest: &str,
        observation: &Observation,
    ) -> Result<bool, AgentHarnessObservationError> {
        if !valid_digest(event_digest) {
            return Err(AgentHarnessObservationError::InvalidEventDigest);
        }
        if observation.kind() != &ObservationKind::AgentHarnessEvent
            || observation.payload().pointer("/harness/version").is_none()
        {
            return Err(AgentHarnessObservationError::InvalidPersistedPayload);
        }
        let payload: PersistedAgentHarnessPayload =
            serde_json::from_value(observation.payload().clone())
                .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)?;
        let event = payload.to_event(observation)?;
        let persisted_identity = AgentHarnessEventIdentity::from(&event);
        if &persisted_identity != identity {
            return Ok(false);
        }
        let expected_digest = semantic_digest(
            observation.occurred_at(),
            observation.source(),
            observation.subject(),
            &payload,
        )?;
        Ok(expected_digest == event_digest)
    }
}

/// Redacts receipt metadata and observation evidence from diagnostic output.
impl fmt::Debug for AgentHarnessObservation {
    /// Emits only the type name and a non-exhaustive marker, never nested evidence or identifiers.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentHarnessObservation")
            .finish_non_exhaustive()
    }
}

/// Error returned while constructing or validating a harness observation.
#[derive(Debug, Clone, PartialEq, Eq, Error, Diagnostic)]
pub enum AgentHarnessObservationError {
    /// A receipt identity component was blank.
    #[error("{field} must not be blank")]
    #[diagnostic(code(evidra::agent_harness_observation::blank_identity_field))]
    BlankIdentityField {
        /// Name of the blank identity field.
        field: &'static str,
    },
    /// A semantic digest was not lowercase SHA-256.
    #[error("invalid agent harness event digest")]
    #[diagnostic(code(evidra::agent_harness_observation::invalid_event_digest))]
    InvalidEventDigest,
    /// The normalized payload could not be serialized.
    #[error("failed to serialize agent harness observation payload")]
    #[diagnostic(code(evidra::agent_harness_observation::payload_serialization))]
    PayloadSerialization,
    /// Persisted harness payload data violated the versioned contract.
    #[error("invalid persisted agent harness observation payload")]
    #[diagnostic(code(evidra::agent_harness_observation::invalid_persisted_payload))]
    InvalidPersistedPayload,
    /// Observation construction failed.
    #[error("agent harness observation construction failed: {0}")]
    Observation(#[from] ObservationError),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAgentHarnessPayload {
    schema: String,
    source_event_id: String,
    harness: PersistedHarnessRef,
    session_id: String,
    event_type: String,
    redaction: PersistedRedaction,
    excerpts: Vec<PersistedExcerpt>,
    facets: Vec<PersistedFacet>,
}

/// Converts between validated events and the exact versioned persistence representation.
impl PersistedAgentHarnessPayload {
    /// Copies a validated event's payload fields into the current schema without its observation
    /// envelope fields.
    fn from_event(event: &AgentHarnessEvent) -> Self {
        Self {
            schema: HARNESS_OBSERVATION_SCHEMA.to_owned(),
            source_event_id: event.source_event_id().as_str().to_owned(),
            harness: PersistedHarnessRef {
                name: event.harness().name().to_owned(),
                version: event.harness().version().map(str::to_owned),
            },
            session_id: event.session_id().as_str().to_owned(),
            event_type: event.event_type().as_str().to_owned(),
            redaction: PersistedRedaction {
                policy: event.redaction().policy().to_owned(),
                version: event.redaction().version().to_owned(),
                transformations: event.redaction().transformations().to_vec(),
            },
            excerpts: event
                .excerpts()
                .iter()
                .map(|excerpt| PersistedExcerpt {
                    kind: excerpt.kind().to_owned(),
                    text: excerpt.text().to_owned(),
                })
                .collect(),
            facets: event
                .facets()
                .iter()
                .map(|facet| PersistedFacet {
                    name: facet.name().to_owned(),
                    value: facet.value().clone(),
                })
                .collect(),
        }
    }

    /// Reconstructs and revalidates an event using envelope fields from `observation`.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHarnessObservationError::InvalidPersistedPayload`] when the schema marker,
    /// nested values, source kind, or collection bounds violate the current contract.
    fn to_event(
        &self,
        observation: &Observation,
    ) -> Result<AgentHarnessEvent, AgentHarnessObservationError> {
        if self.schema != HARNESS_OBSERVATION_SCHEMA {
            return Err(AgentHarnessObservationError::InvalidPersistedPayload);
        }
        let source_event_id = SourceEventId::new(self.source_event_id.clone())
            .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)?;
        let harness = HarnessRef::new(self.harness.name.clone(), self.harness.version.clone())
            .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)?;
        let session_id = HarnessSessionId::new(self.session_id.clone())
            .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)?;
        let event_type = HarnessEventType::new(self.event_type.clone())
            .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)?;
        let redaction = RedactionRecord::new(
            self.redaction.policy.clone(),
            self.redaction.version.clone(),
            self.redaction.transformations.clone(),
        )
        .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)?;
        let excerpts = self
            .excerpts
            .iter()
            .map(|excerpt| RedactedExcerpt::new(excerpt.kind.clone(), excerpt.text.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)?;
        let facets = self
            .facets
            .iter()
            .map(|facet| ObservationFacet::new(facet.name.clone(), facet.value.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)?;
        AgentHarnessEvent::new(AgentHarnessEventDraft {
            occurred_at: observation.occurred_at(),
            source: observation.source().clone(),
            subject: observation.subject().clone(),
            source_event_id,
            harness,
            session_id,
            event_type,
            redaction,
            excerpts,
            facets,
        })
        .map_err(|_| AgentHarnessObservationError::InvalidPersistedPayload)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedHarnessRef {
    name: String,
    version: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRedaction {
    policy: String,
    version: String,
    transformations: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedExcerpt {
    kind: String,
    text: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFacet {
    name: String,
    value: FacetValue,
}

#[derive(Serialize)]
struct AgentHarnessSemanticEnvelope<'a> {
    schema: &'static str,
    occurred_at: DateTime<Utc>,
    source: &'a SourceRef,
    subject: &'a SubjectRef,
    persisted_payload: &'a PersistedAgentHarnessPayload,
}

/// Hashes the canonical JSON encoding of the versioned event envelope as lowercase SHA-256.
///
/// # Errors
///
/// Returns [`AgentHarnessObservationError::PayloadSerialization`] if the semantic envelope cannot
/// be encoded as JSON.
fn semantic_digest(
    occurred_at: DateTime<Utc>,
    source: &SourceRef,
    subject: &SubjectRef,
    payload: &PersistedAgentHarnessPayload,
) -> Result<String, AgentHarnessObservationError> {
    let envelope = AgentHarnessSemanticEnvelope {
        schema: HARNESS_DIGEST_SCHEMA,
        occurred_at,
        source,
        subject,
        persisted_payload: payload,
    };
    let serialized = serde_json::to_vec(&envelope)
        .map_err(|_| AgentHarnessObservationError::PayloadSerialization)?;
    Ok(format!("{:x}", Sha256::digest(serialized)))
}

/// Returns whether `digest` is exactly 64 lowercase hexadecimal ASCII characters.
fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Preserves an identity component exactly unless it contains only whitespace.
///
/// # Errors
///
/// Returns [`AgentHarnessObservationError::BlankIdentityField`] with the supplied field name when
/// `value` is blank.
fn validated_identity(
    field: &'static str,
    value: String,
) -> Result<String, AgentHarnessObservationError> {
    if value.trim().is_empty() {
        return Err(AgentHarnessObservationError::BlankIdentityField { field });
    }
    Ok(value)
}

impl_redacted_debug!(
    SourceEventId,
    HarnessRef,
    HarnessSessionId,
    HarnessEventType,
    FacetValue,
    ObservationFacet,
    RedactionRecord,
    RedactedExcerpt,
    AgentHarnessEventDraft,
    AgentHarnessEvent,
);

/// Error returned when agent-harness evidence violates the normalized contract.
#[derive(Debug, Error, Diagnostic)]
pub enum AgentHarnessEventError {
    /// A required text field was blank.
    #[error("{field} must not be blank")]
    #[diagnostic(code(evidra::agent_harness_event::blank_field))]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// The source category was not `agent-harness`.
    #[error("agent harness source kind must be agent-harness")]
    #[diagnostic(code(evidra::agent_harness_event::unexpected_source_kind))]
    UnexpectedSourceKind,

    /// An event included more excerpts than the contract permits.
    #[error("event has {count} excerpts; maximum is {maximum}")]
    #[diagnostic(code(evidra::agent_harness_event::too_many_excerpts))]
    TooManyExcerpts {
        /// Number of excerpts supplied.
        count: usize,
        /// Maximum permitted excerpts.
        maximum: usize,
    },

    /// A redacted excerpt exceeded the contract's byte bound.
    #[error("excerpt has {bytes} bytes; maximum is {maximum}")]
    #[diagnostic(code(evidra::agent_harness_event::excerpt_too_large))]
    ExcerptTooLarge {
        /// UTF-8 bytes supplied.
        bytes: usize,
        /// Maximum permitted UTF-8 bytes.
        maximum: usize,
    },

    /// An event included more facets than the contract permits.
    #[error("event has {count} facets; maximum is {maximum}")]
    #[diagnostic(code(evidra::agent_harness_event::too_many_facets))]
    TooManyFacets {
        /// Number of facets supplied.
        count: usize,
        /// Maximum permitted facets.
        maximum: usize,
    },
}

/// Preserves a harness-event field exactly unless it contains only whitespace.
///
/// # Errors
///
/// Returns [`AgentHarnessEventError::BlankField`] with the supplied field name when `value` is
/// blank.
fn validated(field: &'static str, value: String) -> Result<String, AgentHarnessEventError> {
    if value.trim().is_empty() {
        return Err(AgentHarnessEventError::BlankField { field });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use crate::{SourceRef, SubjectRef};

    use super::{
        AgentHarnessEvent, AgentHarnessEventDraft, AgentHarnessEventError,
        AgentHarnessEventIdentity, AgentHarnessObservation, AgentHarnessObservationError,
        FacetValue, HarnessEventType, HarnessRef, HarnessSessionId, MAX_EXCERPT_BYTES,
        MAX_EXCERPTS, MAX_FACETS, ObservationFacet, RedactedExcerpt, RedactionRecord,
        SourceEventId,
    };

    /// Builds a representative valid draft shared by event and persistence tests.
    fn valid_event_draft() -> AgentHarnessEventDraft {
        AgentHarnessEventDraft {
            occurred_at: "2026-09-19T08:00:00Z"
                .parse()
                .expect("timestamp should be valid"),
            source: SourceRef::new("agent-harness", "session.jsonl#event-17")
                .expect("source should be valid"),
            subject: SubjectRef::new("repository", "/workspace/evidra")
                .expect("subject should be valid"),
            source_event_id: SourceEventId::new("event-17")
                .expect("source event ID should be valid"),
            harness: HarnessRef::new("claude-code", Some("1.0".to_owned()))
                .expect("harness should be valid"),
            session_id: HarnessSessionId::new("session-1").expect("session ID should be valid"),
            event_type: HarnessEventType::new("tool-completed")
                .expect("event type should be valid"),
            redaction: RedactionRecord::new("obfsck", "1", vec!["secret-redaction".to_owned()])
                .expect("redaction should be valid"),
            excerpts: vec![
                RedactedExcerpt::new("tool-output", "tests passed")
                    .expect("excerpt should be valid"),
            ],
            facets: vec![
                ObservationFacet::new("verification.result", FacetValue::Text("passed".to_owned()))
                    .expect("facet should be valid"),
            ],
        }
    }

    /// Confirms each facet variant uses its corresponding untagged JSON scalar representation.
    #[test]
    fn facet_values_serialize_as_json_scalars() {
        assert_eq!(
            serde_json::to_value(FacetValue::Text("codex".to_owned()))
                .expect("text facet should serialize"),
            json!("codex")
        );
        assert_eq!(
            serde_json::to_value(FacetValue::Integer(7)).expect("integer facet should serialize"),
            json!(7)
        );
        assert_eq!(
            serde_json::to_value(FacetValue::Boolean(true))
                .expect("boolean facet should serialize"),
            json!(true)
        );
    }

    /// Confirms every validated harness value object rejects whitespace-only required fields.
    #[test]
    fn harness_value_objects_reject_blank_fields() {
        assert!(SourceEventId::new(" ").is_err());
        assert!(HarnessRef::new("", None).is_err());
        assert!(HarnessRef::new("claude-code", Some(" ".to_owned())).is_err());
        assert!(HarnessSessionId::new("\t").is_err());
        assert!(HarnessEventType::new("\n").is_err());
        assert!(ObservationFacet::new("", FacetValue::Boolean(true)).is_err());
        assert!(RedactedExcerpt::new("tool-output", "").is_err());
        assert!(RedactionRecord::new("obfsck", "1", vec![" ".to_owned()]).is_err());
    }

    /// Confirms event validation preserves every producer-reported field exposed by accessors.
    #[test]
    fn agent_harness_event_preserves_reported_evidence() {
        let draft = valid_event_draft();
        let occurred_at = draft.occurred_at;
        let event = AgentHarnessEvent::new(draft).expect("event should be valid");

        assert_eq!(event.occurred_at(), occurred_at);
        assert_eq!(event.source().kind(), "agent-harness");
        assert_eq!(event.subject().kind(), "repository");
        assert_eq!(event.source_event_id().as_str(), "event-17");
        assert_eq!(event.harness().name(), "claude-code");
        assert_eq!(event.harness().version(), Some("1.0"));
        assert_eq!(event.session_id().as_str(), "session-1");
        assert_eq!(event.event_type().as_str(), "tool-completed");
        assert_eq!(event.redaction().policy(), "obfsck");
        assert_eq!(event.excerpts()[0].text(), "tests passed");
        assert_eq!(event.facets()[0].name(), "verification.result");
    }

    /// Confirms the event contract rejects evidence attributed to a non-harness source kind.
    #[test]
    fn agent_harness_event_rejects_non_harness_source() {
        let mut draft = valid_event_draft();
        draft.source = SourceRef::new("manual", "cli").expect("source should be valid");

        let error = AgentHarnessEvent::new(draft).expect_err("manual source should be rejected");

        assert!(matches!(
            error,
            AgentHarnessEventError::UnexpectedSourceKind
        ));
    }

    /// Confirms count and byte limits reject oversized drafts while accepting exact boundaries.
    #[test]
    fn agent_harness_event_enforces_excerpt_and_facet_bounds() {
        let excerpt =
            RedactedExcerpt::new("tool-output", "redacted").expect("excerpt should be valid");
        let facet = ObservationFacet::new("tool.name", FacetValue::Text("bash".to_owned()))
            .expect("facet should be valid");

        let mut too_many_excerpts = valid_event_draft();
        too_many_excerpts.excerpts = vec![excerpt.clone(); MAX_EXCERPTS + 1];
        assert!(matches!(
            AgentHarnessEvent::new(too_many_excerpts),
            Err(AgentHarnessEventError::TooManyExcerpts {
                count,
                maximum
            }) if count == MAX_EXCERPTS + 1 && maximum == MAX_EXCERPTS
        ));

        let mut oversized_excerpt = valid_event_draft();
        oversized_excerpt.excerpts = vec![
            RedactedExcerpt::new("tool-output", "x".repeat(MAX_EXCERPT_BYTES + 1))
                .expect("excerpt construction should defer size validation"),
        ];
        assert!(matches!(
            AgentHarnessEvent::new(oversized_excerpt),
            Err(AgentHarnessEventError::ExcerptTooLarge { bytes, maximum })
                if bytes == MAX_EXCERPT_BYTES + 1 && maximum == MAX_EXCERPT_BYTES
        ));

        let mut too_many_facets = valid_event_draft();
        too_many_facets.facets = vec![facet.clone(); MAX_FACETS + 1];
        assert!(matches!(
            AgentHarnessEvent::new(too_many_facets),
            Err(AgentHarnessEventError::TooManyFacets {
                count,
                maximum
            }) if count == MAX_FACETS + 1 && maximum == MAX_FACETS
        ));

        let mut exact_bounds = valid_event_draft();
        exact_bounds.excerpts = vec![excerpt; MAX_EXCERPTS];
        exact_bounds.excerpts[0] =
            RedactedExcerpt::new("tool-output", "x".repeat(MAX_EXCERPT_BYTES))
                .expect("exactly bounded excerpt should construct");
        exact_bounds.facets = vec![facet; MAX_FACETS];
        assert!(AgentHarnessEvent::new(exact_bounds).is_ok());
    }

    /// Confirms direct event serialization preserves the normalized public JSON field layout.
    #[test]
    fn agent_harness_event_serializes_as_normalized_body() {
        let event = AgentHarnessEvent::new(valid_event_draft()).expect("event should be valid");
        let encoded = serde_json::to_value(event).expect("event should serialize");

        assert_eq!(
            encoded,
            json!({
                "occurred_at": "2026-09-19T08:00:00Z",
                "source": {
                    "kind": "agent-harness",
                    "locator": "session.jsonl#event-17"
                },
                "subject": {
                    "kind": "repository",
                    "identifier": "/workspace/evidra"
                },
                "source_event_id": "event-17",
                "harness": {
                    "name": "claude-code",
                    "version": "1.0"
                },
                "session_id": "session-1",
                "event_type": "tool-completed",
                "redaction": {
                    "policy": "obfsck",
                    "version": "1",
                    "transformations": ["secret-redaction"]
                },
                "excerpts": [{
                    "kind": "tool-output",
                    "text": "tests passed"
                }],
                "facets": [{
                    "name": "verification.result",
                    "value": "passed"
                }]
            })
        );
    }

    /// Confirms debug output for events and nested value objects omits all evidence-bearing values.
    #[test]
    fn harness_debug_redacts_source_values() {
        let draft = AgentHarnessEventDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("agent-harness", "secret-locator")
                .expect("source should be valid"),
            subject: SubjectRef::new("repository", "secret-subject")
                .expect("subject should be valid"),
            source_event_id: SourceEventId::new("secret-event-id")
                .expect("source event ID should be valid"),
            harness: HarnessRef::new("secret-harness", Some("secret-version".to_owned()))
                .expect("harness should be valid"),
            session_id: HarnessSessionId::new("secret-session").expect("session should be valid"),
            event_type: HarnessEventType::new("secret-event-type")
                .expect("event type should be valid"),
            redaction: RedactionRecord::new(
                "secret-policy",
                "secret-policy-version",
                vec!["secret-transformation".to_owned()],
            )
            .expect("redaction should be valid"),
            excerpts: vec![
                RedactedExcerpt::new("secret-kind", "secret-excerpt")
                    .expect("excerpt should be valid"),
            ],
            facets: vec![
                ObservationFacet::new(
                    "secret-facet-name",
                    FacetValue::Text("secret-facet-value".to_owned()),
                )
                .expect("facet should be valid"),
            ],
        };
        let draft_debug = format!("{draft:?}");
        let event = AgentHarnessEvent::new(draft).expect("event should be valid");
        let event_debug = format!("{event:?}");
        let nested_debug = format!(
            "{:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
            event.source(),
            event.subject(),
            event.source_event_id(),
            event.harness(),
            event.session_id(),
            event.event_type(),
            event.redaction(),
            event.excerpts()[0],
            event.facets()[0],
            event.facets()[0].value()
        );
        let rendered = format!("{draft_debug} {event_debug} {nested_debug}");

        for secret in [
            "secret-locator",
            "secret-subject",
            "secret-event-id",
            "secret-harness",
            "secret-version",
            "secret-session",
            "secret-event-type",
            "secret-policy",
            "secret-policy-version",
            "secret-transformation",
            "secret-kind",
            "secret-excerpt",
            "secret-facet-name",
            "secret-facet-value",
        ] {
            assert!(!rendered.contains(secret), "debug output leaked {secret}");
        }
    }

    /// Confirms observation recording emits the versioned payload and retains an omitted harness
    /// version as JSON `null`.
    #[test]
    fn harness_observation_records_exact_payload_with_null_version() {
        let mut draft = valid_event_draft();
        draft.harness = HarnessRef::new("claude-code", None).expect("harness should be valid");
        let value = AgentHarnessObservation::record(
            AgentHarnessEvent::new(draft).expect("event should be valid"),
            "evidra-cli",
        )
        .expect("harness observation should record");

        assert_eq!(
            value.observation().payload(),
            &json!({
                "schema": "evidra.agent-harness-observation/v1",
                "source_event_id": "event-17",
                "harness": { "name": "claude-code", "version": null },
                "session_id": "session-1",
                "event_type": "tool-completed",
                "redaction": {
                    "policy": "obfsck",
                    "version": "1",
                    "transformations": ["secret-redaction"]
                },
                "excerpts": [{ "kind": "tool-output", "text": "tests passed" }],
                "facets": [{ "name": "verification.result", "value": "passed" }]
            })
        );
        assert_eq!(
            value.observation().provenance().transformations(),
            ["agent-harness-observation/v1"]
        );
    }

    /// Confirms equivalent events produce the same fixed lowercase SHA-256 semantic digest.
    #[test]
    fn semantic_digest_is_stable_for_equal_events() {
        let event = AgentHarnessEvent::new(valid_event_draft()).expect("event should be valid");
        let first = AgentHarnessObservation::record(event.clone(), "evidra-cli")
            .expect("first observation should record");
        let second = AgentHarnessObservation::record(event, "evidra-cli")
            .expect("second observation should record");

        assert_eq!(first.event_digest(), second.event_digest());
        assert_eq!(first.event_digest().len(), 64);
        assert_eq!(
            first.event_digest(),
            "c4001a71df2014a4d5731e11858a9e40e3f1c7e1349d3e1c53929a228936f4e7"
        );
    }

    /// Confirms changing a persisted semantic field changes the event digest.
    #[test]
    fn semantic_digest_changes_with_event_content() {
        let first = AgentHarnessObservation::record(
            AgentHarnessEvent::new(valid_event_draft()).expect("event should be valid"),
            "evidra-cli",
        )
        .expect("first observation should record");
        let mut changed_draft = valid_event_draft();
        changed_draft.facets = vec![
            ObservationFacet::new("verification.result", FacetValue::Text("failed".to_owned()))
                .expect("facet should be valid"),
        ];
        let second = AgentHarnessObservation::record(
            AgentHarnessEvent::new(changed_draft).expect("event should be valid"),
            "evidra-cli",
        )
        .expect("second observation should record");

        assert_ne!(first.event_digest(), second.event_digest());
    }

    /// Confirms receipt verification distinguishes mismatches from malformed digests and payloads.
    #[test]
    fn verify_receipt_checks_identity_digest_kind_and_payload() {
        let value = AgentHarnessObservation::record(
            AgentHarnessEvent::new(valid_event_draft()).expect("event should be valid"),
            "evidra-cli",
        )
        .expect("observation should record");
        assert!(
            AgentHarnessObservation::verify_receipt(
                value.identity(),
                value.event_digest(),
                value.observation()
            )
            .expect("receipt should verify")
        );

        let wrong_identity = AgentHarnessEventIdentity::new("codex", "session-1", "event-17")
            .expect("identity should be valid");
        assert!(
            !AgentHarnessObservation::verify_receipt(
                &wrong_identity,
                value.event_digest(),
                value.observation()
            )
            .expect("mismatched identity should be a valid comparison")
        );
        assert!(matches!(
            AgentHarnessObservation::verify_receipt(
                value.identity(),
                "not-a-digest",
                value.observation()
            ),
            Err(AgentHarnessObservationError::InvalidEventDigest)
        ));

        let manual = crate::Observation::record(crate::ObservationDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("manual", "cli").expect("source should be valid"),
            kind: crate::ObservationKind::ManualIntervention,
            subject: SubjectRef::new("repository", "/tmp/example")
                .expect("subject should be valid"),
            payload: json!({"summary": "manual"}),
            provenance: crate::Provenance::direct("evidra-cli")
                .expect("provenance should be valid"),
        })
        .expect("manual observation should record");
        assert!(matches!(
            AgentHarnessObservation::verify_receipt(
                value.identity(),
                value.event_digest(),
                &manual
            ),
            Err(AgentHarnessObservationError::InvalidPersistedPayload)
        ));
    }

    /// Confirms observation diagnostics and identity-validation errors do not disclose evidence.
    #[test]
    fn harness_observation_debug_and_errors_are_redacted() {
        let value = AgentHarnessObservation::record(
            AgentHarnessEvent::new(valid_event_draft()).expect("event should be valid"),
            "secret-collector",
        )
        .expect("observation should record");
        let rendered = format!("{value:?} {:?}", value.identity());

        for secret in ["event-17", "session-1", "claude-code", "tests passed"] {
            assert!(!rendered.contains(secret), "debug output leaked {secret}");
        }
        let error = AgentHarnessEventIdentity::new("", "secret-session", "secret-event")
            .expect_err("blank identity should fail");
        assert!(!error.to_string().contains("secret-session"));
        assert!(!error.to_string().contains("secret-event"));
    }
}

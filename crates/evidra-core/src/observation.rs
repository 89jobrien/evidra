//! Immutable observation envelopes with source attribution, provenance, and SHA-256 integrity
//! validation.

use std::fmt;

use chrono::{DateTime, Utc};
use miette::Diagnostic;
use serde::de::Error as DeserializeError;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use ulid::Ulid;

/// Uniquely identifies an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObservationId(Ulid);

impl ObservationId {
    /// Generates a fresh ULID-backed identity for a newly recorded observation.
    fn new() -> Self {
        Self(Ulid::new())
    }
}

impl fmt::Display for ObservationId {
    /// Writes the observation's canonical ULID representation.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Classifies the event represented by an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationKind {
    /// A human or automated operator performed an explicit repair or maintenance action.
    ManualIntervention,
    /// An engineering-agent harness reported a validated lifecycle event.
    AgentHarnessEvent,
}

impl fmt::Display for ObservationKind {
    /// Writes the stable kebab-case spelling used by serialized observations.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManualIntervention => formatter.write_str("manual-intervention"),
            Self::AgentHarnessEvent => formatter.write_str("agent-harness-event"),
        }
    }
}

/// Identifies the system that produced an observation.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SourceRef {
    kind: String,
    locator: String,
}

impl fmt::Debug for SourceRef {
    /// Redacts the source kind and locator from debug output.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SourceRef").finish_non_exhaustive()
    }
}

impl<'de> Deserialize<'de> for SourceRef {
    /// Deserializes a source reference while rejecting unknown or blank fields.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawSourceRef {
            kind: String,
            locator: String,
        }

        let raw = RawSourceRef::deserialize(deserializer)?;
        Self::new(raw.kind, raw.locator).map_err(D::Error::custom)
    }
}

impl SourceRef {
    /// Creates a source reference with non-blank kind and locator values.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::BlankField`] when either value is blank.
    pub fn new(
        kind: impl Into<String>,
        locator: impl Into<String>,
    ) -> Result<Self, ObservationError> {
        Ok(Self {
            kind: validated("source.kind", kind.into())?,
            locator: validated("source.locator", locator.into())?,
        })
    }

    /// Returns the source category.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the source-specific location.
    #[must_use]
    pub fn locator(&self) -> &str {
        &self.locator
    }
}

/// Identifies the entity affected by an observation.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SubjectRef {
    kind: String,
    identifier: String,
}

impl fmt::Debug for SubjectRef {
    /// Redacts the subject kind and identifier from debug output.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SubjectRef").finish_non_exhaustive()
    }
}

impl<'de> Deserialize<'de> for SubjectRef {
    /// Deserializes a subject reference while rejecting unknown or blank fields.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawSubjectRef {
            kind: String,
            identifier: String,
        }

        let raw = RawSubjectRef::deserialize(deserializer)?;
        Self::new(raw.kind, raw.identifier).map_err(D::Error::custom)
    }
}

impl SubjectRef {
    /// Creates a subject reference with non-blank kind and identifier values.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::BlankField`] when either value is blank.
    pub fn new(
        kind: impl Into<String>,
        identifier: impl Into<String>,
    ) -> Result<Self, ObservationError> {
        Ok(Self {
            kind: validated("subject.kind", kind.into())?,
            identifier: validated("subject.identifier", identifier.into())?,
        })
    }

    /// Returns the subject category.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the subject identifier.
    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }
}

/// Describes how an observation entered the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Provenance {
    collector: String,
    transformations: Vec<String>,
}

impl<'de> Deserialize<'de> for Provenance {
    /// Deserializes provenance while rejecting unknown fields and blank entries.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawProvenance {
            collector: String,
            transformations: Vec<String>,
        }

        let raw = RawProvenance::deserialize(deserializer)?;
        let collector =
            validated("provenance.collector", raw.collector).map_err(D::Error::custom)?;
        if raw
            .transformations
            .iter()
            .any(|transformation| transformation.trim().is_empty())
        {
            return Err(D::Error::custom(ObservationError::BlankField {
                field: "provenance.transformations",
            }));
        }
        Ok(Self {
            collector,
            transformations: raw.transformations,
        })
    }
}

impl Provenance {
    /// Creates provenance for evidence captured directly without transformations.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::BlankField`] when `collector` is blank.
    pub fn direct(collector: impl Into<String>) -> Result<Self, ObservationError> {
        Self::transformed(collector, Vec::new())
    }

    /// Creates provenance for evidence transformed before storage.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::BlankField`] when the collector or a transformation is blank.
    pub fn transformed(
        collector: impl Into<String>,
        transformations: Vec<String>,
    ) -> Result<Self, ObservationError> {
        if transformations
            .iter()
            .any(|transformation| transformation.trim().is_empty())
        {
            return Err(ObservationError::BlankField {
                field: "provenance.transformations",
            });
        }
        Ok(Self {
            collector: validated("provenance.collector", collector.into())?,
            transformations,
        })
    }

    /// Returns the collector identity.
    #[must_use]
    pub fn collector(&self) -> &str {
        &self.collector
    }

    /// Returns transformations applied before the observation was stored.
    #[must_use]
    pub fn transformations(&self) -> &[String] {
        &self.transformations
    }
}

/// Records the digest protecting an observation envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrityRecord {
    algorithm: String,
    digest: String,
}

impl<'de> Deserialize<'de> for IntegrityRecord {
    /// Deserializes validated SHA-256 metadata with a lowercase hexadecimal digest.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawIntegrityRecord {
            algorithm: String,
            digest: String,
        }

        let raw = RawIntegrityRecord::deserialize(deserializer)?;
        if raw.algorithm != "sha256" {
            return Err(D::Error::custom(
                "unsupported observation integrity algorithm",
            ));
        }
        if raw.digest.len() != 64
            || !raw
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(D::Error::custom("invalid lowercase SHA-256 digest"));
        }
        Ok(Self {
            algorithm: raw.algorithm,
            digest: raw.digest,
        })
    }
}

impl IntegrityRecord {
    /// Returns the digest algorithm name.
    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    /// Returns the lowercase hexadecimal digest.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Contains source-provided fields needed to record an observation.
#[derive(Clone, PartialEq)]
pub struct ObservationDraft {
    /// Time at which the underlying event occurred.
    pub occurred_at: DateTime<Utc>,
    /// System that supplied the event.
    pub source: SourceRef,
    /// Event classification.
    pub kind: ObservationKind,
    /// Entity affected by the event.
    pub subject: SubjectRef,
    /// Source-specific structured data.
    pub payload: serde_json::Value,
    /// Collection and transformation history.
    pub provenance: Provenance,
}

impl fmt::Debug for ObservationDraft {
    /// Omits all draft fields from debug output to avoid exposing evidence.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservationDraft")
            .finish_non_exhaustive()
    }
}

/// Immutable, source-attributed record of an event or measurement.
#[derive(Clone, PartialEq, Serialize)]
pub struct Observation {
    id: ObservationId,
    occurred_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
    source: SourceRef,
    kind: ObservationKind,
    subject: SubjectRef,
    payload: serde_json::Value,
    provenance: Provenance,
    integrity: IntegrityRecord,
}

impl fmt::Debug for Observation {
    /// Omits all envelope fields from debug output to avoid exposing evidence.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Observation")
            .finish_non_exhaustive()
    }
}

impl<'de> Deserialize<'de> for Observation {
    /// Deserializes an observation only when its schema and integrity digest are valid.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawObservation {
            id: ObservationId,
            occurred_at: DateTime<Utc>,
            observed_at: DateTime<Utc>,
            source: SourceRef,
            kind: ObservationKind,
            subject: SubjectRef,
            payload: serde_json::Value,
            provenance: Provenance,
            integrity: IntegrityRecord,
        }

        let raw = RawObservation::deserialize(deserializer)?;
        let observation = Self {
            id: raw.id,
            occurred_at: raw.occurred_at,
            observed_at: raw.observed_at,
            source: raw.source,
            kind: raw.kind,
            subject: raw.subject,
            payload: raw.payload,
            provenance: raw.provenance,
            integrity: raw.integrity,
        };
        if !observation.verify_integrity().map_err(D::Error::custom)? {
            return Err(D::Error::custom("observation integrity mismatch"));
        }
        Ok(observation)
    }
}

impl Observation {
    /// Records a draft as an immutable observation with identity and integrity metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::IntegritySerialization`] if the integrity envelope cannot be
    /// encoded.
    pub fn record(draft: ObservationDraft) -> Result<Self, ObservationError> {
        let id = ObservationId::new();
        let observed_at = Utc::now();
        let integrity = integrity_for(&id, observed_at, &draft)?;

        Ok(Self {
            id,
            occurred_at: draft.occurred_at,
            observed_at,
            source: draft.source,
            kind: draft.kind,
            subject: draft.subject,
            payload: draft.payload,
            provenance: draft.provenance,
            integrity,
        })
    }

    /// Returns the observation identity.
    #[must_use]
    pub fn id(&self) -> &ObservationId {
        &self.id
    }

    /// Returns when the underlying event occurred.
    #[must_use]
    pub fn occurred_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }

    /// Returns when Evidra observed the event.
    #[must_use]
    pub fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }

    /// Returns the event source.
    #[must_use]
    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns the event classification.
    #[must_use]
    pub fn kind(&self) -> &ObservationKind {
        &self.kind
    }

    /// Returns the affected subject.
    #[must_use]
    pub fn subject(&self) -> &SubjectRef {
        &self.subject
    }

    /// Returns the source-specific payload.
    #[must_use]
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }

    /// Returns collection provenance.
    #[must_use]
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Returns observation integrity metadata.
    #[must_use]
    pub fn integrity(&self) -> &IntegrityRecord {
        &self.integrity
    }

    /// Verifies that the stored digest still matches the observation envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::IntegritySerialization`] if the envelope cannot be encoded.
    pub fn verify_integrity(&self) -> Result<bool, ObservationError> {
        if self.integrity.algorithm != "sha256" {
            return Ok(false);
        }

        let envelope = IntegrityEnvelope {
            id: &self.id,
            occurred_at: self.occurred_at,
            observed_at: self.observed_at,
            source: &self.source,
            kind: self.kind,
            subject: &self.subject,
            payload: &self.payload,
            provenance: &self.provenance,
        };
        Ok(self.integrity.digest == digest_for(&envelope)?)
    }
}

/// Error returned when observation data cannot satisfy domain requirements.
#[derive(Debug, Clone, PartialEq, Eq, Error, Diagnostic)]
pub enum ObservationError {
    /// A required text field was blank.
    #[error("{field} must not be blank")]
    #[diagnostic(code(evidra::observation::blank_field))]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// The integrity envelope could not be serialized.
    #[error("failed to serialize observation integrity envelope")]
    #[diagnostic(code(evidra::observation::integrity_serialization))]
    IntegritySerialization,
}

#[derive(Serialize)]
struct IntegrityEnvelope<'a> {
    id: &'a ObservationId,
    occurred_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
    source: &'a SourceRef,
    kind: ObservationKind,
    subject: &'a SubjectRef,
    payload: &'a serde_json::Value,
    provenance: &'a Provenance,
}

/// Builds SHA-256 integrity metadata for a draft and its assigned ledger metadata.
fn integrity_for(
    id: &ObservationId,
    observed_at: DateTime<Utc>,
    draft: &ObservationDraft,
) -> Result<IntegrityRecord, ObservationError> {
    let envelope = IntegrityEnvelope {
        id,
        occurred_at: draft.occurred_at,
        observed_at,
        source: &draft.source,
        kind: draft.kind,
        subject: &draft.subject,
        payload: &draft.payload,
        provenance: &draft.provenance,
    };

    Ok(IntegrityRecord {
        algorithm: "sha256".to_owned(),
        digest: digest_for(&envelope)?,
    })
}

/// Serializes an integrity envelope and returns its lowercase SHA-256 digest.
fn digest_for(envelope: &IntegrityEnvelope<'_>) -> Result<String, ObservationError> {
    let serialized =
        serde_json::to_vec(envelope).map_err(|_| ObservationError::IntegritySerialization)?;
    let digest = Sha256::digest(serialized);
    Ok(format!("{digest:x}"))
}

/// Preserves a text field after confirming that it contains non-whitespace content.
fn validated(field: &'static str, value: String) -> Result<String, ObservationError> {
    if value.trim().is_empty() {
        return Err(ObservationError::BlankField { field });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use chrono::Utc;
    use serde_json::json;

    use super::{
        Observation, ObservationDraft, ObservationKind, Provenance, SourceRef, SubjectRef,
    };

    /// Confirms recording assigns identity, time, and SHA-256 integrity metadata.
    #[test]
    fn record_creates_verifiable_observation() {
        let before = Utc::now();
        let draft = ObservationDraft {
            occurred_at: before,
            source: SourceRef::new("manual", "cli").expect("source should be valid"),
            kind: ObservationKind::ManualIntervention,
            subject: SubjectRef::new("repository", "/tmp/example")
                .expect("subject should be valid"),
            payload: json!({"summary": "Removed stale generated artifacts"}),
            provenance: Provenance::direct("evidra-cli").expect("provenance should be valid"),
        };

        let observation = Observation::record(draft).expect("observation should be recorded");

        assert_eq!(observation.id().to_string().len(), 26);
        assert!(observation.observed_at() >= before);
        assert_eq!(observation.integrity().algorithm(), "sha256");
        assert_eq!(observation.integrity().digest().len(), 64);
    }

    /// Confirms the harness event kind has stable display and serialization spellings.
    #[test]
    fn harness_observation_kind_has_stable_spelling() {
        assert_eq!(
            ObservationKind::AgentHarnessEvent.to_string(),
            "agent-harness-event"
        );
        assert_eq!(
            serde_json::to_value(ObservationKind::AgentHarnessEvent)
                .expect("kind should serialize"),
            json!("agent-harness-event")
        );
    }

    /// Confirms transformed provenance accepts populated fields and rejects blank entries.
    #[test]
    fn transformed_provenance_validates_every_field() {
        let provenance = Provenance::transformed(
            "evidra-cli",
            vec!["agent-harness-observation/v1".to_owned()],
        )
        .expect("provenance should be valid");

        assert_eq!(provenance.collector(), "evidra-cli");
        assert_eq!(
            provenance.transformations(),
            ["agent-harness-observation/v1"]
        );
        assert!(Provenance::transformed(" ", Vec::new()).is_err());
        assert!(Provenance::transformed("evidra-cli", vec![" ".to_owned()]).is_err());
    }

    /// Confirms draft and observation debug output does not reveal evidence fields.
    #[test]
    fn observation_debug_omits_payload_and_provenance() {
        let draft = ObservationDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("manual", "secret-source").expect("source should be valid"),
            kind: ObservationKind::ManualIntervention,
            subject: SubjectRef::new("repository", "secret-subject")
                .expect("subject should be valid"),
            payload: json!({"summary": "secret-payload"}),
            provenance: Provenance::direct("secret-collector").expect("provenance should be valid"),
        };
        let draft_debug = format!("{draft:?}");
        let observation = Observation::record(draft).expect("observation should be recorded");
        let rendered = format!("{draft_debug} {observation:?}");

        for secret in [
            "secret-source",
            "secret-subject",
            "secret-payload",
            "secret-collector",
        ] {
            assert!(!rendered.contains(secret), "debug output leaked {secret}");
        }
    }

    /// Confirms the integrity serialization error has stable text and no underlying source.
    #[test]
    fn integrity_serialization_error_has_no_source() {
        let error = super::ObservationError::IntegritySerialization;

        assert!(error.source().is_none());
        assert_eq!(
            error.to_string(),
            "failed to serialize observation integrity envelope"
        );
    }

    /// Confirms source and subject references reject blank required values.
    #[test]
    fn source_and_subject_reject_blank_values() {
        assert!(SourceRef::new("", "cli").is_err());
        assert!(SourceRef::new("manual", " ").is_err());
        assert!(SubjectRef::new("", "/tmp/example").is_err());
        assert!(SubjectRef::new("repository", " ").is_err());
        assert!(
            serde_json::from_value::<SourceRef>(json!({"kind": "", "locator": "cli"})).is_err()
        );
    }

    /// Confirms source and subject debug output redacts their identifying values.
    #[test]
    fn source_and_subject_debug_redacts_values() {
        let source = SourceRef::new("secret-source-kind", "secret-source-locator")
            .expect("source should be valid");
        let subject = SubjectRef::new("secret-subject-kind", "secret-subject-identifier")
            .expect("subject should be valid");
        let rendered = format!("{source:?} {subject:?}");

        for secret in [
            "secret-source-kind",
            "secret-source-locator",
            "secret-subject-kind",
            "secret-subject-identifier",
        ] {
            assert!(!rendered.contains(secret), "debug output leaked {secret}");
        }
    }

    /// Confirms deserialization rejects an observation whose payload was altered.
    #[test]
    fn tampered_observation_fails_integrity_verification() {
        let observation = Observation::record(ObservationDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("manual", "cli").expect("source should be valid"),
            kind: ObservationKind::ManualIntervention,
            subject: SubjectRef::new("repository", "/tmp/example")
                .expect("subject should be valid"),
            payload: json!({"summary": "Original summary"}),
            provenance: Provenance::direct("evidra-cli").expect("provenance should be valid"),
        })
        .expect("observation should be recorded");
        let mut encoded = serde_json::to_value(observation).expect("observation should serialize");
        encoded["payload"]["summary"] = json!("Tampered summary");
        let error = serde_json::from_value::<Observation>(encoded)
            .expect_err("tampered observation should be rejected");

        assert!(error.to_string().contains("integrity mismatch"));
    }

    /// Confirms deserialization rejects fields outside the observation schema.
    #[test]
    fn observation_deserialization_rejects_unknown_fields() {
        let observation = Observation::record(ObservationDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("manual", "cli").expect("source should be valid"),
            kind: ObservationKind::ManualIntervention,
            subject: SubjectRef::new("repository", "/tmp/example")
                .expect("subject should be valid"),
            payload: json!({"summary": "Original summary"}),
            provenance: Provenance::direct("evidra-cli").expect("provenance should be valid"),
        })
        .expect("observation should be recorded");
        let mut encoded = serde_json::to_value(observation).expect("observation should serialize");
        encoded["unsigned"] = json!("unexpected");

        assert!(serde_json::from_value::<Observation>(encoded).is_err());
    }
}

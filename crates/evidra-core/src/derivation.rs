//! Derived records: banded facets, typed relationships, and explicit uncertainty.
//!
//! This module owns the revisable layer that sits above the observation ledger. Derived records never
//! mutate: a correction appends a new record plus a [`RelationKind::Supersedes`] relationship, and
//! the current view is resolved by walking that chain (ADR-008).
//!
//! Two rules shape everything here. Numeric facets are banded rather than raw, because a raw
//! millisecond cannot answer whether a failure is fast or slow (ADR-009). And uncertainty is a
//! stored profile rather than a score, because published attribution accuracy does not support a
//! continuous one (ADR-011).
//!
//! `recorded_at` is supplied by the caller rather than read from the clock, so that identical inputs
//! produce an identical record and determinism is testable rather than aspirational.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use ulid::Ulid;

use crate::harness::FacetValue;
use crate::observation::{ObservationId, SubjectRef};

/// Identity of a derived record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DerivationId(Ulid);

impl DerivationId {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    /// Parses a previously persisted identity.
    ///
    /// # Errors
    ///
    /// Returns [`DerivationError::BlankField`] when `raw` is blank and
    /// [`DerivationError::InvalidIdentity`] when it is not a ULID.
    pub fn parse(raw: &str) -> Result<Self, DerivationError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DerivationError::BlankField {
                field: "derivation id",
            });
        }
        Ulid::from_string(trimmed)
            .map(Self)
            .map_err(|_| DerivationError::InvalidIdentity)
    }

    /// Mints a new identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for DerivationId {
    fn default() -> Self {
        Self::new()
    }
}

/// Identity of a relationship between two derived records or observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelationshipId(Ulid);

impl RelationshipId {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    /// Mints a new identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for RelationshipId {
    fn default() -> Self {
        Self::new()
    }
}

/// Confidence that a derivation is correct.
///
/// Banded rather than continuous. Published step-level attribution accuracy sits near 14% for
/// LLM-judge methods and near 47% for the best framework, so a float would imply a precision the
/// method does not have (ADR-011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfidenceBand {
    /// Heuristic or unverified. Must not be cited as a finding.
    Speculative,
    /// A single deterministic pass with no corroboration.
    Weak,
    /// Corroborated by two or more independent evidence links.
    Moderate,
    /// Deterministically reproducible from the recorded evidence.
    Strong,
}

impl ConfidenceBand {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Speculative => "speculative",
            Self::Weak => "weak",
            Self::Moderate => "moderate",
            Self::Strong => "strong",
        }
    }

    /// Returns whether an assisted method may record this band.
    ///
    /// Assisted methods are capped at [`ConfidenceBand::Speculative`] and [`ConfidenceBand::Weak`].
    /// This is what keeps an unverified inference from being cited as a finding.
    #[must_use]
    pub fn permitted_for_assisted(&self) -> bool {
        matches!(self, Self::Speculative | Self::Weak)
    }
}

/// Whether the evidence behind a derivation has moved since it was computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Freshness {
    /// Derived within the configured recomputation window.
    Current,
    /// Derived, but the window has elapsed.
    Aging,
    /// The underlying evidence has changed since derivation.
    Stale,
}

impl Freshness {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Aging => "aging",
            Self::Stale => "stale",
        }
    }
}

/// Whether evidence against the derivation exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Contradiction {
    /// No refuting evidence link exists.
    Uncontested,
    /// Refuting evidence exists but has not been reconciled.
    Contested,
    /// Refuting evidence exists and has been reconciled against this derivation.
    Reconciled,
}

impl Contradiction {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uncontested => "uncontested",
            Self::Contested => "contested",
            Self::Reconciled => "reconciled",
        }
    }
}

/// How closely the actual evidence matched the declared scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeFidelity {
    /// Derived over exactly the declared window and selection.
    Exact,
    /// Derived over a superset of the declared selection.
    Broader,
    /// Derived over a subset of the declared selection.
    Narrower,
}

impl ScopeFidelity {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Broader => "broader",
            Self::Narrower => "narrower",
        }
    }
}

/// The five uncertainty dimensions ADR-004 requires as stored state.
///
/// Each dimension is independent so a consumer can filter on one without parsing a composite. A
/// derivation may be well-evidenced and stale, or fresh and contested, and the two are not
/// interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UncertaintyProfile {
    confidence: ConfidenceBand,
    freshness: Freshness,
    contradiction: Contradiction,
    scope: ScopeFidelity,
}

impl UncertaintyProfile {
    /// Creates the profile for a freshly derived, uncontested, exactly-scoped result.
    #[must_use]
    pub fn deterministic() -> Self {
        Self {
            confidence: ConfidenceBand::Strong,
            freshness: Freshness::Current,
            contradiction: Contradiction::Uncontested,
            scope: ScopeFidelity::Exact,
        }
    }

    /// Creates a profile for an assisted derivation, capped at [`ConfidenceBand::Weak`].
    #[must_use]
    pub fn assisted() -> Self {
        Self {
            confidence: ConfidenceBand::Weak,
            freshness: Freshness::Current,
            contradiction: Contradiction::Contested,
            scope: ScopeFidelity::Exact,
        }
    }

    /// Replaces the confidence band.
    #[must_use]
    pub fn with_confidence(mut self, confidence: ConfidenceBand) -> Self {
        self.confidence = confidence;
        self
    }

    /// Replaces the freshness dimension.
    #[must_use]
    pub fn with_freshness(mut self, freshness: Freshness) -> Self {
        self.freshness = freshness;
        self
    }

    /// Replaces the contradiction dimension.
    #[must_use]
    pub fn with_contradiction(mut self, contradiction: Contradiction) -> Self {
        self.contradiction = contradiction;
        self
    }

    /// Replaces the scope fidelity dimension.
    #[must_use]
    pub fn with_scope(mut self, scope: ScopeFidelity) -> Self {
        self.scope = scope;
        self
    }

    /// Returns the confidence band.
    #[must_use]
    pub fn confidence(&self) -> ConfidenceBand {
        self.confidence
    }

    /// Returns the freshness dimension.
    #[must_use]
    pub fn freshness(&self) -> Freshness {
        self.freshness
    }

    /// Returns the contradiction dimension.
    #[must_use]
    pub fn contradiction(&self) -> Contradiction {
        self.contradiction
    }

    /// Returns the scope fidelity dimension.
    #[must_use]
    pub fn scope(&self) -> ScopeFidelity {
        self.scope
    }
}

/// What produced a derivation, and therefore what it is allowed to conclude.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DerivationMethod {
    /// Reproducible from recorded evidence by versioned local code.
    Deterministic {
        /// Version of the producing code, so a derivation can be replayed.
        version: String,
    },
    /// Produced by a model. Never eligible for an evaluative disposition.
    Assisted {
        /// Identifier of the producing model.
        model: String,
        /// Version of the prompt that shaped the output.
        prompt_version: String,
    },
}

impl DerivationMethod {
    /// Returns whether this method may produce an evaluative disposition.
    ///
    /// Only deterministic code decides whether an action is allowed, denied, warned, quarantined, or
    /// escalated (ADR-005). An assisted method may cluster, propose, and summarise, nothing more.
    #[must_use]
    pub fn may_dispose(&self) -> bool {
        matches!(self, Self::Deterministic { .. })
    }

    /// Returns the stable persisted spelling of the method kind.
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Deterministic { .. } => "deterministic",
            Self::Assisted { .. } => "assisted",
        }
    }
}

/// A namespace and name pair selecting part of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetFilter {
    namespace: String,
    name: String,
    value: FacetValue,
}

impl FacetFilter {
    /// Creates a filter over one facet.
    ///
    /// # Errors
    ///
    /// Returns [`DerivationError::BlankField`] when `namespace` or `name` is blank.
    pub fn new(
        namespace: impl Into<String>,
        name: impl Into<String>,
        value: FacetValue,
    ) -> Result<Self, DerivationError> {
        let namespace = namespace.into();
        let name = name.into();
        if namespace.trim().is_empty() {
            return Err(DerivationError::BlankField {
                field: "facet namespace",
            });
        }
        if name.trim().is_empty() {
            return Err(DerivationError::BlankField {
                field: "facet name",
            });
        }
        Ok(Self {
            namespace,
            name,
            value,
        })
    }

    /// Returns the facet namespace.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the facet name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the selected value.
    #[must_use]
    pub fn value(&self) -> &FacetValue {
        &self.value
    }
}

/// The window and selection a derivation was computed over.
///
/// Stored rather than inferred, because a derivation that does not record its scope cannot be
/// recomputed or falsified, and [`ScopeFidelity`] would be unverifiable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivationScope {
    subject: SubjectRef,
    from_occurred_at: DateTime<Utc>,
    to_occurred_at: DateTime<Utc>,
    selection: Vec<FacetFilter>,
}

impl DerivationScope {
    /// Creates a scope over a closed window.
    ///
    /// # Errors
    ///
    /// Returns [`DerivationError::InvertedWindow`] when `from_occurred_at` is after `to_occurred_at`.
    pub fn new(
        subject: SubjectRef,
        from_occurred_at: DateTime<Utc>,
        to_occurred_at: DateTime<Utc>,
        selection: Vec<FacetFilter>,
    ) -> Result<Self, DerivationError> {
        if from_occurred_at > to_occurred_at {
            return Err(DerivationError::InvertedWindow);
        }
        Ok(Self {
            subject,
            from_occurred_at,
            to_occurred_at,
            selection,
        })
    }

    /// Returns the subject the derivation covers.
    #[must_use]
    pub fn subject(&self) -> &SubjectRef {
        &self.subject
    }

    /// Returns the inclusive start of the window.
    #[must_use]
    pub fn from_occurred_at(&self) -> DateTime<Utc> {
        self.from_occurred_at
    }

    /// Returns the inclusive end of the window.
    #[must_use]
    pub fn to_occurred_at(&self) -> DateTime<Utc> {
        self.to_occurred_at
    }

    /// Returns the filters that selected the evidence.
    #[must_use]
    pub fn selection(&self) -> &[FacetFilter] {
        &self.selection
    }

    /// Returns whether `occurred_at` falls inside this scope's window.
    #[must_use]
    pub fn contains(&self, occurred_at: DateTime<Utc>) -> bool {
        occurred_at >= self.from_occurred_at && occurred_at <= self.to_occurred_at
    }
}

/// Which indexed column holds a facet value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FacetValueSlot {
    /// Scalar text facet.
    Text,
    /// Banded integer facet.
    Integer,
    /// Boolean facet.
    Boolean,
}

impl FacetValueSlot {
    /// Returns the stable persisted spelling used in the `slot` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
        }
    }

    /// Returns the slot that stores `value`.
    #[must_use]
    pub fn of(value: &FacetValue) -> Self {
        match value {
            FacetValue::Text(_) => Self::Text,
            FacetValue::Integer(_) => Self::Integer,
            FacetValue::Boolean(_) => Self::Boolean,
        }
    }
}

/// What kind of thing a derivation computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivationKind {
    /// A banded projection of one observation into indexed facet columns.
    Facet,
    /// A count or distribution over a set of observations.
    Aggregate,
    /// A grouping produced by a clustering method.
    Cluster,
}

impl DerivationKind {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Facet => "facet",
            Self::Aggregate => "aggregate",
            Self::Cluster => "cluster",
        }
    }

    /// Returns whether this kind carries individual projected facets.
    #[must_use]
    pub fn carries_facets(&self) -> bool {
        matches!(self, Self::Facet)
    }
}

/// Whether evidence supports or counts against a derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceRole {
    /// Evidence that increases belief in the derivation.
    Supporting,
    /// Evidence that counts against it. Must be retained, never dropped (ADR-003).
    Refuting,
}

impl EvidenceRole {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Supporting => "supporting",
            Self::Refuting => "refuting",
        }
    }
}

/// What an evidence link or relationship points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceTarget {
    /// Points at an immutable observation.
    Observation(ObservationId),
    /// Points at another derivation, permitting chained inference.
    Derivation(DerivationId),
}

impl EvidenceTarget {
    /// Returns the stable persisted spelling of the target kind.
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Observation(_) => "observation",
            Self::Derivation(_) => "derivation",
        }
    }

    /// Returns the identity string used for indexed lookups.
    ///
    /// Renders the canonical ULID rather than the debug representation, because this value becomes
    /// an index key and must stay stable and compact across processes.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Observation(id) => format!("observation:{id}"),
            Self::Derivation(id) => format!("derivation:{}", id.as_str()),
        }
    }
}

/// How two records relate.
///
/// The set is closed so the graph stays queryable. [`RelationKind::Prevented`] and
/// [`RelationKind::NoEffect`] are load-bearing: a graph recording only what happened cannot answer
/// "what would have avoided this" or "what was already ruled out", which are the two questions that
/// make a failure actionable rather than merely explicable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelationKind {
    /// Increases confidence in the target.
    Supports,
    /// Counts against the target.
    Refutes,
    /// The source contributed causally to the target.
    CausedBy,
    /// The source made the target possible without causing it.
    Enabled,
    /// Acting on the source avoided the target. The inhibitory direction.
    Prevented,
    /// A newer derivation replaces an older one for current-view purposes.
    Supersedes,
    /// The source derivation was computed from the target.
    DerivesFrom,
    /// The two were observed together without an established causal direction.
    CoOccursWith,
    /// A hypothesis was tested and had no effect. Recording the null is evidence.
    NoEffect,
}

impl RelationKind {
    /// Returns the stable persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Refutes => "refutes",
            Self::CausedBy => "caused-by",
            Self::Enabled => "enabled",
            Self::Prevented => "prevented",
            Self::Supersedes => "supersedes",
            Self::DerivesFrom => "derives-from",
            Self::CoOccursWith => "co-occurs-with",
            Self::NoEffect => "no-effect",
        }
    }

    /// Returns whether this relation forms part of a supersedes chain.
    #[must_use]
    pub fn is_revision(&self) -> bool {
        matches!(self, Self::Supersedes)
    }
}

/// The inputs a [`Derivation`] is constructed from.
///
/// `recorded_at` is supplied rather than read from the clock so that identical inputs produce an
/// identical record, which is what makes determinism a testable property rather than an aspiration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationDraft {
    /// Identity to record under.
    pub id: DerivationId,
    /// What kind of thing is being derived.
    pub kind: DerivationKind,
    /// The window and selection the derivation runs over.
    pub scope: DerivationScope,
    /// The uncertainty to record.
    pub profile: UncertaintyProfile,
    /// What produced the result.
    pub method: DerivationMethod,
    /// Projected facets, empty for non-facet kinds.
    pub facets: Vec<(String, FacetValue)>,
    /// Caller-supplied record time.
    pub recorded_at: DateTime<Utc>,
    /// The derivation this one replaces, if any.
    pub supersedes: Option<DerivationId>,
}

/// A record of what was derived, over which evidence, and how uncertain the result is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Derivation {
    id: DerivationId,
    kind: DerivationKind,
    scope: DerivationScope,
    profile: UncertaintyProfile,
    method: DerivationMethod,
    facets: Vec<(String, FacetValue)>,
    recorded_at: DateTime<Utc>,
    supersedes: Option<DerivationId>,
}

/// The wire shape a derivation is deserialized from, before its invariants are re-checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDerivation {
    id: DerivationId,
    kind: DerivationKind,
    scope: DerivationScope,
    profile: UncertaintyProfile,
    method: DerivationMethod,
    facets: Vec<(String, FacetValue)>,
    recorded_at: DateTime<Utc>,
    supersedes: Option<DerivationId>,
}

impl<'de> Deserialize<'de> for Derivation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawDerivation::deserialize(deserializer)?;
        let derivation = Self {
            id: raw.id,
            kind: raw.kind,
            scope: raw.scope,
            profile: raw.profile,
            method: raw.method,
            facets: raw.facets,
            recorded_at: raw.recorded_at,
            supersedes: raw.supersedes,
        };
        derivation.validate().map_err(serde::de::Error::custom)?;
        Ok(derivation)
    }
}

impl Derivation {
    /// Creates a derived record.
    ///
    /// # Errors
    ///
    /// Returns [`DerivationError::BlankField`] when any facet namespace is blank,
    /// [`DerivationError::SelfSupersession`] when `supersedes` names this record,
    /// [`DerivationError::FacetKindMismatch`] when a [`DerivationKind::Facet`] derivation carries no
    /// facets or a non-facet kind carries some, and [`DerivationError::UngatedDisposition`] when an
    /// assisted method records a band above [`ConfidenceBand::Weak`].
    pub fn new(draft: DerivationDraft) -> Result<Self, DerivationError> {
        let derivation = Self {
            id: draft.id,
            kind: draft.kind,
            scope: draft.scope,
            profile: draft.profile,
            method: draft.method,
            facets: draft.facets,
            recorded_at: draft.recorded_at,
            supersedes: draft.supersedes,
        };
        derivation.validate()?;
        Ok(derivation)
    }

    /// Re-checks every construction invariant.
    ///
    /// Called on construction and on deserialization. Deriving `Deserialize` would let a stored
    /// record skip these checks entirely, and a record read back from disk is exactly as untrusted
    /// as one arriving from a producer.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Derivation::new`].
    fn validate(&self) -> Result<(), DerivationError> {
        for (namespace, _) in &self.facets {
            if namespace.trim().is_empty() {
                return Err(DerivationError::BlankField {
                    field: "facet namespace",
                });
            }
        }

        if self.supersedes == Some(self.id) {
            return Err(DerivationError::SelfSupersession);
        }

        if self.kind.carries_facets() != !self.facets.is_empty() {
            return Err(DerivationError::FacetKindMismatch);
        }

        if !self.method.may_dispose() && !self.profile.confidence().permitted_for_assisted() {
            return Err(DerivationError::UngatedDisposition);
        }

        Ok(())
    }

    /// Returns the record identity.
    #[must_use]
    pub fn id(&self) -> DerivationId {
        self.id
    }

    /// Returns what kind of thing was derived.
    #[must_use]
    pub fn kind(&self) -> DerivationKind {
        self.kind
    }

    /// Returns the window and selection this was computed over.
    #[must_use]
    pub fn scope(&self) -> &DerivationScope {
        &self.scope
    }

    /// Returns the stored uncertainty profile.
    #[must_use]
    pub fn profile(&self) -> UncertaintyProfile {
        self.profile
    }

    /// Returns what produced this record.
    #[must_use]
    pub fn method(&self) -> &DerivationMethod {
        &self.method
    }

    /// Returns the projected facets.
    #[must_use]
    pub fn facets(&self) -> &[(String, FacetValue)] {
        &self.facets
    }

    /// Returns the caller-supplied record time.
    #[must_use]
    pub fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }

    /// Returns the derivation this one replaces, if any.
    #[must_use]
    pub fn supersedes(&self) -> Option<DerivationId> {
        self.supersedes
    }

    /// Returns the SHA-256 digest over the canonical record.
    ///
    /// # Errors
    ///
    /// Returns [`DerivationError::IntegritySerialization`] when the record cannot be serialized.
    pub fn content_hash(&self) -> Result<[u8; 32], DerivationError> {
        let bytes =
            serde_json::to_vec(self).map_err(|_| DerivationError::IntegritySerialization)?;
        Ok(Sha256::digest(&bytes).into())
    }
}

/// A directed, typed edge between two records, carrying its own uncertainty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relationship {
    id: RelationshipId,
    from: EvidenceTarget,
    to: EvidenceTarget,
    relation: RelationKind,
    profile: UncertaintyProfile,
    method: DerivationMethod,
    recorded_at: DateTime<Utc>,
}

/// The wire shape a relationship is deserialized from, before its invariants are re-checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRelationship {
    id: RelationshipId,
    from: EvidenceTarget,
    to: EvidenceTarget,
    relation: RelationKind,
    profile: UncertaintyProfile,
    method: DerivationMethod,
    recorded_at: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for Relationship {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawRelationship::deserialize(deserializer)?;
        let relationship = Self {
            id: raw.id,
            from: raw.from,
            to: raw.to,
            relation: raw.relation,
            profile: raw.profile,
            method: raw.method,
            recorded_at: raw.recorded_at,
        };
        relationship.validate().map_err(serde::de::Error::custom)?;
        Ok(relationship)
    }
}

impl Relationship {
    /// Creates a relationship between two records.
    ///
    /// # Errors
    ///
    /// Returns [`DerivationError::SelfRelationship`] when `from` and `to` are the same target, and
    /// [`DerivationError::UngatedDisposition`] when an assisted method records a band above
    /// [`ConfidenceBand::Weak`].
    pub fn new(
        id: RelationshipId,
        from: EvidenceTarget,
        to: EvidenceTarget,
        relation: RelationKind,
        profile: UncertaintyProfile,
        method: DerivationMethod,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, DerivationError> {
        let relationship = Self {
            id,
            from,
            to,
            relation,
            profile,
            method,
            recorded_at,
        };
        relationship.validate()?;
        Ok(relationship)
    }

    /// Re-checks every construction invariant on the read path.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Relationship::new`].
    fn validate(&self) -> Result<(), DerivationError> {
        if self.from == self.to {
            return Err(DerivationError::SelfRelationship);
        }
        if !self.method.may_dispose() && !self.profile.confidence().permitted_for_assisted() {
            return Err(DerivationError::UngatedDisposition);
        }
        Ok(())
    }

    /// Returns the relationship identity.
    #[must_use]
    pub fn id(&self) -> RelationshipId {
        self.id
    }

    /// Returns the source target.
    #[must_use]
    pub fn from(&self) -> EvidenceTarget {
        self.from
    }

    /// Returns the target target.
    #[must_use]
    pub fn to(&self) -> EvidenceTarget {
        self.to
    }

    /// Returns the relation kind.
    #[must_use]
    pub fn relation(&self) -> RelationKind {
        self.relation
    }

    /// Returns the stored uncertainty profile.
    #[must_use]
    pub fn profile(&self) -> UncertaintyProfile {
        self.profile
    }

    /// Returns what produced this relationship.
    #[must_use]
    pub fn method(&self) -> &DerivationMethod {
        &self.method
    }

    /// Returns the caller-supplied record time.
    #[must_use]
    pub fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }
}

/// A facet value as it appears in a resolved current view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentFacet {
    derivation: DerivationId,
    namespace: String,
    name: String,
    value: FacetValue,
    profile: UncertaintyProfile,
    current: bool,
}

impl CurrentFacet {
    /// Creates one row of a resolved current view.
    ///
    /// # Errors
    ///
    /// Returns [`DerivationError::BlankField`] when `namespace` or `name` is blank.
    pub fn new(
        derivation: DerivationId,
        namespace: impl Into<String>,
        name: impl Into<String>,
        value: FacetValue,
        profile: UncertaintyProfile,
        current: bool,
    ) -> Result<Self, DerivationError> {
        let namespace = namespace.into();
        let name = name.into();
        if namespace.trim().is_empty() {
            return Err(DerivationError::BlankField {
                field: "facet namespace",
            });
        }
        if name.trim().is_empty() {
            return Err(DerivationError::BlankField {
                field: "facet name",
            });
        }
        Ok(Self {
            derivation,
            namespace,
            name,
            value,
            profile,
            current,
        })
    }

    /// Returns the derivation this row came from.
    #[must_use]
    pub fn derivation(&self) -> DerivationId {
        self.derivation
    }

    /// Returns the facet namespace.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the facet name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the facet value.
    #[must_use]
    pub fn value(&self) -> &FacetValue {
        &self.value
    }

    /// Returns the uncertainty profile of the source derivation.
    #[must_use]
    pub fn profile(&self) -> UncertaintyProfile {
        self.profile
    }

    /// Returns whether this row is the head of its supersedes chain.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.current
    }
}

/// One grouped facet value and how many derivations carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetCount {
    value: FacetValue,
    count: u64,
    freshness: Freshness,
}

impl FacetCount {
    /// Creates one grouped count.
    #[must_use]
    pub fn new(value: FacetValue, count: u64, freshness: Freshness) -> Self {
        Self {
            value,
            count,
            freshness,
        }
    }

    /// Returns the grouped value.
    #[must_use]
    pub fn value(&self) -> &FacetValue {
        &self.value
    }

    /// Returns how many derivations carried it.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Returns the weakest freshness among the grouped derivations.
    #[must_use]
    pub fn freshness(&self) -> Freshness {
        self.freshness
    }
}

/// Failures raised while constructing derived records.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DerivationError {
    /// A required field was empty or whitespace.
    #[error("{field} must not be blank")]
    BlankField {
        /// The field that was blank.
        field: &'static str,
    },

    /// An identity string could not be parsed.
    #[error("identity is not a valid ULID")]
    InvalidIdentity,

    /// The scope window ended before it began.
    #[error("scope window is inverted")]
    InvertedWindow,

    /// A derivation was recorded as replacing itself.
    #[error("a derivation cannot supersede itself")]
    SelfSupersession,

    /// Facet presence did not match the declared derivation kind.
    #[error("facet presence does not match the derivation kind")]
    FacetKindMismatch,

    /// A relationship connected a record to itself.
    #[error("relationship endpoints must differ")]
    SelfRelationship,

    /// An assisted method recorded a confidence band it may not hold.
    #[error("assisted derivations may not exceed weak confidence")]
    UngatedDisposition,

    /// The record could not be serialized for hashing.
    #[error("failed to serialize the derived record")]
    IntegritySerialization,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone as _, Utc};
    use serde_json::json;

    use super::{
        ConfidenceBand, Contradiction, CurrentFacet, Derivation, DerivationDraft, DerivationError,
        DerivationId, DerivationKind, DerivationMethod, DerivationScope, EvidenceRole,
        EvidenceTarget, FacetCount, FacetFilter, FacetValueSlot, Freshness, RelationKind,
        Relationship, RelationshipId, ScopeFidelity, UncertaintyProfile,
    };
    use crate::{
        FacetValue, Observation, ObservationDraft, ObservationId, ObservationKind, Provenance,
        SourceRef, SubjectRef,
    };

    /// Records a real observation so the test holds a genuine ledger identity.
    fn observation_id() -> ObservationId {
        let observation = Observation::record(ObservationDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("manual", "evidra note").expect("source should validate"),
            kind: ObservationKind::ManualIntervention,
            subject: SubjectRef::new("repository", "/workspace").expect("subject should validate"),
            payload: json!({"summary": "fixture"}),
            provenance: Provenance::direct("evidra-core-tests")
                .expect("provenance should validate"),
        })
        .expect("observation should record");
        *observation.id()
    }
    fn scope() -> DerivationScope {
        let subject = SubjectRef::new("repository", "/workspace").expect("subject should validate");
        let from = Utc
            .with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
            .single()
            .expect("valid instant");
        let to = from + Duration::days(1);
        DerivationScope::new(subject, from, to, Vec::new()).expect("window should be ordered")
    }

    fn deterministic() -> DerivationMethod {
        DerivationMethod::Deterministic {
            version: "1".into(),
        }
    }

    fn assisted() -> DerivationMethod {
        DerivationMethod::Assisted {
            model: "test-model".into(),
            prompt_version: "p1".into(),
        }
    }

    fn facet_draft(id: DerivationId) -> DerivationDraft {
        DerivationDraft {
            id,
            kind: DerivationKind::Facet,
            scope: scope(),
            profile: UncertaintyProfile::deterministic(),
            method: deterministic(),
            facets: vec![("outcome".into(), FacetValue::Text("verified-fail".into()))],
            recorded_at: Utc::now(),
            supersedes: None,
        }
    }

    /// Confirms a facet derivation records its identity, scope, and facets.
    #[test]
    fn derivation_records_identity_and_facets() {
        let id = DerivationId::new();
        let derivation = Derivation::new(facet_draft(id)).expect("valid draft should construct");

        assert_eq!(derivation.id(), id);
        assert_eq!(derivation.kind(), DerivationKind::Facet);
        assert_eq!(derivation.facets().len(), 1);
        assert!(derivation.supersedes().is_none());
    }

    /// Confirms `recorded_at` is supplied rather than read from the clock, so identical inputs
    /// produce an identical record.
    #[test]
    fn identical_inputs_yield_identical_digest() -> Result<(), Box<dyn std::error::Error>> {
        let at = Utc
            .with_ymd_and_hms(2026, 10, 2, 12, 0, 0)
            .single()
            .expect("valid instant");
        let id = DerivationId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ULID");

        let mut first = facet_draft(id);
        first.recorded_at = at;
        let mut second = facet_draft(id);
        second.recorded_at = at;

        let left = Derivation::new(first)?;
        let right = Derivation::new(second)?;

        assert_eq!(left.content_hash()?, right.content_hash()?);
        Ok(())
    }

    /// Confirms a derivation that declares facets must carry some, and vice versa.
    #[test]
    fn facet_presence_must_match_kind() {
        let mut empty = facet_draft(DerivationId::new());
        empty.facets.clear();
        assert_eq!(
            Derivation::new(empty).expect_err("facet kind with no facets should fail"),
            DerivationError::FacetKindMismatch
        );

        let mut aggregate = facet_draft(DerivationId::new());
        aggregate.kind = DerivationKind::Aggregate;
        assert_eq!(
            Derivation::new(aggregate).expect_err("aggregate with facets should fail"),
            DerivationError::FacetKindMismatch
        );
    }

    /// Confirms a derivation cannot supersede itself.
    #[test]
    fn self_supersession_is_refused() {
        let id = DerivationId::new();
        let mut draft = facet_draft(id);
        draft.supersedes = Some(id);
        assert_eq!(
            Derivation::new(draft).expect_err("self supersession should fail"),
            DerivationError::SelfSupersession
        );
    }

    /// Confirms an assisted method may not record a confidence band above weak.
    #[test]
    fn assisted_method_is_band_capped() {
        let mut draft = facet_draft(DerivationId::new());
        draft.method = assisted();
        draft.profile = UncertaintyProfile::deterministic().with_confidence(ConfidenceBand::Strong);
        assert_eq!(
            Derivation::new(draft).expect_err("assisted at strong should fail"),
            DerivationError::UngatedDisposition
        );

        let allowed = facet_draft(DerivationId::new());
        let mut allowed = allowed;
        allowed.method = assisted();
        allowed.profile = UncertaintyProfile::assisted();
        assert!(Derivation::new(allowed).is_ok());
    }

    /// Confirms a deterministic method may hold any band, including strong.
    #[test]
    fn deterministic_method_may_hold_strong() {
        let draft = facet_draft(DerivationId::new());
        let derivation = Derivation::new(draft).expect("deterministic strong should construct");
        assert_eq!(derivation.profile().confidence(), ConfidenceBand::Strong);
    }

    /// Confirms assisted confidence caps are a property of the band, not of a call site.
    #[test]
    fn assisted_band_cap_holds_for_every_band() {
        for band in [
            ConfidenceBand::Speculative,
            ConfidenceBand::Weak,
            ConfidenceBand::Moderate,
            ConfidenceBand::Strong,
        ] {
            assert_eq!(
                band.permitted_for_assisted(),
                matches!(band, ConfidenceBand::Speculative | ConfidenceBand::Weak),
                "band {band:?} cap must be decided by the band alone"
            );
        }
    }

    /// Confirms only deterministic methods may dispose.
    #[test]
    fn only_deterministic_methods_may_dispose() {
        assert!(deterministic().may_dispose());
        assert!(!assisted().may_dispose());
    }

    /// Confirms an inverted scope window is refused.
    #[test]
    fn inverted_window_is_refused() {
        let subject = SubjectRef::new("repository", "/workspace").expect("subject should validate");
        let from = Utc
            .with_ymd_and_hms(2026, 10, 2, 0, 0, 0)
            .single()
            .expect("valid instant");
        let to = from - Duration::days(1);
        assert_eq!(
            DerivationScope::new(subject, from, to, Vec::new())
                .expect_err("inverted window should fail"),
            DerivationError::InvertedWindow
        );
    }

    /// Confirms scope membership is inclusive of both bounds.
    #[test]
    fn scope_window_is_inclusive() {
        let scope = scope();
        assert!(scope.contains(scope.from_occurred_at()));
        assert!(scope.contains(scope.to_occurred_at()));
        assert!(!scope.contains(scope.to_occurred_at() + Duration::seconds(1)));
    }

    /// Confirms facet filters reject blank namespaces and names.
    #[test]
    fn facet_filter_rejects_blank_parts() {
        assert_eq!(
            FacetFilter::new("  ", "name", FacetValue::Boolean(true))
                .expect_err("blank namespace should fail"),
            DerivationError::BlankField {
                field: "facet namespace"
            }
        );
        assert_eq!(
            FacetFilter::new("ns", "  ", FacetValue::Boolean(true))
                .expect_err("blank name should fail"),
            DerivationError::BlankField {
                field: "facet name"
            }
        );
    }

    /// Confirms the slot always names the column that stores a value.
    #[test]
    fn facet_slot_matches_value() {
        assert_eq!(
            FacetValueSlot::of(&FacetValue::Text("a".into())),
            FacetValueSlot::Text
        );
        assert_eq!(
            FacetValueSlot::of(&FacetValue::Integer(1)),
            FacetValueSlot::Integer
        );
        assert_eq!(
            FacetValueSlot::of(&FacetValue::Boolean(false)),
            FacetValueSlot::Boolean
        );
    }

    /// Confirms a relationship cannot connect a record to itself.
    #[test]
    fn self_relationship_is_refused() {
        let target = EvidenceTarget::Derivation(DerivationId::new());
        assert_eq!(
            Relationship::new(
                RelationshipId::new(),
                target,
                target,
                RelationKind::Supports,
                UncertaintyProfile::deterministic(),
                deterministic(),
                Utc::now(),
            )
            .expect_err("self relationship should fail"),
            DerivationError::SelfRelationship
        );
    }

    /// Confirms an assisted relationship is band capped exactly as a derivation is.
    #[test]
    fn assisted_relationship_is_band_capped() {
        let result = Relationship::new(
            RelationshipId::new(),
            EvidenceTarget::Derivation(DerivationId::new()),
            EvidenceTarget::Derivation(DerivationId::new()),
            RelationKind::CausedBy,
            UncertaintyProfile::deterministic(),
            assisted(),
            Utc::now(),
        );
        assert_eq!(
            result.expect_err("assisted at strong should fail"),
            DerivationError::UngatedDisposition
        );
    }

    /// Confirms targets are keyed by kind and by canonical ULID, so both directions are indexable
    /// without a join and the key survives a process boundary.
    #[test]
    fn evidence_target_keys_are_distinct_by_kind() {
        let observation = EvidenceTarget::Observation(observation_id());
        let derivation = EvidenceTarget::Derivation(DerivationId::default());
        assert_ne!(observation.key(), derivation.key());
        assert!(observation.key().starts_with("observation:"));
        assert!(derivation.key().starts_with("derivation:"));

        // The key must be the ULID, never the debug rendering of the wrapper.
        let id = DerivationId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ULID");
        assert_eq!(
            EvidenceTarget::Derivation(id).key(),
            "derivation:01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
    }

    /// Confirms only supersedes participates in a revision chain.
    #[test]
    fn revision_relations_are_identified() {
        assert!(RelationKind::Supersedes.is_revision());
        for kind in [
            RelationKind::Supports,
            RelationKind::Refutes,
            RelationKind::CausedBy,
            RelationKind::Enabled,
            RelationKind::Prevented,
            RelationKind::DerivesFrom,
            RelationKind::CoOccursWith,
            RelationKind::NoEffect,
        ] {
            assert!(!kind.is_revision(), "{kind:?} must not be a revision edge");
        }
    }

    /// Confirms the profile carries five independent dimensions rather than one score.
    #[test]
    fn profile_dimensions_are_independent() {
        let profile = UncertaintyProfile::deterministic()
            .with_confidence(ConfidenceBand::Moderate)
            .with_freshness(Freshness::Stale)
            .with_contradiction(Contradiction::Reconciled)
            .with_scope(ScopeFidelity::Narrower);

        assert_eq!(profile.confidence(), ConfidenceBand::Moderate);
        assert_eq!(profile.freshness(), Freshness::Stale);
        assert_eq!(profile.contradiction(), Contradiction::Reconciled);
        assert_eq!(profile.scope(), ScopeFidelity::Narrower);
    }

    /// Confirms an assisted profile starts contested rather than clean.
    #[test]
    fn assisted_profile_is_contested() {
        let profile = UncertaintyProfile::assisted();
        assert_eq!(profile.confidence(), ConfidenceBand::Weak);
        assert_eq!(profile.contradiction(), Contradiction::Contested);
    }

    /// Confirms identity parsing rejects blank and malformed values.
    #[test]
    fn derivation_id_parse_rejects_junk() {
        assert!(matches!(
            DerivationId::parse("   "),
            Err(DerivationError::BlankField { .. })
        ));
        assert_eq!(
            DerivationId::parse("not-a-ulid").expect_err("junk should fail"),
            DerivationError::InvalidIdentity
        );
        let parsed =
            DerivationId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ULID should parse");
        assert_eq!(parsed.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    }

    /// Confirms a current-view row distinguishes a chain head from a retained predecessor.
    #[test]
    fn current_facet_marks_chain_heads() {
        let head = DerivationId::new();
        let retained = DerivationId::new();
        let row = CurrentFacet::new(
            head,
            "outcome",
            "class",
            FacetValue::Text("verified-fail".into()),
            UncertaintyProfile::deterministic(),
            true,
        )
        .expect("valid row should construct");
        assert!(row.is_current());
        assert_ne!(row.derivation(), retained);
        assert!(
            !CurrentFacet::new(
                retained,
                "outcome",
                "class",
                FacetValue::Text("verified-pass".into()),
                UncertaintyProfile::deterministic(),
                false,
            )
            .expect("retained row should construct")
            .is_current()
        );
    }

    /// Confirms grouped counts carry freshness so a stale group is visible without re-querying.
    #[test]
    fn facet_count_reports_freshness() {
        let count = FacetCount::new(FacetValue::Integer(3), 7, Freshness::Aging);
        assert_eq!(count.count(), 7);
        assert_eq!(count.freshness(), Freshness::Aging);
    }

    /// Confirms evidence roles are distinguishable, since refuting links must never be dropped.
    #[test]
    fn evidence_roles_are_distinct() {
        assert_ne!(
            EvidenceRole::Supporting.as_str(),
            EvidenceRole::Refuting.as_str()
        );
        assert_eq!(EvidenceRole::Refuting.as_str(), "refuting");
    }

    /// Confirms inhibited and null relations are expressible, which a what-only graph cannot do.
    #[test]
    fn inhibited_and_null_relations_exist() {
        assert_eq!(RelationKind::Prevented.as_str(), "prevented");
        assert_eq!(RelationKind::NoEffect.as_str(), "no-effect");
    }

    /// Confirms a source reference validates so derivation errors stay distinct from observation ones.
    #[test]
    fn source_ref_validation_is_independent() {
        assert!(SourceRef::new("manual", "note").is_ok());
        assert!(SourceRef::new("", "note").is_err());
    }

    /// Confirms a round-tripped derivation survives serialization and deserialization unchanged.
    #[test]
    fn derivation_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let original = Derivation::new(facet_draft(DerivationId::new()))?;
        let json = serde_json::to_string(&original)?;
        let restored: Derivation = serde_json::from_str(&json)?;
        assert_eq!(original, restored);
        Ok(())
    }

    /// Confirms deserialization cannot smuggle in a self-supersession.
    ///
    /// A record read back from disk is exactly as untrusted as one arriving from a producer, so the
    /// constructor's invariants must be re-checked rather than assumed.
    #[test]
    fn deserialization_cannot_smuggle_self_supersession() -> Result<(), Box<dyn std::error::Error>>
    {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let raw = format!(
            r#"{{
              "id": "{id}",
              "kind": "facet",
              "scope": {{
                "subject": {{"kind": "repository", "path": "/workspace"}},
                "from_occurred_at": "2026-10-01T00:00:00Z",
                "to_occurred_at": "2026-10-02T00:00:00Z",
                "selection": []
              }},
              "profile": {{
                "confidence": "strong",
                "freshness": "current",
                "contradiction": "uncontested",
                "scope": "exact"
              }},
              "method": {{"kind": "deterministic", "version": "1"}},
              "facets": [["outcome", {{"Text": "verified-fail"}}]],
              "recorded_at": "2026-10-02T09:00:00Z",
              "supersedes": "{id}"
            }}"#
        );

        assert!(
            serde_json::from_str::<Derivation>(&raw).is_err(),
            "a self-superseding record must not survive deserialization"
        );
        Ok(())
    }

    /// Confirms deserialization cannot smuggle in an assisted method holding a strong band.
    #[test]
    fn deserialization_cannot_smuggle_ungated_confidence() -> Result<(), Box<dyn std::error::Error>>
    {
        let raw = r#"{
          "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
          "kind": "aggregate",
          "scope": {
            "subject": {"kind": "repository", "path": "/workspace"},
            "from_occurred_at": "2026-10-01T00:00:00Z",
            "to_occurred_at": "2026-10-02T00:00:00Z",
            "selection": []
          },
          "profile": {
            "confidence": "strong",
            "freshness": "current",
            "contradiction": "uncontested",
            "scope": "exact"
          },
          "method": {"kind": "assisted", "model": "m", "prompt_version": "p"},
          "facets": [],
          "recorded_at": "2026-10-02T09:00:00Z",
          "supersedes": null
        }"#;

        assert!(
            serde_json::from_str::<Derivation>(raw).is_err(),
            "an assisted record at strong confidence must not survive deserialization"
        );
        Ok(())
    }

    /// Confirms deserialization cannot smuggle in a self-referential relationship.
    #[test]
    fn deserialization_cannot_smuggle_self_relationship() {
        let raw = r#"{
              "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
              "from": {"observation": "01ARZ3NDEKTSV4RRFFQ69G5FAV"},
              "to": {"observation": "01ARZ3NDEKTSV4RRFFQ69G5FAV"},
              "relation": "supports",
              "profile": {
                "confidence": "strong",
                "freshness": "current",
                "contradiction": "uncontested",
                "scope": "exact"
              },
              "method": {"kind": "deterministic", "version": "1"},
              "recorded_at": "2026-10-02T09:00:00Z"
            }"#;

        assert!(
            serde_json::from_str::<Relationship>(raw).is_err(),
            "a self-referential relationship must not survive deserialization"
        );
    }

    /// Property coverage for the derived-domain invariants.
    ///
    /// The tests above pin named cases. These range over the input space the constructors actually
    /// accept: eight polymorphic fields on [`DerivationDraft`] and seven on [`Relationship`]. The
    /// round-trip property carries the most weight here, because every field is serde-derived and
    /// the read path re-runs validation that a stored record could otherwise bypass.
    mod props {
        use chrono::{DateTime, Duration, TimeZone as _, Utc};
        use proptest::prelude::*;
        use ulid::Ulid;

        use super::{
            ConfidenceBand, Contradiction, Derivation, DerivationDraft, DerivationId,
            DerivationKind, DerivationMethod, DerivationScope, EvidenceTarget, FacetValueSlot,
            Freshness, RelationKind, Relationship, RelationshipId, ScopeFidelity,
            UncertaintyProfile,
        };
        use crate::{FacetValue, SubjectRef};

        /// Crockford base32 as fixed by the ULID specification.
        const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

        /// Any valid ULID string.
        ///
        /// A 26-character ULID encodes 130 bits into a 128-bit value, so the leading character
        /// carries only three significant bits and may not exceed `7`. Restricting it keeps every
        /// generated string parseable, which turns a broken assumption here into a failed property
        /// rather than a silently skipped one.
        fn ulid_string() -> impl Strategy<Value = String> {
            (
                prop::sample::select(&ALPHABET[..8]),
                prop::collection::vec(prop::sample::select(ALPHABET), 25),
            )
                .prop_map(|(first, rest)| {
                    let mut encoded = String::with_capacity(26);
                    encoded.push(char::from(first));
                    for symbol in rest {
                        encoded.push(char::from(symbol));
                    }
                    encoded
                })
        }

        /// Any derivation identity.
        fn derivation_id() -> impl Strategy<Value = DerivationId> {
            ulid_string()
                .prop_map(|raw| DerivationId::parse(&raw).expect("generated ULID is well formed"))
        }

        /// Any relationship identity.
        fn relationship_id() -> impl Strategy<Value = RelationshipId> {
            ulid_string().prop_map(|raw| {
                RelationshipId(Ulid::from_string(&raw).expect("generated ULID is well formed"))
            })
        }

        /// A short token that is never blank and cannot collide with the sentinels below.
        fn token() -> impl Strategy<Value = String> {
            "[a-z]{1,8}".prop_map(String::from)
        }

        /// Any instant inside a representable range.
        fn instant() -> impl Strategy<Value = DateTime<Utc>> {
            (0i64..4_102_444_800_000i64).prop_map(|millis| {
                Utc.timestamp_millis_opt(millis)
                    .single()
                    .expect("a UTC millisecond is never ambiguous")
            })
        }

        /// Any subject that passes the blank check.
        fn subject() -> impl Strategy<Value = SubjectRef> {
            ("repository", token()).prop_map(|(kind, identifier)| {
                SubjectRef::new(kind, identifier).expect("generated subject is not blank")
            })
        }

        /// Any scope over an ordered window.
        fn scope() -> impl Strategy<Value = DerivationScope> {
            (subject(), instant(), 0i64..86_400_000i64).prop_map(|(subject, from, span)| {
                DerivationScope::new(
                    subject,
                    from,
                    from + Duration::milliseconds(span),
                    Vec::new(),
                )
                .expect("generated window is ordered")
            })
        }

        /// Any facet value.
        fn facet_value() -> impl Strategy<Value = FacetValue> {
            prop_oneof![
                any::<String>().prop_map(FacetValue::Text),
                any::<i64>().prop_map(FacetValue::Integer),
                any::<bool>().prop_map(FacetValue::Boolean),
            ]
        }

        /// Any confidence band.
        fn band() -> impl Strategy<Value = ConfidenceBand> {
            prop_oneof![
                Just(ConfidenceBand::Speculative),
                Just(ConfidenceBand::Weak),
                Just(ConfidenceBand::Moderate),
                Just(ConfidenceBand::Strong),
            ]
        }

        /// Any freshness band.
        fn freshness() -> impl Strategy<Value = Freshness> {
            prop_oneof![
                Just(Freshness::Current),
                Just(Freshness::Aging),
                Just(Freshness::Stale),
            ]
        }

        /// Any contradiction state.
        fn contradiction() -> impl Strategy<Value = Contradiction> {
            prop_oneof![
                Just(Contradiction::Uncontested),
                Just(Contradiction::Contested),
                Just(Contradiction::Reconciled),
            ]
        }

        /// Any scope-fidelity state.
        fn fidelity() -> impl Strategy<Value = ScopeFidelity> {
            prop_oneof![
                Just(ScopeFidelity::Exact),
                Just(ScopeFidelity::Broader),
                Just(ScopeFidelity::Narrower),
            ]
        }

        /// Any relation kind.
        fn relation() -> impl Strategy<Value = RelationKind> {
            prop_oneof![
                Just(RelationKind::Supports),
                Just(RelationKind::Refutes),
                Just(RelationKind::CausedBy),
                Just(RelationKind::Enabled),
                Just(RelationKind::Prevented),
                Just(RelationKind::Supersedes),
                Just(RelationKind::DerivesFrom),
                Just(RelationKind::CoOccursWith),
                Just(RelationKind::NoEffect),
            ]
        }

        /// Any evidence target.
        ///
        /// Observations need a real ledger identity and `ObservationId::new` is private, so the
        /// observation arm reuses the fixture identity rather than inventing one.
        fn evidence_target() -> impl Strategy<Value = EvidenceTarget> {
            prop_oneof![
                Just(EvidenceTarget::Observation(super::observation_id())),
                derivation_id().prop_map(EvidenceTarget::Derivation),
            ]
        }

        /// Any draft [`Derivation::new`] is obliged to accept.
        ///
        /// Facets are always generated non-empty and then dropped for kinds that do not carry
        /// them, because a [`DerivationKind::Facet`] record with no facets is itself rejected: a
        /// facet derivation that projects nothing is a contradiction rather than a valid record.
        ///
        /// The band is clamped when the method is assisted, so the strategy yields only records the
        /// constructor must accept. The rejection paths are covered by their own property rather
        /// than filtered out here.
        fn accepted_draft() -> impl Strategy<Value = DerivationDraft> {
            (
                derivation_id(),
                prop::option::of(derivation_id()),
                prop_oneof![
                    Just(DerivationKind::Facet),
                    Just(DerivationKind::Aggregate),
                    Just(DerivationKind::Cluster),
                ],
                scope(),
                any::<bool>(),
                band(),
                freshness(),
                contradiction(),
                fidelity(),
                prop::collection::vec((token(), facet_value()), 1..4),
                instant(),
            )
                .prop_filter(
                    "supersession never names the record itself",
                    |(id, supersedes, ..)| supersedes != &Some(*id),
                )
                .prop_map(
                    |(
                        id,
                        supersedes,
                        kind,
                        scope,
                        assisted,
                        confidence,
                        freshness,
                        contradiction,
                        fidelity,
                        facets,
                        recorded_at,
                    )| {
                        let method = if assisted {
                            DerivationMethod::Assisted {
                                model: "test-model".into(),
                                prompt_version: "p1".into(),
                            }
                        } else {
                            DerivationMethod::Deterministic {
                                version: "1".into(),
                            }
                        };
                        let confidence = if assisted && !confidence.permitted_for_assisted() {
                            ConfidenceBand::Weak
                        } else {
                            confidence
                        };
                        let facets = if kind.carries_facets() {
                            facets
                        } else {
                            Vec::new()
                        };
                        DerivationDraft {
                            id,
                            kind,
                            scope,
                            profile: UncertaintyProfile::deterministic()
                                .with_confidence(confidence)
                                .with_freshness(freshness)
                                .with_contradiction(contradiction)
                                .with_scope(fidelity),
                            method,
                            facets,
                            recorded_at,
                            supersedes,
                        }
                    },
                )
        }

        /// Builds a profile deterministically, for use outside a combinator.
        fn uncertainty_of(confidence: ConfidenceBand) -> UncertaintyProfile {
            UncertaintyProfile::deterministic().with_confidence(confidence)
        }

        /// Any relationship [`Relationship::new`] is obliged to accept.
        fn accepted_relationship() -> impl Strategy<Value = Relationship> {
            (
                relationship_id(),
                evidence_target(),
                evidence_target(),
                relation(),
                band(),
                any::<bool>(),
                instant(),
            )
                .prop_filter(
                    "a relationship never points at itself",
                    |(_, from, to, ..)| from != to,
                )
                .prop_map(
                    |(id, from, to, relation, confidence, assisted, recorded_at)| {
                        let method = if assisted {
                            DerivationMethod::Assisted {
                                model: "test-model".into(),
                                prompt_version: "p1".into(),
                            }
                        } else {
                            DerivationMethod::Deterministic {
                                version: "1".into(),
                            }
                        };
                        let confidence = if assisted && !confidence.permitted_for_assisted() {
                            ConfidenceBand::Weak
                        } else {
                            confidence
                        };
                        Relationship::new(
                            id,
                            from,
                            to,
                            relation,
                            uncertainty_of(confidence),
                            method,
                            recorded_at,
                        )
                        .expect("strategy produces an accepted relationship")
                    },
                )
        }

        proptest! {
            /// A record accepted at construction survives a JSON round trip unchanged.
            ///
            /// This is the property the manual `Deserialize` exists to protect: the wire shape is
            /// untrusted, so the read path must re-check every invariant rather than trust the
            /// bytes it was handed.
            #[test]
            fn roundtrip_preserves_record(draft in accepted_draft()) {
                let derivation =
                    Derivation::new(draft).expect("strategy produces an accepted draft");

                let encoded = serde_json::to_string(&derivation).expect("record serializes");
                let decoded: Derivation =
                    serde_json::from_str(&encoded).expect("record deserializes");

                prop_assert_eq!(&decoded, &derivation);
                prop_assert_eq!(
                    decoded.content_hash().expect("digest is computable"),
                    derivation.content_hash().expect("digest is computable"),
                );
            }

            /// A relationship accepted at construction survives a JSON round trip unchanged.
            #[test]
            fn relationship_roundtrip_preserves_record(relationship in accepted_relationship()) {
                let encoded = serde_json::to_string(&relationship).expect("record serializes");
                let decoded: Relationship =
                    serde_json::from_str(&encoded).expect("record deserializes");

                prop_assert_eq!(&decoded, &relationship);
            }

            /// An assisted method is accepted exactly when its band is permitted.
            ///
            /// Stated as an equivalence rather than one direction, so a band that is refused but
            /// should have been accepted fails just as loudly as the smuggling case.
            #[test]
            fn assisted_is_never_confident(confidence in band(), recorded_at in instant()) {
                let draft = DerivationDraft {
                    id: DerivationId::new(),
                    kind: DerivationKind::Aggregate,
                    scope: DerivationScope::new(
                        SubjectRef::new("repository", "workspace").expect("subject"),
                        recorded_at,
                        recorded_at + Duration::days(1),
                        Vec::new(),
                    )
                    .expect("window is ordered"),
                    profile: uncertainty_of(confidence),
                    method: DerivationMethod::Assisted {
                        model: "test-model".into(),
                        prompt_version: "p1".into(),
                    },
                    facets: Vec::new(),
                    recorded_at,
                    supersedes: None,
                };

                match Derivation::new(draft) {
                    Ok(record) => prop_assert!(
                        record.profile().confidence().permitted_for_assisted(),
                        "an assisted record at {:?} must not exceed the cap",
                        confidence,
                    ),
                    Err(_) => prop_assert!(
                        !confidence.permitted_for_assisted(),
                        "a permitted band was refused at {:?}",
                        confidence,
                    ),
                }
            }

            /// The recorded slot always matches the variant of the value it stores.
            #[test]
            fn slot_matches_value(value in facet_value()) {
                let expected = match value {
                    FacetValue::Text(_) => FacetValueSlot::Text,
                    FacetValue::Integer(_) => FacetValueSlot::Integer,
                    FacetValue::Boolean(_) => FacetValueSlot::Boolean,
                };
                prop_assert_eq!(FacetValueSlot::of(&value), expected);
            }

            /// Debug output never carries a facet value or a subject identifier.
            ///
            /// Namespace and name are deliberately not asserted: they are structural keys needed to
            /// make a record legible in diagnostics. Facet values and the subject identifier are
            /// evidence-derived and must stay out.
            #[test]
            fn debug_never_contains_payload(marker in token(), recorded_at in instant()) {
                let value = format!("val-{marker}");
                let identifier = format!("subj-{marker}");
                let subject =
                    SubjectRef::new("repository", identifier.clone()).expect("subject is not blank");
                let from = recorded_at;
                let derivation = Derivation::new(DerivationDraft {
                    id: DerivationId::new(),
                    kind: DerivationKind::Facet,
                    scope: DerivationScope::new(
                        subject,
                        from,
                        from + Duration::days(1),
                        Vec::new(),
                    )
                    .expect("window is ordered"),
                    profile: uncertainty_of(ConfidenceBand::Speculative),
                    method: DerivationMethod::Assisted {
                        model: "test-model".into(),
                        prompt_version: "p1".into(),
                    },
                    facets: vec![("outcome".into(), FacetValue::Text(value.clone()))],
                    recorded_at,
                    supersedes: None,
                })
                .expect("facet derivation is accepted");

                let rendered = format!("{derivation:?}");
                prop_assert!(
                    !rendered.contains(&value),
                    "facet value leaked into debug output: {rendered}",
                );
                prop_assert!(
                    !rendered.contains(&identifier),
                    "subject identifier leaked into debug output: {rendered}",
                );
            }
        }
    }
}

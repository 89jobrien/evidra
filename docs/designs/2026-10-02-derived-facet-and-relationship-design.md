# Design: Derived Facets and Relationships

- Status: Proposed
- Proposed: 2026-10-02

## Goal

Add a revisable derived layer above the immutable observation ledger: banded facets that can be
aggregated and queried, and typed append-only relationships that connect observations, facets, and
later claims — each carrying evidence links, scope, freshness, contradiction, and confidence, and each
persisted separately from the observations it interprets.

This is the facet-derivation slice anticipated by the agent-harness evidence contract design. It
fills the reserved `evidra-engine` boundary. It does not introduce failure attribution or decision
capture; those are scoped in [Follow-Up Slices](#follow-up-slices).

## Approved Approach

Derive, never mutate.

`observations` remains the only mutable-free ledger and keeps its three immutability triggers. The
derived layer adds `derivations`, `derivation_facets`, `derivation_evidence`, and `relationships` as
**four additional append-only tables**. No `UPDATE` or `DELETE` is issued anywhere in schema v3.

"Revisable" is therefore not implemented as row mutation. A correction appends a _new_ derivation and
a `supersedes` relationship pointing at the old one; the current view is resolved by walking that
chain. This satisfies ADR-002 (corrections are additional records and relationships),
ADR-003 (claims never summarize away their evidence), and ADR-004 (the uncertainty profile
travels with the record) without introducing a single in-place edit.

Facets are stored as **projected, banded columns** alongside the JSON document so that aggregation
is a `GROUP BY` rather than a re-parse. Numeric signals are bucketed at derivation time; a raw
`duration_ms` facet cannot answer "which failure class recurs" but a `session.duration.bucket` can.

Inference is separated from authority. Every derivation records the method that produced it, and
only deterministic methods may emit an evaluative disposition. Model-assisted derivations are
labeled `Assisted` and may cluster, propose, or summarize, but cannot produce an `allow` outcome.

## Contents

- [Why the current model is insufficient](#why-the-current-model-is-insufficient)
- [Context Map](#context-map)
- [Crate Ownership](#crate-ownership)
- [Public API](#public-api)
- [Storage Format](#storage-format)
- [Facet Taxonomy](#facet-taxonomy)
- [Validation Rules](#validation-rules)
- [Data Flow](#data-flow)
- [Hexagonal Boundaries](#hexagonal-boundaries)
- [Reliability Expectations](#reliability-expectations)
- [Conformance Strategy](#conformance-strategy)
- [Policy Evaluation via Rulery](#policy-evaluation-via-rulery)
- [Property Tests](#property-tests)
- [Roadmap Checkpoints](#roadmap-checkpoints)
- [Out of Scope](#out-of-scope)
- [Follow-Up Slices](#follow-up-slices)
- [Risk Checklist](#risk-checklist)

## Why the current model is insufficient

`observations` answers "what happened, according to whom." It cannot answer three questions that
operational intelligence requires:

1. **Aggregation.** Every stored payload is an opaque `serde_json::Value`. Answering "which failure
   classes recur in this crate over the last 40 sessions" means parsing every document on every
   query. The harness contract already established `ObservationFacet` as immutable source-reported
   metadata, but those facets live inside the payload and are not indexed.
2. **Relationship.** ADR-002 states corrections "must be represented as additional observations and
   relationships." No relationship primitive exists, so the only way to express that one record
   bears on another is to duplicate it.
3. **Revisability with uncertainty.** ADR-004 requires quality, freshness, contradiction, scope, and
   confidence to be first-class state. `Observation` has `Provenance` and `IntegrityRecord` but no
   confidence, no scope, and no contradiction channel.

## Context Map

### Files to Modify

| File                                                               | Purpose                           | Changes Needed                                                                      |
| ------------------------------------------------------------------ | --------------------------------- | ----------------------------------------------------------------------------------- |
| `crates/evidra-core/src/derivation.rs`                             | New domain                        | Derived facet, uncertainty profile, evidence link, relationship types               |
| `crates/evidra-core/src/lib.rs`                                    | Public API exports                | Export the derivation domain                                                        |
| `crates/evidra-core/src/ports.rs`                                  | Domain ports                      | Add `DerivationStore` and `RelationshipStore`                                       |
| `crates/evidra-engine/src/lib.rs`                                  | Reserved boundary                 | Deterministic deriver, banding tables, aggregation                                  |
| `crates/evidra-store/src/sqlite.rs`                                | Schema v3                         | Four append-only tables, migration v2 to v3, queries                                |
| `crates/evidra-store/src/lib.rs`                                   | Adapter exports                   | Export the two stores                                                               |
| `crates/evidra-cli/src/main.rs`                                    | Composition root                  | `derive`, `facet`, `relate`, `explain` subcommands                                  |
| `docs/conformance.md`                                              | New contract index                | Numbered contract clauses cited by every conformance test                           |
| `crates/evidra-core/tests/conformance_derivation_domain.rs`        | New conformance suite             | §1–§5 core derived-domain contracts                                                 |
| `crates/evidra-store/tests/conformance_derivation_store.rs`        | New conformance suite             | §6 `DerivationStore` port substitutability                                          |
| `crates/evidra-store/tests/conformance_relationship_store.rs`      | New conformance suite             | §7 `RelationshipStore` port substitutability                                        |
| `crates/evidra-store/tests/conformance_schema_migration.rs`        | New conformance suite             | §8 schema ladder and non-rewrite guarantee                                          |
| `crates/evidra-engine/tests/conformance_derivation.rs`             | New conformance suite             | §9 determinism and method gating                                                    |
| `crates/evidra-core/tests/conformance_architecture.rs`             | New conformance suite             | §10 dependency and layering contracts                                               |
| `crates/evidra-store/tests/conformance_policy_files.rs`            | New conformance suite             | §11 `policies/` structural contracts                                                |
| `crates/evidra-core/tests/property_derivation_domain.rs`           | New property suite                | Slot/value agreement, round-trip, tampering, namespace rejection, `Debug` redaction |
| `crates/evidra-core/tests/property_uncertainty_profile.rs`         | New property suite                | Profile consistency invariants including `Assisted` confidence gating               |
| `crates/evidra-store/tests/property_supersedes_chain.rs`           | New property suite                | Chain resolution over arbitrary graph shapes                                        |
| `crates/evidra-store/tests/property_store_invariants.rs`           | New property suite                | Aggregation conservation, scope confinement, trigger enforcement                    |
| `crates/evidra-engine/tests/property_banding.rs`                   | New property suite                | Band totality, monotonicity, cardinality, edge mapping                              |
| `crates/evidra-engine/tests/property_determinism.rs`               | New property suite                | Byte-identical output for identical input                                           |
| `Cargo.toml`                                                       | Workspace manifest                | Add `proptest` and the `evidra-engine` workspace dependency                         |
| `Cargo.toml`                                                       | Workspace manifest (Slice 4 only) | Raise `rust-version` to 1.98, add the `rulery` path dependency, move to resolver 3  |
| `policies/`                                                        | Policy intent (Slice 4 only)      | Collapse four empty subdirectories into one Rulery package layout                   |
| `README.md`                                                        | Workspace status                  | Mark the derived layer as implemented                                               |
| `AGENTS.md`                                                        | Agent architecture guidance       | Replace the reserved-boundary description                                           |
| `docs/designs/2026-10-02-derived-facet-and-relationship-design.md` | This design                       | —                                                                                   |

### Dependencies

| File                                                    | Relationship                                                                               |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `docs/adr/ADR-002-append-only-observations.md`          | Requires corrections to be new records and relationships, not mutations                    |
| `docs/adr/ADR-003-separate-evidence-from-claims.md`     | Requires evidence links to be retained and both supporting and refuting evidence supported |
| `docs/adr/ADR-004-explicit-uncertainty.md`              | Requires quality, freshness, contradiction, scope, and confidence as first-class state     |
| `docs/adr/ADR-005-deterministic-control-boundaries.md`  | Restricts authority decisions to deterministic code                                        |
| `docs/adr/ADR-007-sqlite-and-versioned-policy-files.md` | Keeps SQLite behind a core port and policy intent in Git-tracked files                     |
| `crates/evidra-core/src/harness.rs`                     | `ObservationFacet` and `FacetValue` vocabulary reused for projected facets                 |
| `crates/evidra-core/src/observation.rs`                 | Private fields, validating constructors, redacted `Debug`, `deny_unknown_fields`           |
| `crates/evidra-store/src/sqlite.rs`                     | Migration ladder, integrity validation, and the append-only trigger pattern                |
| `crates/evidra-adapters/src/inbox.rs`                   | Upstream producer boundary; unchanged in this slice                                        |

### Test Coverage

| Test Location                                             | Coverage                                                                                                           |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `crates/evidra-core/src/derivation.rs`                    | Constructor validation, band parsing, evidence-set role rules, redacted `Debug`, unknown-field rejection           |
| `crates/evidra-engine/src/lib.rs`                         | Banding boundaries, aggregation correctness, determinism, method gating                                            |
| `crates/evidra-store/src/sqlite.rs`                       | v2-to-v3 migration, trigger enforcement, index-backed aggregation, round-trip integrity                            |
| `docs/conformance.md` + `crates/*/tests/conformance_*.rs` | The eleven numbered contract sections in [Conformance Strategy](#conformance-strategy)                             |
| `crates/*/tests/property_*.rs`                            | The twenty-one invariants in [Property Tests](#property-tests), each with a committed `proptest-regressions/` seed |

Focused unit tests stay inline as `#[cfg(test)] mod tests`, matching the existing convention in
`observation.rs`, `harness.rs`, `ingest.rs`, and `sqlite.rs`. Conformance suites are the cross-cutting
layer and live in `tests/`, per `system-patterns.md`: _"Test invariants at their owning layer: domain
construction/integrity, store mutation/schema/path controls, and end-to-end CLI behavior."_

There is no existing coverage for derived state, facets, or relationships, and no conformance
structure of any kind in the workspace today: no conformance crate, no `assert_*_contract` function,
no protocol-drift surface, and no feature flags. `evidra-engine` has no tests because it exposes no
API.

### Reference Patterns

| File                                                | Pattern to Follow                                                          |
| --------------------------------------------------- | -------------------------------------------------------------------------- |
| `crates/evidra-core/src/observation.rs`             | `IntegrityEnvelope` serialization plus digest verification on deserialize  |
| `crates/evidra-core/src/ports.rs`                   | Associated adapter error satisfying `Error + Send + Sync + 'static`        |
| `crates/evidra-store/src/sqlite.rs`                 | `migrate_v1_to_v2`, `validate_v2`, and the per-table immutability triggers |
| `crates/evidra-adapters/src/agent_harness_jsonl.rs` | Bounded input, fixed diagnostics that never echo source values             |

### Risk

- Facet names are open strings, so a typo silently creates an unqueryable namespace. Banding tables
  in `evidra-engine` must be the only producer of names, and unknown namespaces must be rejected on
  write rather than stored.
- `derivation_evidence.target_id` is polymorphic across observations and derivations, so it cannot
  carry a foreign key. Referential integrity is enforced in code, not by the engine.
- A derived layer that is appended faster than it is queried grows without bound. Retention is out of
  scope, so this slice must document that derivation volume is a function of observation count.

## Crate Ownership

- **`evidra-core`** owns the derived domain: uncertainty profile, evidence links, facet projections,
  relationships, their validation invariants, and the two new store ports.
- **`evidra-engine`** owns deterministic derivation: band definitions, facet projection from
  observations, aggregation, and the method gating that separates inference from authority. It has
  no dependency on `evidra-store`.
- **`evidra-store`** owns persistence for the derived tables, the v2-to-v3 migration, and the
  aggregation queries.
- **`evidra-cli`** composes and renders only.

No new crate is added, per the workspace rule that a crate requires a distinct responsibility that
cannot fit an existing boundary. `evidra-engine` exists precisely for this work.

```text
evidra-cli -> evidra-store -> evidra-core
          \-> evidra-engine -> evidra-core
evidra-adapters -> evidra-core
```

`evidra-engine` depends on `evidra-core` only. It must not depend on `evidra-store`; the CLI
composes an engine result and hands it to a store.

## Public API

### Confidence and Quality Bands

Confidence is banded rather than a float. Published step-level attribution accuracy for LLM-based
methods sits between roughly 14% and 47% on the `Who&When` benchmark; a continuous score would imply
a precision the method does not have. Bands force consumers to acknowledge the uncertainty class.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfidenceBand {
    /// Heuristic or unverified; must not be cited as a finding.
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
    pub fn as_str(&self) -> &'static str;
}
```

### Uncertainty Profile

ADR-004 requires five dimensions. `confidence` alone is insufficient because a high-confidence claim
can still be stale, contradicted, or out of scope.

```rust
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

    /// Returns the confidence band.
    #[must_use]
    pub fn confidence(&self) -> ConfidenceBand;

    /// Returns the freshness dimension.
    #[must_use]
    pub fn freshness(&self) -> Freshness;

    /// Returns the contradiction dimension.
    #[must_use]
    pub fn contradiction(&self) -> Contradiction;

    /// Returns the scope fidelity dimension.
    #[must_use]
    pub fn scope(&self) -> ScopeFidelity;
}
```

The four dimensions are independently stored so a consumer can filter on `freshness = stale` without
parsing a composite score. Per ADR-004, a derived aggregate may rank work but never replaces this
profile.

### Derivation Method and Gating

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DerivationMethod {
    /// Reproducible from recorded observations by versioned local code.
    Deterministic { version: String },
    /// Produced by a model. Never eligible for an evaluative disposition.
    Assisted {
        model: String,
        prompt_version: String,
    },
}

impl DerivationMethod {
    /// Returns whether this method may produce an evaluative disposition.
    ///
    /// Only [`DerivationMethod::Deterministic`] may. ADR-005 reserves authority for deterministic
    /// code.
    #[must_use]
    pub fn may_dispose(&self) -> bool;
}
```

### Derivation Scope and Selection

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivationScope {
    subject: SubjectRef,
    from_occurred_at: DateTime<Utc>,
    to_occurred_at: DateTime<Utc>,
    selection: Vec<FacetFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetFilter {
    namespace: String,
    name: String,
    value: FacetValue,
}
```

`DerivationScope` is stored, not inferred. A derivation that does not record the window and
selection it ran over cannot be recomputed or falsified, and `ScopeFidelity` would be
unverifiable.

### Facet Projection

```rust
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
    pub fn as_str(&self) -> &'static str;

    /// Returns the slot that stores `value`.
    #[must_use]
    pub fn of(value: &FacetValue) -> Self;
}
```

Projected facets reuse the harness contract's `FacetValue`, so `evidra-core` has one facet value
type rather than two. `FacetValueSlot` names which of the three indexed columns carries the value, and
the schema's `CHECK` constraint requires exactly that one column to be populated and the other two to
be null. Unlike source-reported facets, which are immutable metadata attached by a producer, a
projected facet is computed by `evidra-engine` and lives in an indexed column.

### Evidence Links

ADR-003 requires both supporting and refuting evidence to be retained.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceRole {
    /// Evidence that increases belief in the derivation.
    Supporting,
    /// Evidence that counts against it. Must be retained, never dropped.
    Refuting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceTarget {
    /// Points at an immutable observation.
    Observation(ObservationId),
    /// Points at another derivation, permitting chained inference.
    Derivation(DerivationId),
}
```

### Relationship Kinds

The vocabulary is open-ended on the evidence side but closed on the relation side, because a
closed relation set is what makes the graph queryable.

```rust
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
```

`Prevented` and `NoEffect` are load-bearing. A graph that records only what happened cannot answer
"what would have avoided this" or "what was already ruled out," which are the two questions that make
a failure actionable rather than merely explicable.

### Derivation and Relationship Records

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DerivationId(Ulid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelationshipId(Ulid);

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

pub struct Relationship {
    id: RelationshipId,
    from: EvidenceTarget,
    to: EvidenceTarget,
    relation: RelationKind,
    profile: UncertaintyProfile,
    method: DerivationMethod,
    recorded_at: DateTime<Utc>,
}
```

Both carry private fields, validating constructors, `deny_unknown_fields` deserialization, and
redacted `Debug`. Both are immutable once recorded. `Derivation::supersedes` is denormalized from
the relationship graph for query efficiency and must agree with the corresponding `Supersedes`
relationship or the record is rejected on write.

#### `recorded_at` is supplied, never read from the clock

`recorded_at` is a constructor parameter on both records. It is **not** stamped from `Utc::now()` the
way `Observation::record` stamps `observed_at` at `observation.rs:411`.

This is a deliberate divergence and it exists so that determinism is a testable property rather than
an aspiration. The engine must produce byte-identical output for identical input
(§9, `property_derivation_determinism`), which is only checkable if the caller controls the timestamp.
A clock read inside the constructor would make every derivation unique and the property vacuous.

`DerivationId` and `RelationshipId` are likewise caller-supplied in tests, which is what allows a
generator to assert chain-resolution behavior over arbitrary shapes. Production callers pass
`Ulid::new()`; `evidra-engine` passes a deterministic derivation from the input to keep replay stable.

### Domain Errors

```rust
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DerivationError {
    #[error("{field} must not be blank")]
    BlankField { field: &'static str },

    #[error("scope window is inverted")]
    InvertedWindow,

    #[error("derived facet namespace {namespace} is not registered")]
    UnknownNamespace { namespace: String },

    #[error("derived facet {namespace}.{name} has too many distinct values")]
    UnboundedFacet { namespace: String, name: String },

    #[error("relationship targets must differ")]
    SelfRelationship,

    #[error("supersedes link is missing its relationship")]
    UnlinkedSupersedes,

    #[error("assisted derivation may not produce an evaluative disposition")]
    UngatedDisposition,

    #[error("failed to serialize derivation integrity envelope")]
    IntegritySerialization,
}
```

Error display text must never include observation payloads, facet values from source-provided
documents, or locator strings. `UnboundedFacet` reports the facet's own identity because that is
Evidra-authored vocabulary, not source content.

### Store Ports

```rust
pub trait DerivationStore {
    type Error: Error + Send + Sync + 'static;

    /// Appends a derivation and its indexed facet projection atomically.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when the record cannot be appended or a facet namespace is unknown.
    fn append(&mut self, derivation: &Derivation) -> Result<(), Self::Error>;

    /// Returns facet values for a scope, resolving the `Supersedes` chain so each
    /// superseded derivation is represented exactly once by its current successor.
    ///
    /// This is a read-through view. Superseded rows remain stored and remain individually
    /// retrievable; this method decides which ones count as current.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when the projection cannot be read or the chain cannot be resolved.
    fn current_facets(
        &self,
        scope: &DerivationScope,
    ) -> Result<Vec<CurrentFacet>, Self::Error>;

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

pub trait RelationshipStore {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentFacet {
    pub derivation: DerivationId,
    pub namespace: String,
    pub name: String,
    pub value: FacetValue,
    pub profile: UncertaintyProfile,
    /// True when this derivation is the head of its supersedes chain.
    pub current: bool,
}
```

`current_facets` exists because the port is otherwise write-only. Without it the supersedes chain
cannot be resolved, and the "derive, never mutate" promise would have no reader.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetCount {
    pub value: FacetValue,
    pub count: u64,
    pub freshness: Freshness,
}
```

## Storage Format

Schema version advances from 2 to 3. `evidra init` remains the only migration entry point, and the
migration must not rewrite existing observation documents.

```sql
CREATE TABLE IF NOT EXISTS derivations (
    id TEXT PRIMARY KEY NOT NULL,
    recorded_at TEXT NOT NULL,
    document TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS derivations_recorded_at_idx
    ON derivations (recorded_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS derivation_facets (
    derivation_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    slot TEXT NOT NULL CHECK (slot IN ('text', 'integer', 'boolean')),
    value_text TEXT,
    value_integer INTEGER,
    value_boolean INTEGER,
    PRIMARY KEY (derivation_id, namespace, name),
    FOREIGN KEY (derivation_id) REFERENCES derivations(id),
    CHECK (
        (slot = 'text'      AND value_text IS NOT NULL AND value_integer IS NULL AND value_boolean IS NULL)
     OR (slot = 'integer'  AND value_text IS NULL     AND value_integer IS NOT NULL AND value_boolean IS NULL)
     OR (slot = 'boolean'  AND value_text IS NULL     AND value_integer IS NULL     AND value_boolean IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS derivation_facets_group_idx
    ON derivation_facets (namespace, name, value_text);

CREATE INDEX IF NOT EXISTS derivation_facets_numeric_idx
    ON derivation_facets (namespace, name, value_integer);

CREATE TABLE IF NOT EXISTS derivation_evidence (
    derivation_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('supporting', 'refuting')),
    target_kind TEXT NOT NULL CHECK (target_kind IN ('observation', 'derivation')),
    target_id TEXT NOT NULL,
    PRIMARY KEY (derivation_id, role, target_kind, target_id),
    FOREIGN KEY (derivation_id) REFERENCES derivations(id)
);

CREATE TABLE IF NOT EXISTS relationships (
    id TEXT PRIMARY KEY NOT NULL,
    from_key TEXT NOT NULL,
    to_key TEXT NOT NULL,
    relation TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    document TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS relationships_from_idx
    ON relationships (from_key, relation);
CREATE INDEX IF NOT EXISTS relationships_to_idx
    ON relationships (to_key, relation);
```

`relationships` projects `from` and `to` into `from_key` and `to_key` text columns of the form
`observation:01J...` or `derivation:01J...`, and `relation` into a text column, so that both edge
directions are indexable without a join table and without parsing the JSON document. The `document`
column remains the source of truth for the full record; the three projected columns are validated
against it on write and are covered by the same integrity digest.

Each of the four tables receives the same three immutability triggers used by `observations`, with a
table-specific message. `derivation_facets` and `derivation_evidence` are projections of an already
immutable `derivations` row, so mutating them independently would break the integrity digest; they
are append-only for the same reason.

`derivation_evidence.target_id` is polymorphic and therefore carries no foreign key. `evidra-store`
verifies referential integrity in code before appending and returns `StoreError::DanglingEvidence`.

The derived records carry their own SHA-256 `IntegrityRecord` over the same envelope shape used by
`Observation`, so a tampered derivation fails to deserialize.

## Facet Taxonomy

`evidra-engine` owns the band tables. Namespaces are registered constants; an unregistered namespace
is rejected on write rather than stored, because a typo would otherwise create a permanently
unqueryable partition.

| Namespace  | Facet                  | Derivation                                                             | Rationale                                                         |
| ---------- | ---------------------- | ---------------------------------------------------------------------- | ----------------------------------------------------------------- |
| `subject`  | `crate`                | Repository-relative crate or module from the subject locator           | Primary aggregation axis for per-crate failure rates              |
| `subject`  | `path.depth`           | Banded directory depth of the subject locator                          | Distinguishes leaf edits from whole-tree operations               |
| `session`  | `duration.bucket`      | `xs` under 5m, `s` 5–15m, `m` 15–60m, `l` 1–4h, `xl` over 4h           | Raw milliseconds cannot be grouped meaningfully                   |
| `session`  | `turns.bucket`         | Banded user-turn count                                                 | Separates one-shot fixes from extended work                       |
| `session`  | `tool.errors.bucket`   | `0`, `1-2`, `3-5`, `6-10`, `10+`                                       | Error-count density is the strongest cheap failure predictor      |
| `session`  | `delegation.depth`     | Maximum observed subagent nesting                                      | godmode dispatches worktrees; depth predicts coordination failure |
| `outcome`  | `class`                | `verified-pass`, `verified-fail`, `unverified`, `blocked`, `abandoned` | Separates "not yet checked" from "checked and wrong"              |
| `outcome`  | `verification.gate`    | Which gate produced `outcome.class`, when one did                      | Attributes the verdict to a mechanism                             |
| `friction` | `category`             | Normalized friction class from harness-reported facets                 | The recurrence axis users actually ask about                      |
| `friction` | `interruptions.bucket` | Banded user-interruption count                                         | Interruption density correlates with agent drift                  |
| `cost`     | `input.tokens.bucket`  | `0`, `1k-10k`, `10k-50k`, `50k-200k`, `200k+`                          | Cost is a facet, never a ranking key on its own                   |
| `cost`     | `wall.bucket`          | Same bands as `session.duration.bucket`                                | Separates thinking time from waiting time                         |

Two rules govern the whole taxonomy:

1. **No unbounded numeric facet.** Every numeric facet is banded. A raw value may be retained inside
   the JSON document for reference, but the indexed column always holds the band.
2. **Every derived facet is scoped.** A facet value is only meaningful with the window and selection
   that produced it, so `DerivationScope` travels with the record and `ScopeFidelity` reports whether
   the actual evidence matched the declared scope.

The existing source-reported facet names — `agent.name`, `model.id`, `tool.name`, `tool.outcome`,
`policy.decision`, `verification.result` — are read from observation payloads to _filter_ derivations
but are not re-projected. They are already immutable producer metadata and re-deriving them would
create two sources for one fact.

## Validation Rules

- Every identity, namespace, facet name, and method version is non-blank.
- A facet namespace must be registered in the `evidra-engine` band table. An unregistered namespace
  is rejected with `DerivationError::UnknownNamespace` rather than stored, so a misspelling cannot
  create a permanently unqueryable partition.
- A banded integer facet must emit no more than eight distinct bands. Exceeding it yields
  `DerivationError::UnboundedFacet` and indicates a band table that does not match the data.
- `DerivationScope.from_occurred_at` must not exceed `to_occurred_at`.
- `Freshness::Stale` requires at least one `refuting` or superseding link, or an explicit
  `recomputed_from` derivation. Freshness is never asserted without a reason.
- `Contradiction::Uncontested` requires zero refuting evidence links.
- A `DerivationKind::Facet` derivation carries at least one projected facet.
- A `DerivationKind::Aggregate` derivation carries no individual facets; its counts live in the
  document body.
- `RelationKind::Supersedes` requires the `to` target to be a `Derivation` and the source to record a
  matching `supersedes` field. Mismatches fail the append.
- No relationship may have `from == to`.
- `DerivationMethod::Assisted` may not produce an evaluative disposition. Enforced by
  `DerivationMethod::may_dispose`, checked in the engine before write, and asserted in tests.
- Confidence bands are never widened by an `Assisted` method. An assisted derivation may only produce
  `Speculative` or `Weak`.
- Redacted `Debug` output for every derived type omits facet values, payload bodies, and target
  identifiers.
- The existing `observations` triggers and the `AgentHarnessEvent` append path are unchanged.

## Data Flow

1. A producer writes redacted `AgentHarnessEvent` records through the existing inbox path. The
   derived layer does not alter ingestion.
2. `evidra derive` composes an engine deriver with a `DerivationStore`. The deriver selects
   observations by `DerivationScope`, projects banded facets, and computes an `UncertaintyProfile`.
3. The store appends the `Derivation` row and its `derivation_facets` projection in one transaction.
4. A separate step links evidence with explicit `supporting` and `refuting` roles. A derivation with
   no evidence links is not admissible.
5. Corrections append a new derivation plus a `Supersedes` relationship. The previous row is never
   modified; the trigger makes an attempted update abort.
6. `evidra facet` runs a `GROUP BY` against the indexed projection and returns `FacetCount` rows with
   their freshness.
7. `evidra explain` walks `RelationshipStore::neighbors` in both directions to render the evidence
   and refutation chain for a node.

## Hexagonal Boundaries

- **Domain:** `evidra_core::{Derivation, Relationship, UncertaintyProfile, RelationKind, ...}`
- **Inbound port:** `evidra_core::{DerivationStore, RelationshipStore}`
- **Deterministic logic:** `evidra_engine`, depending on `evidra-core` only.
- **Outbound adapter:** `evidra_store::{SqliteDerivationStore, SqliteRelationshipStore}`
- **Composition:** `evidra-cli`, which selects a deriver and a store and renders results.

`evidra-engine` must not open a database, invoke a model, or import `evidra-store`. Model invocation,
if added later, arrives through a port in `evidra-core` so the engine remains testable and
deterministic-only.

## Reliability Expectations

This section constrains how the derived layer may be consumed. It is a design commitment, not
documentation.

1. **Attribution built on this layer will be wrong often.** The best published step-level failure
   attribution results sit near 47% on synthetic benchmarks and near 29% on hand-crafted ones, with
   LLM-judge baselines near 14%. A consumer must not present a `Moderate` derivation as a root cause
   without the evidence chain alongside it.
2. **`CausedBy` is a hypothesis.** The step that manifests a failure is frequently not the step that
   decided it. `CausedBy` links must be recorded with a confidence band and the deciding derivation
   linked separately.
3. **Joint causes are not decomposed.** When two steps fail only together, single-link attribution is
   misleading in both directions. Consumers must read the full inbound `CausedBy` set before
   concluding a single cause, and no derivation may claim sole responsibility without stating its
   method.
4. **`Assisted` output is never authority.** It may cluster, propose, and summarize. Per ADR-005 it
   cannot gate, allow, deny, or escalate.
5. **Absence is not evidence.** A missing event is absence of evidence. The harness contract already
   states this; derivation must not infer that an action did not occur from a missing facet.

## Conformance Strategy

The workspace has 131 focused tests and no conformance layer. This slice introduces one, because the
derived layer's risk is not that individual functions are wrong — it is that an architectural rule is
quietly violated somewhere a unit test cannot see.

### Shape

Conformance suites live in `crates/<crate>/tests/conformance_<surface>.rs` and are plain `#[test]`
functions. No conformance crate, no typed runner, no registration list.

Three reasons, all of them constraints in this workspace rather than preference:

- `Cargo.toml` declares `members = ["crates/*"]`, so a `tests/conformance` sibling would require
  editing the members list.
- `AGENTS.md` states _"Do not add new crates without a distinct responsibility that cannot fit an
  existing boundary."_
- `cargo nextest run --workspace` already picks up `crates/*/tests/`, so this shape needs **zero CI
  edits**. In this shape a forgotten registration is impossible, because the `#[test]` attribute _is_
  the registration.

### Contract index

`docs/conformance.md` is the Git-tracked, numbered index. Every conformance test cites its section in
its name and in every assertion message:

```rust
// §6.3 — a superseded derivation is retained, never rewritten

fn assert_derivation_store_contract(store: &impl DerivationStore) {
    // 3. Superseding appends a new row; the prior row is unchanged and still
    //    retrievable, and only the successor reports `current: true`.
    let first = append_facet_derivation(store, "outcome.class", "verified-fail");
    let second = append_facet_derivation(store, "outcome.class", "verified-pass");
    store
        .link(&second, &first, RelationKind::Supersedes)
        .expect("contract 3: supersedes link should be appendable");

    let view = store
        .current_facets(&store_scope())
        .expect("contract 3: supersedes chain should resolve");

    assert!(
        view.iter().any(|f| f.derivation == first && !f.current),
        "contract 3: superseded derivation must be retained with current=false"
    );
    assert!(
        view.iter().any(|f| f.derivation == second && f.current),
        "contract 3: superseding derivation must report current=true"
    );
    assert_eq!(
        view.iter().filter(|f| f.current).count(),
        1,
        "contract 3: exactly one head per supersedes chain"
    );
}

#[test]
fn sqlite_derivation_store_satisfies_contract() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = SqliteDerivationStore::initialize(&dir.path().join("evidra.db"))
        .expect("store should initialize");
    assert_derivation_store_contract(&store);
}
```

Three conventions carry the whole strategy:

1. **One file per contract surface, filename is the index.** A new port gets a new file; nothing is
   wired.
2. **One shared assertion body, one thin `#[test]` per implementation.** The body is written once and
   never duplicated, so a second store implementation cannot quietly diverge. This is the property that
   distinguishes conformance from an ordinary unit test: it is a _breadth_ guarantee, not a depth one.
3. **Numbered clauses, and every assertion message names its clause.** A failure names the contract it
   broke, not just the line.

### Sections

| §   | File                                | Contract surface                                                                                                                            |
| --- | ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | `conformance_derivation_domain.rs`  | Facet projection: slot/value agreement, registered namespaces, no unbounded band                                                            |
| 2   | `conformance_derivation_domain.rs`  | Uncertainty profile: freshness requires a reason, uncontested requires zero refuting links                                                  |
| 3   | `conformance_derivation_domain.rs`  | Evidence links: both roles retained, polymorphic targets, `Contradiction` consistency                                                       |
| 4   | `conformance_derivation_domain.rs`  | Relationships: closed relation set, no self-loops, supersedes agreement                                                                     |
| 5   | `conformance_derivation_domain.rs`  | Redaction: `Debug` omits facet values, payloads, and target ids for every derived type                                                      |
| 6   | `conformance_derivation_store.rs`   | `DerivationStore`: append-only, supersede-by-append, aggregation scope fidelity                                                             |
| 7   | `conformance_relationship_store.rs` | `RelationshipStore`: append-only, bidirectional neighbours, dangling-target rejection                                                       |
| 8   | `conformance_schema_migration.rs`   | Schema ladder: v1→v2→v3, observation documents never rewritten, unknown version rejected                                                    |
| 9   | `conformance_derivation.rs`         | Engine: identical input yields identical digest, banding is total and monotone, `Assisted` gating                                           |
| 10  | `conformance_architecture.rs`       | Layering: core has no storage dep, engine depends only on core, no cycles                                                                   |
| 11  | `conformance_policy_files.rs`       | `policies/` conforms to the Rulery package layout, compiles under `LockMode::Frozen`, and every assumption declares an invalidation trigger |

§8 must assert the non-rewrite guarantee directly, because it is the one property that would silently
destroy evidence: record every observation document's digest before migration and assert the set is
unchanged afterwards.

§10 turns `AGENTS.md` rules into executable assertions. The dependency check reads each crate's
`Cargo.toml` and asserts `evidra-core` declares no `rusqlite`, HTTP client, or model SDK, and that
`evidra-engine` declares only `evidra-core`.

§11 is the first coverage of `policies/`, which today is four directories holding four `.gitkeep`
files with nothing validating them. ADR-007 mandates the directory; this section makes it real — and
via Rulery, per [Policy Evaluation via Rulery](#policy-evaluation-via-rulery).

### Policy Evaluation via Rulery

ADR-005 requires that _"deterministic code decides whether an action is allowed, denied, warned,
quarantined, or escalated for approval."_ That is a rules engine, and `~/dev/rulery` already is one: a
YAML rule compiler with four-valued truth (`true`/`false`/`unknown`/`invalid`), reproducible traces,
lockfile integrity, and versioned artifacts. Evidra should consume it rather than reimplement
evaluation.

Direction is one-way: **rulery is a dependency of the control layer, never of the derived-facet
layer.** The derived layer stays pure, deterministic, and dependency-light; policy evaluation reads it.

#### What rulery provides, and what it does not

Rulery has **no** concept of an assumption, invariant, control, accepted risk, or invalidation
trigger. Those are not extendable as new authored constructs — `filesystem.rs:16-18` hard-codes
exactly three root files (`rulery.yaml`, `vocabulary.yaml`, `actions.yaml`) and two directories
(`rules/`, `scenarios/`), and `OutcomeKind` is a closed four-variant enum.

But the vocabulary is open, and that is the extension point. An assumption becomes a declared record
type with an `invalidated-at` date field; the invalidation trigger becomes a rule over that field.

```yaml
# policies/vocabulary.yaml
types:
  assumption:
    kind: record
    closed: true
    fields:
      statement: { type: text, presence: required }
      owner: { type: text, presence: required }
      invalidated-at: { type: date, presence: optional }
      invalidated-when: { type: text, presence: optional }
      review-by: { type: date, presence: optional }
roots:
  assumption: { type: assumption }
```

This requires **no rulery source change and no `docs/specification.md` change**, so it does not touch
the 40-`RUL`-code conformance gate. Evidra owns the semantic layer; rulery owns type checking,
precedence, evaluation, and trace reproduction.

#### The disposition mapping is lossy and must be explicit

Evidra's ADR-005 names five dispositions. Rulery has four outcome kinds. There is no `warn` and no
`quarantine`, and **adding a fifth variant is not an option** — it would touch `outcome.rs`,
`validate_precedence` in `package.rs:220-231`, `policy_evaluator.rs`, and the spec's rank table.

| Evidra disposition | Rulery encoding                                           | Note                                                                                                                            |
| ------------------ | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| allow              | `Outcome::Approve`                                        | Direct                                                                                                                          |
| deny               | `Outcome::Deny`                                           | Direct                                                                                                                          |
| escalate           | `Outcome::Escalate { destination }`                       | Direct                                                                                                                          |
| warn               | `Outcome::Approve` + declarative `emit-warning` action    | Actions are declarative obligations; rulery v0.1 does not execute them, so the disposition is non-blocking and machine-readable |
| quarantine         | `Outcome::Escalate { destination: "quarantine:sandbox" }` | Distinguished by `EscalationId`, not by a new variant                                                                           |

One precedence hazard: under `safety_first`, `approve` loses to `escalate` and `request_information`
at equal priority. Because `warn` is encoded as `Approve`, a policy mixing warns with escalations must
use `priority_first` or an explicit rank map, or warnings will be silently swallowed.

#### Consumption surface

Use `ProductionApplication`, which already wires `FilesystemPackageStore`, `JiffTimeZoneDatabase`,
and `PolicyCompiler`:

```rust
pub trait ApplicationService {
    fn compile_package(&self, root: &PackagePath, lock_mode: LockMode)
        -> Result<CompileWorkflowOutput, ApplicationError>;
    fn evaluate_at(&self, package: &CompiledPackage, decision: &DecisionId,
                   facts: &CaseFacts, at: UtcInstant)
        -> Result<DecisionTrace, ApplicationError>;
}
```

Two signature traps, both cases where the specification and the shipped code disagree:

- The spec declares `DecisionEvaluator: Send + Sync` taking an `EvaluationContext`. **`EvaluationContext`
  does not exist in any crate.** The shipped trait is `PolicyEvaluator`, has no supertrait, and takes
  five parameters including an explicit `evaluated_at: UtcInstant`. Cite the implementation.
- The spec declares `PackageStore: Send + Sync` with an associated `Error`. The shipped trait has
  neither and hard-codes `Result<_, StoreError>`.

`ProductionApplication::evaluate_at` hard-codes `TraceDetail::Complete`. Requesting compact traces
requires dropping to `ProductionPolicyEvaluator` directly.

#### What this changes outside the control layer

- **`recorded_at` discipline is now load-bearing twice over.** `evaluate_at` takes an explicit
  `UtcInstant`, so a policy evaluation is reproducible from `(package, decision, facts, instant, tzdb)`
  — the same determinism property the derived layer claims. Both must stay consistent: a derivation
  evaluated at `t` and a policy evaluated at `t` must agree on `t`.
- **`BTreeMap`/`BTreeSet` only.** Rulery's `trace_hash` reproducibility depends on it. Building
  `CaseFacts` through a `HashMap` breaks the hash.
- **Trace reproducibility carries a host dependency.** `JiffTimeZoneDatabase::local_date` reads the
  host tzdb and records the identity `"jiff/system-tzdb"`, so bit-identical traces across machines
  require declaring `timezone: UTC` in the manifest and injecting an identity-bearing adapter. Flag
  this in any conformance assertion that compares trace hashes.
- **The shipped hash contract is serde-JSON bytes, not JCS.** `specification.md:1831` claims RFC 8785
  via `serde_jcs`, but there is no `serde_jcs` in `Cargo.lock`; determinism comes from
  `BTreeMap`/`BTreeSet` ordering plus serde field order. Cross-language hash reproduction must target
  the shipped contract.

#### Costs, stated plainly

| Cost                    | Detail                                                                                                                                                                                                                                                                     |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **MSRV**                | rulery requires Rust **1.98**; evidra declares `rust-version = "1.85.0"`. A path dependency forces the workspace floor to 1.98, and rulery uses resolver 3 against evidra's 2.                                                                                             |
| **Not published**       | rulery has no crates.io release, no git tags, and only an `## [Unreleased]` changelog. Consume as a path dependency or pinned git/submodule.                                                                                                                               |
| **Directory shape**     | rulery's layout is incompatible with `policies/{assumptions,controls,invariants,accepted-risks}/`. Those four directories must collapse into one rulery package, with the four concerns becoming fact roots inside `vocabulary.yaml`.                                      |
| **Lockfile discipline** | `LockMode::Frozen` requires `rulery.lock` to exist and match. Only `rulery lock` writes it, ambient env vars do nothing, and CI must pass `Frozen` explicitly. A policy edit without a re-lock fails compilation — which is the desired behaviour, but must be documented. |

#### Why `unknown` matters here

Rulery's most relevant property for evidra is that an absent fact is `unknown`, not `false`. Its own
embedding fixture states the reason:

> an absent fact is `unknown`, not `false`. A consumer that flattens absence into denial cannot
> distinguish "we know this is unacceptable" from "we do not know", and the escalation outcome is
> what carries that difference.

This is the same principle as `## Reliability Expectations` item 5 and as ADR-004's requirement that
uncertainty be first-class. Rulery enforces it in the evaluator; a hand-rolled `if` chain would not.

### Property Tests

Conformance tests check a fixed, enumerated set of clauses. Property tests cover the space the clause
list cannot: arbitrary inputs, arbitrary chain shapes, arbitrary band boundaries. Both layers are
required — a conformance suite that enumerates three banding cases says nothing about the fourth.

Property tests live in `crates/<crate>/tests/property_<surface>.rs`, run under plain
`cargo nextest run --workspace`, and are **not** registered anywhere else. This is deliberate:
proptest needs its own harness for shrink output and regression replay, so these cases must execute
through the standard Rust test harness rather than the conformance path. Failed cases are replayed
from a checked-in `proptest-regressions/` seed directory.

`proptest` is added to `[workspace.dependencies]` as a `[dev-dependencies]` entry to `evidra-core`,
`evidra-engine`, and `evidra-store`. It is the workspace's first property-testing dependency;
`memx`, `tracers`, and `godmode` all already use it, so the version choice has precedent.

#### Properties

| File                                                | Property                                  | Holds that                                                                                          |
| --------------------------------------------------- | ----------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `evidra-engine/tests/property_banding.rs`           | `banding_is_total`                        | Every input maps to exactly one band; never panics, never returns a gap                             |
| `evidra-engine/tests/property_banding.rs`           | `banding_is_monotone`                     | If `a <= b` then `band(a) <= band(b)` under band ordinal order                                      |
| `evidra-engine/tests/property_banding.rs`           | `band_cardinality_is_bounded`             | No banded facet ever emits more than the eight bands its table declares                             |
| `evidra-engine/tests/property_banding.rs`           | `band_edges_map_to_declared_band`         | A value exactly on a band boundary maps to the documented band, not its neighbour                   |
| `evidra-core/tests/property_uncertainty_profile.rs` | `uncontested_implies_no_refuting_links`   | `Contradiction::Uncontested` and any `refuting` evidence cannot coexist                             |
| `evidra-core/tests/property_uncertainty_profile.rs` | `stale_requires_a_reason`                 | `Freshness::Stale` requires a superseding or refuting link                                          |
| `evidra-core/tests/property_uncertainty_profile.rs` | `assisted_is_never_confident`             | For any method and confidence, `Assisted` permits only `Speculative` or `Weak`                      |
| `evidra-core/tests/property_derivation_domain.rs`   | `slot_matches_value`                      | `FacetValueSlot` always names the column that is populated                                          |
| `evidra-core/tests/property_derivation_domain.rs`   | `roundtrip_preserves_record`              | Serialize then deserialize yields an equal value, for any valid record                              |
| `evidra-core/tests/property_derivation_domain.rs`   | `tampering_breaks_integrity`              | Any single-field mutation of an encoded record fails deserialization                                |
| `evidra-core/tests/property_derivation_domain.rs`   | `unknown_namespace_is_rejected`           | Any unregistered namespace string is refused rather than stored                                     |
| `evidra-core/tests/property_derivation_domain.rs`   | `debug_never_contains_payload`            | `Debug` output contains no facet value, payload field, or target id, for any record                 |
| `evidra-store/tests/property_supersedes_chain.rs`   | `acyclic_chain_has_one_head`              | Any acyclic supersedes graph resolves to exactly one current derivation per component               |
| `evidra-store/tests/property_supersedes_chain.rs`   | `cyclic_chain_is_rejected`                | Any chain containing a cycle is refused at write rather than resolved                               |
| `evidra-store/tests/property_supersedes_chain.rs`   | `resolution_is_order_independent`         | The head of a chain does not depend on insertion order                                              |
| `evidra-store/tests/property_store_invariants.rs`   | `aggregation_conserves_rows`              | `sum(FacetCount.count)` equals the number of matching rows; `GROUP BY` loses and duplicates nothing |
| `evidra-store/tests/property_store_invariants.rs`   | `scope_never_leaks`                       | An aggregate over a scope never counts a derivation whose `occurred_at` falls outside it            |
| `evidra-store/tests/property_store_invariants.rs`   | `update_always_aborts`                    | `UPDATE` on any derived table raises for every stored row, never succeeds                           |
| `evidra-store/tests/property_store_invariants.rs`   | `duplicate_append_is_rejected`            | Appending a record with a used identity yields a rejection, never a silent replace                  |
| `evidra-store/tests/property_store_invariants.rs`   | `relationship_never_self_loops`           | Any `from`/`to` pair with equal targets is refused                                                  |
| `evidra-engine/tests/property_determinism.rs`       | `identical_input_yields_identical_record` | The same inputs and the same injected `recorded_at` produce a byte-identical digest                 |

Three of these are worth singling out.

**`assisted_is_never_confident`** is a security-relevant invariant. It must hold for _every_ input, not
just the ones a reviewer thought to check, because a single `Assisted` derivation recorded at
`Moderate` would be a trusted causal claim that no evidence supports. This is the property that
enforces ADR-005 without relying on anyone remembering to gate the write path.

**`scope_never_leaks`** is the one that would otherwise ship silently. A `GROUP BY` with a `WHERE`
clause written by hand is exactly the kind of code that is correct for the fixtures and wrong for the
cases nobody tried, and a cross-scope count is a data-integrity defect that reads as a plausible
number.

**`acyclic_chain_has_one_head`** and **`cyclic_chain_is_rejected`** together pin the resolution
algorithm. The second matters because `Supersedes` chains are assembled from independent writes; a
cycle can only arise from concurrent or replayed ingestion, and must be caught at write rather than
resolved into an infinite walk.

#### Generating values for a private-field domain

`Derivation`, `Relationship`, and `Observation` all have private fields and validating constructors, so
`proptest::Arbitrary` cannot construct them directly. This is intentional and the property tests must
not erode it.

Generators produce **primitive inputs** — non-blank strings, integers, timestamps, `FacetValue`,
`ConfidenceBand`, `RelationKind` — and the test constructs the record through the public validating
constructor. The property therefore exercises the constructor's own rejection paths, which is the
behaviour worth testing. Where a generator needs a structurally valid record, it composes several
primitive generators inside a closure that calls the real constructor rather than a private one.

`proptest-regressions/` is committed for each crate that has property tests, so a shrinking failure
survives a clean checkout.

### Scope discipline

Conformance covers contracts that no single unit test can reach: port substitutability, architectural
layering, schema migration invariants, and repository structure. Property tests cover invariants that
must hold across the whole input space. Per-function behaviour stays in the existing inline
`#[cfg(test)]` suites.

The dividing line: if a clause can be written as `assert_eq!` with concrete values in the owning
module, it belongs there. If it must hold for _all_ values of a type, or across all shapes of a graph,
it is a property test. Duplicating a conformance clause as a property test is only worthwhile when the
enumerated cases cannot cover the space.

## Roadmap Checkpoints

Each follow-up slice is gated on conformance sections and property invariants, not on manual review. A
slice is done when its checkpoint cases pass and no previously-passing case regresses.

| Slice                                                 | New conformance sections                                             | New property invariants                                                  | Gate                                                                                                               |
| ----------------------------------------------------- | -------------------------------------------------------------------- | ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| **1. Derived facets and relationships** (this design) | §1–§10                                                               | all twenty-one                                                           | `cargo nextest run --workspace` green; §8 non-rewrite holds                                                        |
| **2. Decision capture**                               | §1.6 decision construction, §4.6 `Supersedes` from a validity breach | `decision_always_names_an_alternative`, `validity_condition_is_total`    | Every decision names at least one rejected alternative; an assumption with no `invalidates_when` fails §11         |
| **3. Failure attribution**                            | §3.5 competing-hypothesis retention, §9.5 method honesty             | `joint_cause_yields_no_sole_cause`, `causal_band_requires_corroboration` | Joint-cause case must not produce a sole-cause claim; `CausedBy` at `Moderate`+ requires two corroborating links   |
| **4. Session insight reports**                        | §11 full — policy file schemas                                       | `report_is_deterministic_for_fixed_inputs`, `report_never_widens_a_band` | Every report definition is Git-tracked and deterministic; no report embeds an `Assisted` value without its profile |
| **5. Trajectory export**                              | §12 export contract                                                  | `export_is_a_pure_projection`, `export_roundtrip_is_lossless`            | Export leaves every digest in the ledger unchanged                                                                 |

The property invariants are the stricter half of the gate, and deliberately so for slice 3.
`joint_cause_yields_no_sole_cause` has to hold across arbitrary chain shapes — a hand-written fixture
proving it for one graph is exactly the kind of evidence that fails on the second graph.

Two rules make the checkpoints more than a test list:

- **A slice cannot add a contract section without adding the corresponding clause to
  `docs/conformance.md`.** The doc is the index; a section number with no clause is a review finding.
- **Section numbers are stable.** Later slices append (§12, §13); they never renumber. Test names
  embed the section, so renumbering silently invalidates every citation in review discussion.
- **A property failure is never "flaky until it shrinks."** A committed
  `proptest-regressions/` seed that reproduces on a clean checkout is a blocking finding, not a
  tolerated flake.

### Protocol-drift gate

`coursers` and `tracers` track hash-pinned contract surfaces so that an unintentional change to a
public port fails loudly. `coursers` records the governance rule: _"Do not use `protocol drift
--update` merely to silence an unexplained change."_

evidra has no such surface today. Adding `evidra-core/src/ports.rs` and `evidra-core/src/derivation.rs`
as tracked surfaces is deferred to a separate change rather than folded into this slice, since it
introduces a taskit dependency the workspace does not currently have. It is recorded here so the
deferral is visible: once the derived layer lands, these two files are the contract surface that most
needs drift protection.

## Out of Scope

- Failure attribution, root-cause localization, and counterfactual replay.
- Decision records with rationale and rejected alternatives.
- Automatic recomputation of stale derivations.
- Trajectory export to ATIF, ADP, or any RL training format.
- Embeddings, vector search, or semantic clustering beyond deterministic banding.
- Model invocation, prompt construction, or any network call.
- Retention, compaction, or deletion of derived records.
- Multi-user identity, synchronization, or remote transport.
- Granting any control authority to a derivation.

## Follow-Up Slices

Each slice is gated on the conformance sections named here. See
[Roadmap Checkpoints](#roadmap-checkpoints).

### Decision Capture — Slice 2

Adds `DecisionClaim { decision, rationale, alternatives: Vec<RejectedAlternative>,
validity: ValidityCondition }` as a new `DerivationKind`. The distinctive requirement is that
`RejectedAlternative` records what was _not_ chosen and why, which is the only primitive that makes
a decision reviewable later. `ValidityCondition` carries `holds_while` and `invalidates_when` so a
later observation can supersede a decision that no longer applies.

**Checkpoint**: §1.6 and §4.6. A decision with an empty `alternatives` list fails, and an assumption
with no `invalidates_when` fails §11.

### Failure Attribution — Slice 3

Adds attribution as a method-gated derivation over closed sessions, producing `CausedBy` and
`Prevented` relationships with explicit competing hypotheses rather than a single answer. Must model
the joint-cause case from [Reliability Expectations](#reliability-expectations) and must preserve
the failed branch per ADR-002.

**Checkpoint**: §3.5 and §9.5. The joint-cause fixture must not produce a sole-cause claim, and any
`CausedBy` at `Moderate` or above must carry at least two corroborating supporting links.

### Policy Evaluation and Session Insight Reports — Slice 4

Replaces ad hoc `evidra facet` invocations with named, versioned report definitions, and replaces
hand-rolled disposition logic with Rulery, per ADR-005 and ADR-007. See
[Policy Evaluation via Rulery](#policy-evaluation-via-rulery).

Two halves:

- **Policy.** `policies/` becomes a single Rulery package — `rulery.yaml`, `vocabulary.yaml`,
  `actions.yaml`, `rules/`, `scenarios/`, `rulery.lock` — replacing the four empty subdirectories
  ADR-007 mandated. Assumptions, controls, invariants, and accepted risks become declared fact roots.
  Evaluation goes through `ProductionApplication::evaluate_at`.
- **Reports.** Named, versioned report definitions stay deterministic and Git-reviewable; only their
  inputs and definitions are versioned.

**Checkpoint**: §11 in full. Every policy package compiles under `LockMode::Frozen`, every assumption
declares an `invalidated-at` or `invalidated-when`, and the collapsed `policies/` layout passes
conformance.

### Trajectory Export — Slice 5

Adds an ATIF exporter over the observation ledger. Export is a projection and must never write back,
consistent with ADR-003.

**Checkpoint**: §12. The export run must leave every ledger digest unchanged.

## Risk Checklist

- [ ] Breaking API changes: yes. `evidra-core` gains a public domain module and two ports. No
      existing consumer is affected; `evidra-cli` is the only in-tree consumer and is updated in the
      same slice.
- [ ] Serialized compatibility change: yes. `SCHEMA_VERSION` advances from 2 to 3 with an explicit
      `migrate_v2_to_v3`. Existing observation documents are not rewritten, and §8 asserts it.
- [ ] New external dependencies: one new package, `proptest`, added to `[workspace.dependencies]` as a
      `[dev-dependencies]` entry to `evidra-core`, `evidra-engine`, and `evidra-store`. It never enters
      a production dependency path. `Cargo.toml` also gains an `evidra-engine` workspace dependency,
      which it lacks today because the crate exposes no API. Everything else uses `evidra-core` and
      `chrono`; `tempfile` is already declared and used by `evidra-cli`.
- [ ] Feature flag required: no. The derived tables are inert until a derivation is recorded, and
      `crates/*/tests/` requires no gating.
- [ ] Clock injection changed: yes, deliberately. `Derivation::recorded_at` and
      `Relationship::recorded_at` are constructor parameters rather than `Utc::now()` reads, diverging
      from `Observation::record`. Without this, `identical_input_yields_identical_record` cannot be
      tested. See [Public API](#public-api).
- [ ] Property test runtime: yes. Twenty-one properties across six files. Only
      `property_supersedes_chain.rs` and `property_store_invariants.rs` touch SQLite; the other four
      are pure. `evidra-store` is WAL-configured and each disk-touching case opens its own `TempDir`,
      so case counts on those two files should be bounded to keep the suite in the low seconds.
- [ ] Private-field erosion: mitigated, not assumed. Every generated value is a primitive and the
      record is built through its public validating constructor, so the property tests exercise the
      rejection paths instead of bypassing them. A `proptest::Arbitrary` impl on `Derivation` or
      `Relationship` would be a review finding.
- [ ] Circular dependencies: none. `evidra-engine` depends on `evidra-core`; `evidra-store` depends
      on `evidra-core`; neither depends on the other. §10 asserts this from the manifests rather than
      trusting it.
- [ ] AI authority: none. `Assisted` methods are banded to `Speculative`/`Weak` and are barred from
      evaluative dispositions by `may_dispose`.
- [ ] Unbounded growth: yes, accepted. Derived volume is a function of observation volume and this
      slice defines no retention policy. Documented in the crate-level warning.
- [ ] Loss of the append-only guarantee: no. Four additional append-only tables, no `UPDATE` or
      `DELETE` issued, revision expressed as `Supersedes`.
- [ ] Contract drift: yes, accepted for now. `evidra-core/src/ports.rs` and
      `evidra-core/src/derivation.rs` become the workspace's highest-value drift surfaces, but no
      taskit protocol lock exists today. Deferred and recorded under
      [Protocol-drift gate](#protocol-drift-gate) rather than silently omitted.
- [ ] MSRV raised: yes, in Slice 4. Consuming rulery as a path dependency forces
      `rust-version` from 1.85 to **1.98** and the resolver from 2 to 3. This is the single largest
      cost of the rulery route and it applies to the whole workspace, not just the policy crate.
- [ ] Unpublished dependency: yes, in Slice 4. Rulery has no crates.io release, no git tags, and an
      `## [Unreleased]`-only changelog, so the dependency is a path or pinned git reference. Slice 1
      is unaffected because rulery is not a dependency of the derived layer.
- [ ] Disposition vocabulary narrowed: yes, in Slice 4. ADR-005 names five dispositions; rulery's
      `OutcomeKind` has four and is closed. `warn` maps to `Approve` plus a declarative action and
      `quarantine` maps to `Escalate { destination }`. Under `safety_first`, `approve` loses to
      `escalate` at equal priority, so a policy mixing warns with escalations must not use
      `safety_first`.
- [ ] Trusting the rulery specification: no. The shipped code disagrees with `docs/specification.md`
      on `PolicyEvaluator`/`DecisionEvaluator`, on `EvaluationContext` (which does not exist), on
      `PackageStore`'s supertrait and error type, and on JCS hashing. Any assertion in §11 must target
      the shipped behaviour, not the spec text.

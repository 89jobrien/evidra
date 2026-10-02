# Design: Agent Harness Evidence Contract

- Status: Implemented
- Proposed: 2026-09-19
- Approved: 2026-09-19
- Implemented: 2026-09-19

## Goal

Define a redaction-first, harness-neutral contract that turns selected Claude Code, OpenCode, and
Codex lifecycle events into validated source events with source-reported facets and bounded
redacted excerpts, ready for a later migrated observation-ingestion path.

## Approved Approach

Use a normalized JSONL event contract as the local fan-in boundary. Harness integrations redact
content before it reaches any Evidra-owned file or store, include source references and redaction
provenance, and send the same contract through an adapter that implements a core event-source port.

This design is the first of three bounded slices:

1. Define and parse the agent-harness evidence contract in `evidra-core` and
   `evidra-adapters` without constructing or persisting observations.
2. Add observation conversion, idempotent inbox ingestion, the required persisted-format
   migration, and CLI composition in a separate core/store/CLI design.
3. Add revisable derived facets and correlations in a separate core/engine/store design.

## Contents

- [Context Map](#context-map)
- [Crate Ownership](#crate-ownership)
- [Public API](#public-api)
- [Wire Contract](#wire-contract)
- [Validation Rules](#validation-rules)
- [Data Flow](#data-flow)
- [Hexagonal Boundaries](#hexagonal-boundaries)
- [Out of Scope](#out-of-scope)
- [Follow-Up Slices](#follow-up-slices)
- [Risk Checklist](#risk-checklist)

## Context Map

### Files to Modify

| File                                                | Purpose                       | Changes Needed                                                      |
| --------------------------------------------------- | ----------------------------- | ------------------------------------------------------------------- |
| `crates/evidra-core/src/ports.rs`                   | Domain ports                  | Add the pull-based agent-harness event source port                  |
| `crates/evidra-core/src/lib.rs`                     | Public API exports            | Export harness evidence types and source port                       |
| `crates/evidra-core/src/harness.rs`                 | New normalized harness domain | Define validated event, facet, excerpt, and redaction types         |
| `crates/evidra-adapters/src/lib.rs`                 | Adapter exports               | Export the JSONL harness source and error                           |
| `crates/evidra-adapters/src/agent_harness_jsonl.rs` | New inbound adapter           | Parse bounded JSONL records into validated harness events           |
| `crates/evidra-adapters/Cargo.toml`                 | Adapter dependencies          | Add `chrono`, `evidra-core`, `serde`, `serde_json`, and `thiserror` |
| `README.md`                                         | Workspace status              | Mark `evidra-adapters` as an implemented harness event boundary     |
| `AGENTS.md`                                         | Agent architecture guidance   | Replace the reserved-adapter description after implementation       |

### Dependencies

| File                                                   | Relationship                                                                            |
| ------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| `crates/evidra-store/src/sqlite.rs`                    | Serializes complete observations and prevents this slice from persisting harness events |
| `crates/evidra-cli/src/main.rs`                        | Future composition root for inbox ingestion; unchanged in this slice                    |
| `docs/adr/ADR-002-append-only-observations.md`         | Requires imported events to remain immutable                                            |
| `docs/adr/ADR-003-separate-evidence-from-claims.md`    | Requires source facets to remain distinct from derived claims                           |
| `docs/adr/ADR-004-explicit-uncertainty.md`             | Requires later derived facets to expose confidence and scope                            |
| `docs/adr/ADR-005-deterministic-control-boundaries.md` | Prevents harness or model output from directly granting authority                       |

### Test Coverage

| Test Location                                       | Coverage                                                                      |
| --------------------------------------------------- | ----------------------------------------------------------------------------- |
| `crates/evidra-core/src/harness.rs`                 | Validation, facet values, excerpts, and serialization                         |
| `crates/evidra-adapters/src/agent_harness_jsonl.rs` | Valid records, malformed input, redaction requirements, and size/count bounds |

There is no existing coverage for automated source ingestion because `evidra-adapters` is
currently an empty reserved boundary.

### Reference Patterns

| File                                    | Pattern to Follow                                                                     |
| --------------------------------------- | ------------------------------------------------------------------------------------- |
| `crates/evidra-core/src/observation.rs` | Private fields, validating constructors, custom deserialization, and integrity checks |
| `crates/evidra-core/src/ports.rs`       | Associated adapter error satisfying `Error + Send + Sync + 'static`                   |
| `crates/evidra-store/src/sqlite.rs`     | Concrete adapter error and tests colocated with implementation                        |

### Risk

- This slice intentionally cannot construct an `ObservationDraft`. The follow-up ingestion design
  must add the observation kind, exact persisted payload, and explicit migration together.
- JSONL becomes an external compatibility surface and therefore carries a version discriminator.
- Redaction is attested by the producer and validated structurally by Evidra; Evidra cannot prove
  that a producer removed every secret.
- Source-reported facets are evidence from a harness, not trusted conclusions or policy authority.

## Crate Ownership

- **`evidra-core`** owns the normalized agent-harness event model, validation invariants, and the
  source port.
- **`evidra-adapters`** owns bounded JSONL reading, wire decoding, and conversion errors.
- **Unchanged in this slice:** `evidra-store`, `evidra-cli`, and `evidra-engine`.

No new crate is required. The design preserves the existing dependency direction:

```text
evidra-adapters -> evidra-core
```

## Public API

### Agent Harness Event Source Port

```rust
pub trait AgentHarnessEventSource {
    type Error: std::error::Error + Send + Sync + 'static;

    fn next_event(&mut self) -> Result<Option<AgentHarnessEvent>, Self::Error>;
}
```

`Ok(None)` means the source is exhausted. The port is synchronous because the initial source is a
local bounded reader.

### Harness References

```rust
pub struct SourceEventId {
    value: String,
}

impl SourceEventId {
    pub fn new(value: impl Into<String>) -> Result<Self, AgentHarnessEventError>;
    pub fn as_str(&self) -> &str;
}

pub struct HarnessRef {
    name: String,
    version: Option<String>,
}

impl HarnessRef {
    pub fn new(
        name: impl Into<String>,
        version: Option<String>,
    ) -> Result<Self, AgentHarnessEventError>;
    pub fn name(&self) -> &str;
    pub fn version(&self) -> Option<&str>;
}

pub struct HarnessSessionId {
    value: String,
}

impl HarnessSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, AgentHarnessEventError>;
    pub fn as_str(&self) -> &str;
}

pub struct HarnessEventType {
    value: String,
}

impl HarnessEventType {
    pub fn new(value: impl Into<String>) -> Result<Self, AgentHarnessEventError>;
    pub fn as_str(&self) -> &str;
}
```

`SourceEventId`, `HarnessSessionId`, and `HarnessEventType` use transparent Serde representation so
they serialize as JSON strings. Harness and event names remain validated strings rather than closed
enums so new harnesses and lifecycle events do not require a persisted-format migration.

### Source-Reported Facets

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum FacetValue {
    Text(String),
    Integer(i64),
    Boolean(bool),
}

pub struct ObservationFacet {
    name: String,
    value: FacetValue,
}

impl ObservationFacet {
    pub fn new(
        name: impl Into<String>,
        value: FacetValue,
    ) -> Result<Self, AgentHarnessEventError>;
    pub fn name(&self) -> &str;
    pub fn value(&self) -> &FacetValue;
}
```

These facets are immutable metadata reported by the harness, such as `agent.name`, `model.id`,
`tool.name`, `tool.outcome`, `policy.decision`, or `verification.result`. They are not inferred
facets, claims, confidence scores, or controls.

### Redaction and Excerpts

```rust
pub struct RedactionRecord {
    policy: String,
    version: String,
    transformations: Vec<String>,
}

impl RedactionRecord {
    pub fn new(
        policy: impl Into<String>,
        version: impl Into<String>,
        transformations: Vec<String>,
    ) -> Result<Self, AgentHarnessEventError>;
    pub fn policy(&self) -> &str;
    pub fn version(&self) -> &str;
    pub fn transformations(&self) -> &[String];
}

pub struct RedactedExcerpt {
    kind: String,
    text: String,
}

impl RedactedExcerpt {
    pub fn new(
        kind: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self, AgentHarnessEventError>;
    pub fn kind(&self) -> &str;
    pub fn text(&self) -> &str;
}
```

`RedactionRecord` applies to the complete normalized event and is required even when no excerpt
remains after redaction. This v1 contract deliberately omits a digest of pre-redaction content;
`SourceRef` and `SourceEventId` provide traceability without creating an offline guessing target.

### Normalized Harness Event

```rust
pub struct AgentHarnessEventDraft {
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub source: SourceRef,
    pub subject: SubjectRef,
    pub source_event_id: SourceEventId,
    pub harness: HarnessRef,
    pub session_id: HarnessSessionId,
    pub event_type: HarnessEventType,
    pub redaction: RedactionRecord,
    pub excerpts: Vec<RedactedExcerpt>,
    pub facets: Vec<ObservationFacet>,
}

pub struct AgentHarnessEvent {
    // Private validated fields corresponding to AgentHarnessEventDraft.
}

impl AgentHarnessEvent {
    pub fn new(draft: AgentHarnessEventDraft) -> Result<Self, AgentHarnessEventError>;
    pub fn occurred_at(&self) -> chrono::DateTime<chrono::Utc>;
    pub fn source(&self) -> &SourceRef;
    pub fn subject(&self) -> &SubjectRef;
    pub fn source_event_id(&self) -> &SourceEventId;
    pub fn harness(&self) -> &HarnessRef;
    pub fn session_id(&self) -> &HarnessSessionId;
    pub fn event_type(&self) -> &HarnessEventType;
    pub fn redaction(&self) -> &RedactionRecord;
    pub fn excerpts(&self) -> &[RedactedExcerpt];
    pub fn facets(&self) -> &[ObservationFacet];
}

pub enum AgentHarnessEventError {
    BlankField { field: &'static str },
    UnexpectedSourceKind,
    TooManyExcerpts { count: usize, maximum: usize },
    ExcerptTooLarge { bytes: usize, maximum: usize },
    TooManyFacets { count: usize, maximum: usize },
}
```

The public error implements `std::error::Error`, `Send`, and `Sync`. Its display text never includes
source-provided values. Public data types implement redacted `Debug` output plus `Clone`,
`PartialEq`, and `Eq` where their fields permit it, and `Serialize` where they form the normalized
event body. Debug output never includes excerpts, facets, references, identifiers, or redaction
metadata. The transport-level `schema` discriminator remains adapter-owned and is not a field on
`AgentHarnessEvent`.

### JSONL Adapter

```rust
pub struct AgentHarnessJsonlSource<R> {
    // Private reader and line state.
}

impl<R: std::io::BufRead> AgentHarnessJsonlSource<R> {
    pub fn new(reader: R) -> Self;
}

impl<R: std::io::BufRead> AgentHarnessEventSource for AgentHarnessJsonlSource<R> {
    type Error = AgentHarnessAdapterError;

    fn next_event(&mut self) -> Result<Option<AgentHarnessEvent>, Self::Error>;
}

pub enum AgentHarnessAdapterError {
    Read { line: usize },
    RecordTooLarge { line: usize, bytes: usize, maximum: usize },
    BlankInputLimit { line: usize, maximum: usize },
    InvalidJson { line: usize },
    UnsupportedSchema { line: usize },
    InvalidReference { line: usize, source: ObservationError },
    InvalidEvent { line: usize, source: AgentHarnessEventError },
}
```

Adapter error display text includes line numbers and fixed diagnostics but never echoes schema,
field, excerpt, facet, or other source-provided values. Raw I/O and JSON decoder errors are not
retained as error sources because their messages may contain source-provided content.

The adapter enforces these initial bounds. It reads incrementally into a capped buffer and stops at
128 KiB plus one byte, rather than calling an unbounded `BufRead::read_line` and checking afterward.

- At most 128 KiB per JSONL record.
- At most 128 KiB of blank JSONL input skipped by one read call.
- At most eight excerpts per event.
- At most 8 KiB of UTF-8 text per excerpt.
- At most 64 source-reported facets per event.

### Wire Contract

Each line is one independent event:

```json
{
  "schema": "evidra.agent-harness-event/v1",
  "source_event_id": "01J.../tool-17",
  "occurred_at": "2026-09-19T08:00:00Z",
  "source": {
    "kind": "agent-harness",
    "locator": "$HOME/.claude/projects/.../session.jsonl#event-17"
  },
  "subject": {
    "kind": "repository",
    "identifier": "/Users/joe/dev/evidra"
  },
  "harness": {
    "name": "claude-code",
    "version": "1.x"
  },
  "session_id": "01J...",
  "event_type": "tool-completed",
  "redaction": {
    "policy": "obfsck",
    "version": "1",
    "transformations": ["secret-redaction", "excerpt-selection"]
  },
  "excerpts": [
    {
      "kind": "tool-output",
      "text": "cargo nextest reported one failing test"
    }
  ],
  "facets": [
    { "name": "agent.name", "value": "claude-code" },
    { "name": "tool.name", "value": "bash" },
    { "name": "verification.result", "value": "failed" }
  ]
}
```

The example path is a source locator, not copied transcript content. Producers must resolve
environment-relative locators before emitting an event when a consumer requires an absolute
reference.

## Validation Rules

- Every reference, facet name, excerpt kind, redaction field, and transformation is non-blank.
- The future idempotency key is `(harness.name, session_id, source_event_id)`; duplicate handling is
  deferred to the ingestion slice.
- `source.kind` is `agent-harness` for this adapter.
- The event carries a redaction record even when `excerpts` is empty.
- Blank JSONL lines are skipped. Unknown fields are rejected, and unknown schema versions produce
  `AgentHarnessAdapterError::UnsupportedSchema`.
- Record scanning is capped before searching for a newline, and excessive blank input terminates
  the source with `AgentHarnessAdapterError::BlankInputLimit`.
- The adapter decodes the schema discriminator before the full event so unsupported versions cannot
  be interpreted under the v1 domain contract.
- Excerpts are treated as already redacted. Evidra validates structure and bounds but does not
  claim to prove semantic redaction completeness.
- Facet values are scalar and source-reported. Nested documents remain in future versioned payload
  schemas rather than being smuggled through facets.

## Data Flow

1. A Claude Code hook, OpenCode plugin, or Codex session integration selects an operational event.
2. The producer applies secret/PII redaction and excerpt selection before writing the Evidra JSONL
   contract or inbox record; a harness may separately retain its own native transcript.
3. `AgentHarnessJsonlSource` bounds and decodes one record, validates its schema, and constructs an
   `AgentHarnessEvent`.
4. A later migrated ingestion slice converts the validated event into an immutable observation and
   appends it through `ObservationStore`.

## Hexagonal Boundaries

- **Inbound port:** `evidra_core::AgentHarnessEventSource`.
- **Inbound adapter:** `evidra_adapters::AgentHarnessJsonlSource<R>`.
- **Future sink port:** the existing `evidra_core::ObservationStore`.

Harness-native hooks and plugins are producers of the versioned wire contract; they do not gain
direct access to SQLite or Evidra domain internals.

## Integration Events

The v1 contract supports these recommended `event_type` values without closing the vocabulary:

- `session-started`
- `session-ended`
- `tool-requested`
- `tool-completed`
- `permission-decided`
- `course-corrected`
- `verification-completed`
- `handoff-created`

Claude Code, OpenCode, and Codex integrations may emit only the events their native lifecycle APIs
can support faithfully. Missing events are absence of evidence, not evidence that an action did not
occur.

## Out of Scope

- Reading or storing full transcripts.
- Writing unredacted content to the inbox, database, logs, or diagnostics.
- Idempotent inbox checkpointing and duplicate suppression.
- Observation conversion, persisted payload shape, SQLite migration, or persistence.
- CLI commands, hook installation, or harness-specific configuration generation.
- Derived facets, correlation, claims, confidence scoring, controls, or enforcement.
- Remote transport, synchronization, or multi-user identity.
- AI-generated authority decisions.

## Follow-Up Slices

### Inbox Ingestion

The next design will cover `evidra-core`, `evidra-store`, and `evidra-cli`: the
`AgentHarnessEvent`-to-`ObservationDraft` conversion, exact persisted payload, transformed
provenance construction, repository-local inbox discovery, source-event idempotency, append
transactions, processed or quarantined records, CLI rendering, and the required persisted-format
migration. It will consume `evidra-adapters` without changing that crate.

### Facet Derivation

A later design will cover `evidra-core`, `evidra-engine`, and `evidra-store`: revisable derived
facets with evidence links, scope, freshness, contradiction, confidence, and separate persistence
from immutable observations.

## Risk Checklist

- [x] Breaking API changes: no existing adapter consumers; this slice does not extend
      `ObservationKind`.
- [x] Serialized compatibility change: the new JSONL contract is versioned, but persisted
      observation JSON remains unchanged in this slice.
- [x] New external dependencies: adapter dependencies already exist in the workspace and require no
      new third-party package.
- [x] Feature flag required: no; local JSONL is the initial harness boundary.
- [x] Circular dependencies: none; adapters depend only on core.
- [x] AI authority: none; harness events and facets are attributed evidence only.

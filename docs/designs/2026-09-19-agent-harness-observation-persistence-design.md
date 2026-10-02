# Design: Agent Harness Observation Persistence

- Status: Implemented
- Proposed: 2026-09-19
- Approved: 2026-09-19
- Implemented: 2026-09-19

## Goal

Convert validated `AgentHarnessEvent` values into immutable observations and persist at most one
matching event per harness/session/source-event identity through an explicit SQLite v1-to-v2
migration.

## Approved Approach

Implement the persistence half of the selected atomic-event-file workflow first: core owns the
typed harness observation, semantic digest, and identity; SQLite owns transactional
observation-and-receipt append and migration; inbox claiming remains a separate adapters/CLI slice.

This design splits and supersedes the combined ingestion follow-up described in
`2026-09-19-agent-harness-evidence-contract-design.md`. The broader workflow decisions remain:

- Producers publish one redacted event per atomically renamed inbox file.
- Successful files are deleted after database commit.
- Invalid or conflicting files are quarantined while independent files continue.
- Database and filesystem failures stop the batch for retry.
- Existing databases migrate only when the operator runs `evidra init`.

## Contents

- [Context Map](#context-map)
- [Crate Ownership](#crate-ownership)
- [Public API](#public-api)
- [Persisted Payload](#persisted-payload)
- [Semantic Digest](#semantic-digest)
- [SQLite Schema v2](#sqlite-schema-v2)
- [Migration](#migration)
- [Transaction Semantics](#transaction-semantics)
- [Validation and Threat Model](#validation-and-threat-model)
- [Data Flow](#data-flow)
- [Out of Scope](#out-of-scope)
- [Risk Checklist](#risk-checklist)

## Context Map

### Files to Modify

| File                                    | Purpose                                  | Changes Needed                                                                     |
| --------------------------------------- | ---------------------------------------- | ---------------------------------------------------------------------------------- |
| `crates/evidra-core/src/observation.rs` | Observation envelope and store port data | Add harness kind, transformed provenance, and redacted observation debug/errors    |
| `crates/evidra-core/src/harness.rs`     | Harness evidence domain                  | Add identity, semantic digest, exact payload, and immutable harness observation    |
| `crates/evidra-core/src/ports.rs`       | Domain persistence boundary              | Extend `ObservationStore` with idempotent harness append                           |
| `crates/evidra-core/src/lib.rs`         | Public API                               | Export harness persistence types                                                   |
| `crates/evidra-store/src/sqlite.rs`     | SQLite adapter                           | Add schema v2 migration, structural validation, receipts, and transactional append |
| `README.md`                             | User guidance                            | Document explicit migration, backup guidance, and persistence boundary             |
| `AGENTS.md`                             | Architecture guidance                    | Record schema v2 and typed harness persistence rules                               |

### Dependencies

```text
evidra-store -> evidra-core
evidra-cli   -> evidra-store
evidra-adapters -> evidra-core
```

`evidra-cli` already calls `SqliteObservationStore::initialize` from `evidra init`; no CLI source
change is required. `evidra-adapters` remains unchanged.

### Existing and Required Test Coverage

| Area             | Required Addition                                                                                                                     |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| Core harness     | Exact payload including `version: null`, semantic digest stability, identity conflict inputs, redacted debug/errors                   |
| Core observation | Harness kind, transformed provenance validation, redacted draft/observation debug                                                     |
| Store migration  | v1 validation, v2 structural validation, rollback, unchanged observation bytes, explicit migration-required error, idempotent v2 init |
| Store append     | recorded, duplicate, identity conflict, immediate transaction behavior, generic-port rejection, atomic rollback                       |
| Store controls   | receipt update/delete/replace/orphan/mismatched-identity probes, foreign-key check, private permissions                               |
| CLI integration  | Real v1 fixture upgraded by `evidra init`; ordinary commands reject v1 with actionable guidance                                       |

The CLI migration test belongs in `crates/evidra-cli/tests/cli.rs` even though production CLI code
does not change.

### Risk

- `ObservationKind::AgentHarnessEvent` breaks exhaustive downstream matches and expands persisted
  JSON before `1.0.0`.
- Schema v2 must not rewrite existing append-only observations.
- Identity reuse with different normalized content must become `IdentityConflict`, never
  `Duplicate`.
- Migration DDL, validation, probes, and `user_version` update must commit or roll back together.
- Old binaries cannot read v2; migration has no automatic downgrade.

## Crate Ownership

- **`evidra-core`** owns event-to-observation conversion, the exact payload, semantic digest,
  idempotency identity, append outcome, and the existing `ObservationStore` port extension.
- **`evidra-store`** owns SQLite schema v2, v1-to-v2 migration, validation/probes, permissions, and
  the transaction implementing that port.
- **Unchanged production crates:** `evidra-adapters`, `evidra-cli`, and `evidra-engine`.

No new crate is required. `rusqlite`, `serde_json`, and `sha2` are already workspace dependencies.

## Public API

### Observation Kind

```rust
pub enum ObservationKind {
    ManualIntervention,
    AgentHarnessEvent,
}
```

The persisted spelling is `agent-harness-event`.

### Transformed Provenance

```rust
impl Provenance {
    pub fn transformed(
        collector: impl Into<String>,
        transformations: Vec<String>,
    ) -> Result<Self, ObservationError>;
}
```

The constructor rejects a blank collector or transformation. Harness observation conversion puts
only Evidra's `agent-harness-observation/v1` conversion in provenance. Producer-attested redaction
steps remain attributed inside the payload's `redaction` object.

### Sanitized Observation Errors

```rust
pub enum ObservationError {
    BlankField { field: &'static str },
    IntegritySerialization,
}
```

`IntegritySerialization` becomes a fixed, source-free diagnostic. Internal Serde failures map
explicitly to it rather than using `#[from] serde_json::Error`, so errors wrapped by
`AgentHarnessObservationError::Observation` cannot reveal source-controlled content.

### Harness Identity

```rust
pub struct AgentHarnessEventIdentity {
    harness: String,
    session_id: String,
    source_event_id: String,
}

impl AgentHarnessEventIdentity {
    pub fn new(
        harness: impl Into<String>,
        session_id: impl Into<String>,
        source_event_id: impl Into<String>,
    ) -> Result<Self, AgentHarnessObservationError>;

    pub fn harness(&self) -> &str;
    pub fn session_id(&self) -> &str;
    pub fn source_event_id(&self) -> &str;
}

impl From<&AgentHarnessEvent> for AgentHarnessEventIdentity {
    fn from(event: &AgentHarnessEvent) -> Self;
}
```

The type implements `Clone`, `PartialEq`, `Eq`, and `Hash`; custom `Debug` reveals no values.

### Immutable Harness Observation

```rust
pub struct AgentHarnessObservation {
    identity: AgentHarnessEventIdentity,
    event_digest: String,
    observation: Observation,
}

impl AgentHarnessObservation {
    pub fn record(
        event: AgentHarnessEvent,
        collector: impl Into<String>,
    ) -> Result<Self, AgentHarnessObservationError>;

    pub fn identity(&self) -> &AgentHarnessEventIdentity;
    pub fn event_digest(&self) -> &str;
    pub fn observation(&self) -> &Observation;

    pub fn verify_receipt(
        identity: &AgentHarnessEventIdentity,
        event_digest: &str,
        observation: &Observation,
    ) -> Result<bool, AgentHarnessObservationError>;
}

pub enum AgentHarnessObservationError {
    BlankIdentityField { field: &'static str },
    InvalidEventDigest,
    PayloadSerialization,
    InvalidPersistedPayload,
    Observation(ObservationError),
}
```

The type implements `Clone` and `PartialEq`; custom `Debug` reveals no identity, observation,
excerpt, facet, reference, digest, or payload content. Public serialization diagnostics are fixed
and do not retain raw Serde sources.

### Append Outcome and Existing Port Extension

```rust
pub enum AgentHarnessAppendOutcome {
    Recorded,
    Duplicate,
    IdentityConflict,
}

pub trait ObservationStore {
    type Error: std::error::Error + Send + Sync + 'static;

    fn append(&mut self, observation: &Observation) -> Result<(), Self::Error>;
    fn list(&self, limit: usize) -> Result<Vec<Observation>, Self::Error>;

    fn append_harness_observation(
        &mut self,
        value: &AgentHarnessObservation,
    ) -> Result<AgentHarnessAppendOutcome, Self::Error>;
}
```

Keeping one persistence port preserves ADR-007's SQLite boundary. The outcome implements `Debug`,
`Clone`, `Copy`, `PartialEq`, and `Eq`. Every compliant store must reject
`ObservationKind::AgentHarnessEvent` through generic `append`; the specialized method is the only
valid persistence path for that kind.

## Persisted Payload

Core uses a dedicated private persisted-payload DTO and exact-shape tests. Envelope fields
`occurred_at`, `source`, and `subject` remain outside the payload:

```json
{
  "schema": "evidra.agent-harness-observation/v1",
  "source_event_id": "event-17",
  "harness": {
    "name": "claude-code",
    "version": null
  },
  "session_id": "session-1",
  "event_type": "tool-completed",
  "redaction": {
    "policy": "obfsck",
    "version": "1",
    "transformations": ["secret-redaction", "excerpt-selection"]
  },
  "excerpts": [],
  "facets": [{ "name": "tool.name", "value": "bash" }]
}
```

`harness.version` is always present and is either a string or JSON `null`. Empty excerpt and facet
arrays are present. Unknown or missing fields make receipt verification fail with a fixed
diagnostic.

## Semantic Digest

`event_digest` is lowercase SHA-256 over deterministic JSON for this post-redaction semantic
envelope:

```text
schema = evidra.agent-harness-event-digest/v1
occurred_at
source
subject
persisted_payload
```

The digest excludes generated observation ID, `observed_at`, provenance, and observation integrity,
so retries of the same normalized event are stable. It never hashes pre-redaction content. Core can
reconstruct it from a persisted observation, which lets `verify_receipt` validate identity fields,
kind, payload schema/shape, and digest without trusting receipt columns.

## SQLite Schema v2

Schema v2 retains `observations` unchanged and adds:

```sql
CREATE TABLE agent_harness_receipts (
    harness TEXT NOT NULL,
    session_id TEXT NOT NULL,
    source_event_id TEXT NOT NULL,
    event_digest TEXT NOT NULL,
    observation_id TEXT NOT NULL UNIQUE,
    PRIMARY KEY (harness, session_id, source_event_id),
    FOREIGN KEY (observation_id) REFERENCES observations(id)
);
```

Receipt update, delete, and duplicate-insert triggers block mutation and `INSERT OR REPLACE`.

## Migration

### New Database

Initialization creates schema v2 inside one immediate transaction, validates all definitions and
behavioral probes, sets `application_id` and `user_version = 2`, then commits. Identity/version are
not advanced before validation succeeds.

### Existing v1 Database

`SqliteObservationStore::initialize`:

1. Rejects symbolic-link database/sidecar paths.
2. Restricts the repository store directory to owner-only and database/WAL/SHM files to
   owner-read/write on Unix; non-Unix behavior remains platform best effort.
3. Validates application ID, v1 table/index/trigger definitions, observation metadata, and
   effective append-only controls before mutation.
4. Starts an immediate transaction.
5. Creates receipt objects.
6. Validates receipt columns, composite primary key, unique observation ID, foreign key target,
   trigger definitions, `PRAGMA foreign_keys = 1`, `PRAGMA foreign_key_check`, and behavioral
   probes for update, delete, replace, orphan, and mismatched identity.
7. Confirms all preexisting observation document bytes are unchanged.
8. Sets `user_version = 2` and commits only after every check passes.

Any failure rolls back DDL and version change, leaving a retryable v1 database. Re-running
`evidra init` against valid v2 is non-mutating and idempotent.

### Open and Compatibility

- `SqliteObservationStore::open` accepts only v2.
- Valid v1 returns `StoreError::MigrationRequired` with fixed guidance to back up the database and
  run `evidra init`.
- Unknown versions/application IDs remain `StoreError::UnexpectedDatabase`.
- There is no automatic downgrade. Operators needing old-binary rollback must create a WAL-safe
  SQLite backup before migration; README documents this requirement. Transaction rollback protects
  failed migrations but not intentional downgrade after success.

## Transaction Semantics

`append_harness_observation` uses an immediate transaction and the configured SQLite busy timeout:

1. Verify observation integrity, kind, identity/payload consistency, and semantic digest.
2. Query the receipt composite key while holding the write reservation.
3. Equal stored and incoming digests return `Duplicate` without mutation.
4. Unequal digests return `IdentityConflict` without mutation.
5. Missing receipt inserts the immutable observation and receipt.
6. Validate the inserted receipt relationship, commit, and return `Recorded`.

Uniqueness violations are re-read under the transaction and mapped to `Duplicate` or
`IdentityConflict`; lock timeout and other SQLite failures remain store errors that stop the later
batch. Any error rolls back both inserts.

Generic `append` returns `StoreError::HarnessObservationRequiresReceipt` for harness observations.

## Store Error Changes

```rust
pub enum StoreError {
    MigrationRequired {
        path: std::path::PathBuf,
        current_version: i32,
        target_version: i32,
    },
    HarnessObservationRequiresReceipt,
    InvalidHarnessReceipt,
    Serialization,
}
```

`Serialization` becomes a fixed diagnostic without a raw Serde source. Harness receipt errors and
their complete source chains never render persisted content, identity values, digests, excerpts,
facets, or references. Raw SQLite errors remain adapter diagnostics but SQL statements never embed
event values.

## Validation and Threat Model

- On open/initialize, every receipt must reference an existing `AgentHarnessEvent` observation and
  match its payload identity and semantic digest.
- Structural checks inspect `table_info`, `index_list`/`index_info`, `foreign_key_list`, and required
  trigger SQL in addition to behavioral probes.
- Direct writers able to alter/drop SQLite schema objects or replace the database file are outside
  the attacker threat model. Evidra detects ordinary uncoordinated changes on the next open but
  does not provide signatures against an attacker controlling both database and code.
- Semantic redaction remains producer-attested. This accepted boundary is recorded in the payload;
  a pre-persistence secret scanner and quarantine policy belong to the inbox slice.
- Debug and error tests use sentinel values and inspect complete source chains.

## Data Flow

1. `AgentHarnessObservation::record` receives a validated post-redaction event and collector.
2. Core derives identity, exact payload, semantic digest, Evidra-only provenance, and immutable
   observation.
3. `SqliteObservationStore` starts an immediate transaction and compares any existing receipt.
4. The store returns `Duplicate` or `IdentityConflict` without mutation, or commits observation and
   receipt as `Recorded`.
5. The later inbox slice deletes recorded/duplicate files and quarantines identity conflicts.

## Hexagonal Boundaries

- **Domain:** `AgentHarnessObservation`, `AgentHarnessEventIdentity`, semantic digest rules.
- **Port:** extended `evidra_core::ObservationStore`.
- **Adapter:** `evidra_store::SqliteObservationStore`.
- **Migration entry:** existing `SqliteObservationStore::initialize`, invoked by `evidra init`.

## Out of Scope

- Inbox directory creation, atomic claiming, retry recovery, deletion, or quarantine.
- `evidra ingest` and batch rendering.
- Automatic migration, automatic backup, or downgrade.
- Harness hook/plugin installation.
- Semantic secret detection beyond producer attestation.
- Derived facets, correlation, claims, confidence, controls, or enforcement.
- Remote synchronization or multi-user ingestion.

## Follow-Up Slice

The next design covers `evidra-adapters` and `evidra-cli`: repository-local atomic event files,
processing recovery, stable ordering, pre-persistence secret scanning policy, quarantine
diagnostics, deletion after `Recorded` or `Duplicate`, quarantine after `IdentityConflict`, batch
stop behavior for filesystem/store failures, and summary rendering.

## Risk Checklist

- [x] Breaking API change: yes; `ObservationKind`, `ObservationStore`, `ObservationError`, and
      `StoreError` change before `1.0.0`.
- [x] Serialized compatibility change: yes; guarded by schema v2 and explicit migration.
- [x] Existing persisted evidence rewritten: no; tests compare document bytes.
- [x] New external dependency: no.
- [x] Feature flag required: no.
- [x] Circular dependencies: none.
- [x] AI authority: none; this slice persists attributed evidence only.

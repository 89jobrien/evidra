# Design: Observation Ledger Vertical Slice

- Status: Implemented
- Implemented: 2026-09-18

## Contents

- [Goal](#goal) and [approved approach](#approved-approach)
- [Context map](#context-map) and [crate ownership](#crate-ownership)
- [Public API](#public-api) and [CLI surface](#cli-surface)
- [Data flow](#data-flow) and [hexagonal boundaries](#hexagonal-boundaries)
- [Out of scope](#out-of-scope), [decisions](#decisions), and [risks](#risk-checklist)

## Goal

Create Evidra's five-crate workspace and deliver the smallest useful local-first loop: initialize
an append-only SQLite ledger, record a manual observation, and list recorded observations.

## Approved Approach

Build a thin vertical slice through the domain, SQLite adapter, and CLI while scaffolding the
engine and source-adapter crates without speculative APIs.

## Context Map

At design time, the target repository did not exist. The nearest workspace references were `taskit` for
hexagonal crate boundaries and `crux` for Rust 2024 workspace metadata, ULIDs, SQLite, and
Serde conventions.

### Files to Create

| Area              | Purpose                                                               |
| ----------------- | --------------------------------------------------------------------- |
| Workspace root    | Cargo workspace metadata, licenses, repository guidance, and README   |
| `evidra-core`     | Observation domain types and the storage port                         |
| `evidra-store`    | SQLite implementation of the observation storage port                 |
| `evidra-cli`      | Composition root and `init`, `note`, and `observation list` commands  |
| `evidra-engine`   | Reserved domain-service boundary with crate documentation only        |
| `evidra-adapters` | Reserved source-normalization boundary with crate documentation only  |
| `docs/adr`        | Initial architectural decisions supplied in the product specification |
| `policies`        | Version-controlled policy directory structure                         |

### Dependencies

```text
evidra-cli -> evidra-store -> evidra-core
          \-> evidra-core
evidra-engine      (reserved boundary)
evidra-adapters    (reserved boundary)
```

No dependency points from the domain toward SQLite, CLI parsing, filesystem discovery, or
application rendering.

### Test Coverage

- `evidra-core`: observation construction, identity, integrity, and serialization.
- `evidra-store`: append/list round-trip and duplicate-ID rejection using a temporary database.
- `evidra-cli`: repository initialization and note/list command integration tests.

### Risk

- This creates a new public API that may evolve before version `1.0.0`.
- Observation serialization becomes persisted data and therefore requires future migrations.
- SQLite is a new external dependency and remains isolated in `evidra-store`.
- The first CLI intentionally discovers only the current repository's `.evidra` directory.

## Crate Ownership

- **`evidra-core`** owns immutable observations and the `ObservationStore` port.
- **`evidra-store`** owns SQLite schema creation, transactions, and row mapping.
- **`evidra-cli`** owns argument parsing, repository-local path selection, and output formatting.
- **`evidra-engine`** is created for future correlation and inference logic but receives no
  speculative public API in this slice.
- **`evidra-adapters`** is created for future Git, Cargo, CI, filesystem, and manual source
  normalization but receives no speculative public API in this slice.

## Public API

### Domain Types

```rust
pub struct ObservationId(ulid::Ulid);

pub enum ObservationKind {
    ManualIntervention,
}

pub struct SourceRef {
    kind: String,
    locator: String,
}

pub struct SubjectRef {
    kind: String,
    identifier: String,
}

pub struct Provenance {
    collector: String,
    transformations: Vec<String>,
}

pub struct IntegrityRecord {
    algorithm: String,
    digest: String,
}

pub struct ObservationDraft {
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub source: SourceRef,
    pub kind: ObservationKind,
    pub subject: SubjectRef,
    pub payload: serde_json::Value,
    pub provenance: Provenance,
}

pub struct Observation {
    id: ObservationId,
    occurred_at: chrono::DateTime<chrono::Utc>,
    observed_at: chrono::DateTime<chrono::Utc>,
    source: SourceRef,
    kind: ObservationKind,
    subject: SubjectRef,
    payload: serde_json::Value,
    provenance: Provenance,
    integrity: IntegrityRecord,
}
```

### Domain Functions

```rust
impl Observation {
    pub fn record(draft: ObservationDraft) -> Result<Self, ObservationError>;
    pub fn id(&self) -> &ObservationId;
    pub fn occurred_at(&self) -> chrono::DateTime<chrono::Utc>;
    pub fn observed_at(&self) -> chrono::DateTime<chrono::Utc>;
    pub fn source(&self) -> &SourceRef;
    pub fn kind(&self) -> &ObservationKind;
    pub fn subject(&self) -> &SubjectRef;
    pub fn payload(&self) -> &serde_json::Value;
    pub fn provenance(&self) -> &Provenance;
    pub fn integrity(&self) -> &IntegrityRecord;
    pub fn verify_integrity(&self) -> Result<bool, ObservationError>;
}

impl IntegrityRecord {
    pub fn algorithm(&self) -> &str;
    pub fn digest(&self) -> &str;
}

impl SourceRef {
    pub fn new(kind: impl Into<String>, locator: impl Into<String>) -> Result<Self, ObservationError>;
    pub fn kind(&self) -> &str;
    pub fn locator(&self) -> &str;
}

impl SubjectRef {
    pub fn new(
        kind: impl Into<String>,
        identifier: impl Into<String>,
    ) -> Result<Self, ObservationError>;
    pub fn kind(&self) -> &str;
    pub fn identifier(&self) -> &str;
}

impl Provenance {
    pub fn direct(collector: impl Into<String>) -> Result<Self, ObservationError>;
    pub fn collector(&self) -> &str;
    pub fn transformations(&self) -> &[String];
}
```

### Port

```rust
pub trait ObservationStore {
    type Error: std::error::Error + Send + Sync + 'static;

    fn append(&mut self, observation: &Observation) -> Result<(), Self::Error>;
    fn list(&self, limit: usize) -> Result<Vec<Observation>, Self::Error>;
}
```

### SQLite Adapter

```rust
pub struct SqliteObservationStore {
    connection: rusqlite::Connection,
}

impl SqliteObservationStore {
    pub fn initialize(path: &std::path::Path) -> Result<Self, StoreError>;
    pub fn open(path: &std::path::Path) -> Result<Self, StoreError>;
}

impl ObservationStore for SqliteObservationStore {
    type Error = StoreError;

    fn append(&mut self, observation: &Observation) -> Result<(), Self::Error>;
    fn list(&self, limit: usize) -> Result<Vec<Observation>, Self::Error>;
}
```

## CLI Surface

```text
evidra init
evidra note --summary <TEXT>
evidra observation list [--limit <COUNT>] [--json]
```

- `init` creates `.evidra/evidra.db`, applies schema objects in a transaction, and leaves existing
  data intact. File creation and SQLite identity pragmas occur outside that schema transaction.
- `note` records a `ManualIntervention` observation for the current repository.
- `observation list` returns newest observations first as a compact table or JSON.

## Data Flow

1. The CLI converts validated arguments and current repository context into an
   `ObservationDraft`.
2. `evidra-core` assigns identity and timestamps, computes integrity metadata, and returns an
   immutable `Observation`.
3. `evidra-store` appends the serialized observation to SQLite in a transaction.
4. The list command reads observations through the same port and renders table or JSON output.

## Hexagonal Boundaries

- **Port:** `evidra_core::ObservationStore`.
- **Adapter:** `evidra_store::SqliteObservationStore`.
- **Composition root:** `evidra-cli`, which is the only crate aware of CLI and SQLite together.

## Out of Scope

- Git, Cargo, CI, filesystem, and agent importers.
- Correlation, candidate inference, assumptions, invariants, controls, and entropy scoring.
- YAML policy evaluation.
- Enforcement, autonomous writes, synchronization, and web UI.
- Configuration discovery above the current directory.
- crates.io publication or namespace reservation.

## Decisions

- Rust edition 2024 with a workspace MSRV recorded in the root manifest.
- SQLite uses `rusqlite` with the bundled feature for a reproducible local setup.
- Observation IDs use ULIDs for sortable, locally generated identifiers.
- Integrity records use SHA-256 over a deterministic serialized observation envelope.
- Licenses are `MIT OR Apache-2.0`.
- All persisted observations are append-only; duplicate IDs fail instead of replacing rows.

## Risk Checklist

- [x] Breaking API changes: no existing consumers.
- [x] New external dependencies: `chrono`, `serde`, `serde_json`, `sha2`, `ulid`, `rusqlite`,
      `clap`, `anyhow`, `thiserror`, and test-only CLI/tempfile helpers.
- [x] Feature flag required: no; SQLite is fundamental to this first slice.
- [x] Circular dependencies: none.
- [x] AI authority: none; the slice is fully deterministic.

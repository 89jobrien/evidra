# Design: Agent Harness Inbox Ingestion

- Status: Implemented
- Proposed: 2026-09-19
- Approved: 2026-09-19
- Implemented: 2026-09-19

## Goal

Add deterministic, process-crash-recoverable inbox ingestion with exclusive claiming, fixed-code
quarantine, and text/JSON `evidra ingest` summaries.

## Selected Approach

Use one redacted JSON event per atomically published file, a process-wide advisory ingest lock,
same-filesystem no-replace claim transitions, core-owned ingestion policy, and a filesystem adapter
behind a core port.

This design extends:

- `2026-09-19-agent-harness-evidence-contract-design.md`
- `2026-09-19-agent-harness-observation-persistence-design.md`

## Contents

- [Context Map](#context-map)
- [Trust and Durability](#trust-and-durability)
- [Directory Contract](#directory-contract)
- [Core API](#core-api)
- [Filesystem Adapter](#filesystem-adapter)
- [CLI Surface](#cli-surface)
- [Failure and Output Semantics](#failure-and-output-semantics)
- [Crash Matrix](#crash-matrix)
- [Out of Scope](#out-of-scope)
- [Risk Checklist](#risk-checklist)

## Context Map

### Files to Modify

| File                                  | Purpose                 | Changes Needed                                                             |
| ------------------------------------- | ----------------------- | -------------------------------------------------------------------------- |
| `crates/evidra-core/src/ingest.rs`    | New application service | Inbox port, quarantine policy, summary, source-safe errors, orchestration  |
| `crates/evidra-core/src/lib.rs`       | Public API              | Export ingestion service/types                                             |
| `crates/evidra-adapters/src/inbox.rs` | New filesystem adapter  | Lock, secure scan snapshot, claim, decode, completion, quarantine/recovery |
| `crates/evidra-adapters/src/lib.rs`   | Adapter exports         | Export concrete inbox adapter/error                                        |
| `crates/evidra-adapters/Cargo.toml`   | Adapter dependencies    | Add `rustix` with `std,fs,process` and `ulid`                              |
| `crates/evidra-cli/src/main.rs`       | Composition/rendering   | Add `ingest [--json]`, construct adapters, render result/error             |
| `crates/evidra-cli/Cargo.toml`        | CLI dependencies        | Add `evidra-adapters` and `serde`                                          |
| `crates/evidra-cli/tests/cli.rs`      | End-to-end tests        | Claim/recovery/delete/quarantine/conflict/failure/text/JSON behavior       |
| `Cargo.toml`                          | Workspace dependencies  | Add adapters path entry and pinned rustix with `std,fs,process`            |
| `.github/workflows/ci.yml`            | Platform verification   | Run Rust 1.85 inbox tests on macOS and Linux                               |
| `README.md`                           | User workflow           | Document producer contract and command                                     |
| `AGENTS.md`                           | Agent guidance          | Record lifecycle and trust invariants                                      |

### Dependencies

```text
evidra-cli -> evidra-adapters -> evidra-core
          \-> evidra-store    -> evidra-core
```

`evidra-store` and `evidra-engine` remain unchanged. Effective-UID validation uses
`rustix::process::geteuid`.

### Required Coverage

- Core use case: every decoder/persistence outcome and every operational stop point.
- Filesystem: lock contention, no-clobber collision, processing recovery, unsafe entries,
  filename grammar, hardlinks, ownership/mode, device mismatch, bounded snapshots, sidecar repair.
- CLI: uninitialized/v1 refusal, success summaries, quarantine continuation, partial-work failure,
  stdout/stderr contracts, and crash-retry duplicate cleanup.

### Risk

- Filesystem state and SQLite cannot share one transaction; the state machine must be idempotent.
- Same-user producers can submit forged or insufficiently redacted evidence.
- Source filenames, paths, event content, identities, digests, and raw errors must never enter CLI
  output, quarantine reasons, or public error chains.
- Platform filesystem semantics differ; unsupported no-replace/no-follow behavior must fail closed.

## Crate Ownership

- **`evidra-core`** owns deterministic ingestion policy and the external inbox port.
- **`evidra-adapters`** owns filesystem and JSONL implementations of that port.
- **`evidra-cli`** only composes the inbox/store and renders core results.
- **`evidra-store`** is consumed unchanged through `ObservationStore`.

## Trust and Durability

### Producer Trust

Inbox producers are cooperative processes running as the repository owner. They are not
authenticated, and harness names are attributed claims rather than verified identities. The
adapter rejects unexpected owner, mode, link count, type, name, and device metadata, but cannot
prevent a same-user process that retains a writable descriptor from modifying a file.

Semantic secret scanning is explicitly deferred, superseding the earlier follow-up wording. The
reason is that no detection policy, allowlist, severity threshold, or false-positive workflow has
been approved. This slice accepts irreversible persistence risk from producer-attested redaction;
untrusted producers must not receive inbox write access. A separate governance design is required
before widening that trust boundary.

### Crash Scope

Recovery guarantees cover process termination and command interruption. Power-loss durability is
out of scope; producers and Evidra do not promise directory-entry durability without explicit
filesystem synchronization. Atomic rename/no-replace prevents runtime clobber, while SQLite
provides its existing transactional durability.

## Directory Contract

```text
.evidra/
  ingest.lock
  inbox/        # producer *.tmp and published *.json
  processing/   # Evidra <ULID>.json claims and <ULID>.reason.tmp transitions
  quarantine/   # <ULID>.json plus <ULID>.reason
```

- Producers write a temporary file, close it, then atomically rename it to a ready `.json` file.
- Each ready file contains exactly one `evidra.agent-harness-event/v1` JSON record.
- Ready basenames must be UTF-8, 6–128 bytes, and match
  `[A-Za-z0-9][A-Za-z0-9._-]*\.json`; control characters and reserved dot names are invalid.
- `.tmp` files are ignored but counted toward scan bounds.
- Every other entry, symlink, directory, hardlink (`nlink != 1`), wrong-owner file, or group/world
  writable file causes a fixed operational failure; it is never followed or quarantined.
- Inbox, processing, quarantine, and lock file must share the state directory device.
- `.evidra` must already exist because schema v2 is opened before inbox initialization; direct
  adapter use against a missing state directory returns `CreateDirectory` without creating it.
  `inbox`, `processing`, and `quarantine` are created relative to the validated state handle with
  mode `0700`. All four directories must be owned by the current UID, mode `0700`, and on the
  expected device. The lock is current-owner, singly linked, regular, same-device, and mode `0600`.
- Ready JSON and temporary files must be current-owner, singly linked, regular, same-device, and
  exact mode `0600`; claims preserve that mode across rename. All processing, quarantine, reason,
  and recognized reason-temp files must also be exact mode `0600`.

All child operations are relative to opened state/directory handles. Unix uses no-follow opens,
post-open metadata validation, and atomic no-replace rename. Unsupported platforms fail closed
rather than emulate a clobber-prone transition.

## Core API

### Quarantine Policy

```rust
pub enum QuarantineReason {
    InvalidJson,
    UnsupportedSchema,
    InvalidReference,
    InvalidEvent,
    RecordTooLarge,
    BlankInputLimit,
    EmptyEvent,
    MultipleEvents,
    IdentityConflict,
    IncompleteQuarantine,
}
```

The enum implements `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`,
`Serialize`, and `Display`. Serde and Display use fixed kebab-case codes.

### Inbox Port

```rust
pub enum ClaimedHarnessContent {
    Event(Box<AgentHarnessEvent>),
    Quarantine(QuarantineReason),
}

pub trait AgentHarnessInbox {
    type Claim;
    type Error: std::error::Error + Send + Sync + 'static;

    fn next_claim(&mut self) -> Result<Option<Self::Claim>, Self::Error>;
    fn read_claim(
        &mut self,
        claim: &Self::Claim,
    ) -> Result<ClaimedHarnessContent, Self::Error>;
    fn complete(&mut self, claim: Self::Claim) -> Result<(), Self::Error>;
    fn quarantine(
        &mut self,
        claim: Self::Claim,
        reason: QuarantineReason,
    ) -> Result<(), Self::Error>;
}
```

The port exposes no paths, files, names, or raw decoder errors. The adapter maps content failures
to `ClaimedHarnessContent::Quarantine`; I/O failures remain `Err`.

### Application Service

```rust
pub struct AgentHarnessIngestSummary {
    recorded: u64,
    duplicate: u64,
    quarantined: u64,
    reasons: std::collections::BTreeMap<QuarantineReason, u64>,
}

impl AgentHarnessIngestSummary {
    pub fn recorded(&self) -> u64;
    pub fn duplicate(&self) -> u64;
    pub fn quarantined(&self) -> u64;
    pub fn reasons(&self) -> &std::collections::BTreeMap<QuarantineReason, u64>;
}

pub enum AgentHarnessIngestError {
    Inbox,
    Conversion,
    Store,
}

pub fn ingest_agent_harness<I, S>(
    inbox: &mut I,
    store: &mut S,
    collector: &str,
) -> Result<AgentHarnessIngestSummary, AgentHarnessIngestError>
where
    I: AgentHarnessInbox,
    S: ObservationStore;
```

Summary implements `Debug`, `Clone`, `PartialEq`, `Eq`, and `Serialize`. Error Display/Debug and
source chains are fixed and source-free.

The use case owns policy:

1. Quarantine adapter-classified content failures and continue.
2. Convert a valid event with the caller-supplied collector to `AgentHarnessObservation`; any conversion failure is operational
   `Conversion`, leaves the claim in processing, and stops.
3. Persist through `append_harness_observation`:
   - `Recorded` -> complete, increment recorded;
   - `Duplicate` -> complete, increment duplicate;
   - `IdentityConflict` -> quarantine conflict, continue;
   - store error -> `Store`, retain processing claim, stop.
4. Inbox lifecycle failure -> `Inbox`, retain the state produced by that operation, stop.

## Filesystem Adapter

### Public API

```rust
pub struct AgentHarnessFileInbox {
    // Private directory handles, lock handle, and bounded claim snapshot.
}

impl AgentHarnessFileInbox {
    pub fn initialize(
        state_dir: &std::path::Path,
    ) -> Result<Self, AgentHarnessFileInboxError>;
}

pub struct ClaimedHarnessFile {
    // Private ULID and processing-relative name.
}

pub enum AgentHarnessFileInboxError {
    AlreadyRunning,
    UnsupportedFilesystem,
    CreateDirectory,
    InspectDirectory,
    UnsafeEntry,
    Claim,
    OpenClaim,
    Complete,
    Quarantine,
    WriteReason,
}
```

`ClaimedHarnessFile` is neither Clone nor Copy and has redacted Debug. Errors implement
`std::error::Error`, `Send`, and `Sync`; diagnostics retain no raw I/O or decoder source.

### Exclusive Consumer

Initialization opens `ingest.lock` without following links, validates owner/mode/link count, and
holds an exclusive non-blocking advisory lock for the inbox lifetime. Contention returns
`AlreadyRunning`. This prevents two `evidra ingest` processes from consuming recovered processing
claims simultaneously.

### Bounded Snapshot and Ordering

Initialization performs one bounded inventory scan, then builds queues without rescanning:

1. Inventory and count every quarantine, processing, and inbox entry.
2. Reject the inventory if it exceeds 10,000 or contains unsafe/unclassifiable entries.
3. Repair transitions represented in that inventory.
4. Remove repaired/staging entries from the in-memory inventory.
5. Add remaining processing `<ULID>.json` claims in lexical order.
6. Add ready inbox files in lexical source-name order.

All entries—including ignored `.tmp` files—count toward a combined 10,000-entry maximum. No
rescans occur during one invocation; newly published files wait for the next invocation. This
guarantees termination and avoids quadratic sorting.

Transition repair completes before claim queues are built. Malformed processing names, stale
unknown files, orphan reason/stage/temp files without their matching processing or quarantine JSON,
invalid sidecars, and unsafe
quarantine entries fail initialization rather than being guessed or deleted.

Generated reason staging files are the sole exception: `processing/<ULID>.reason.stage.<ULID>` is an
unpublished write. Initialization validates its grammar/type/owner/mode/device, removes it, and
reprocesses the still-present JSON. It is never interpreted as a reason.

### Claims and No-Clobber

Ready files are claimed with same-device, directory-handle-relative, atomic no-replace rename to
`processing/<ULID>.json`. Before rename, the generated ULID must be unused across processing JSON,
quarantine JSON/reason, and processing reason temp/stage names. Any collision retries up to 16
times, then returns `Claim`. Existing destinations are never overwritten.

Claims are opened relative to the processing directory with no-follow flags. Post-open metadata
must confirm regular file, repository owner, private mode, link count one, and expected device before
the reader is passed to `AgentHarnessJsonlSource`. The adapter requires exactly one event and maps
decoder content errors to fixed quarantine reasons. Decoder `Read` remains `OpenClaim`.

### Completion and Quarantine

Completion unlinks the processing file relative to the processing handle.

Quarantine first writes and closes the fixed reason in an unrecognized
`processing/<ULID>.reason.stage.<ULID>` create-new file. It atomically publishes the complete file as
`processing/<ULID>.reason.tmp`, then no-replace renames JSON and sidecar into quarantine. A crash
before reason publication leaves only a removable stage file and the JSON is reprocessed. Recovery
uses a validated complete temp reason to finish the same transition; it never replaces the intended
reason with another code. A legacy/reasonless quarantine JSON receives
`incomplete-quarantine`. Existing final sidecars must be regular, mode `0600`, singly linked,
correctly owned/on-device, and contain exactly one known reason code.

The adapter supports Linux and macOS with `rustix` safe APIs (`std,fs,process`) for directory-relative
no-follow/no-replace operations and
non-blocking exclusive `flock`. Unsupported `NOREPLACE` results fail closed; only destination-exists
retries a ULID, and `EXDEV` is `UnsupportedFilesystem`. Other targets compile a fail-closed stub.
No unsafe code is introduced.

The CI matrix runs inbox adapter tests on macOS and Linux with Rust 1.85, plus a Windows workspace
check that compiles the fail-closed stub. Windows is not advertised as supported.

## CLI Surface

```text
evidra ingest [--json]
```

The CLI opens schema v2 before inbox initialization. Uninitialized or v1 repositories fail before
claiming any file.

Exact success JSON:

```json
{
  "recorded": 3,
  "duplicate": 2,
  "quarantined": 2,
  "reasons": {
    "identity-conflict": 1,
    "invalid-json": 1
  }
}
```

Reason keys are sorted; zero values are omitted. Exact success text:

```text
Ingested 3 recorded, 2 duplicate, 2 quarantined
Quarantine identity-conflict: 1
Quarantine invalid-json: 1
```

Both modes end with a newline and reveal no evidence metadata.

## Failure and Output Semantics

Operational failures exit nonzero. Stdout remains empty even when earlier claims were successfully
processed. Stderr contains exactly one fixed category line:

```text
ingest failed: inbox
ingest failed: conversion
ingest failed: store
```

No partial text/JSON summary is emitted. Database and conversion failures leave the current claim
in processing. Inbox errors preserve the transition state at the documented operation boundary:

- claim/open failure: JSON remains ready or processing;
- completion failure: committed JSON remains processing and retries as duplicate;
- reason-temp failure: JSON remains processing;
- JSON quarantine rename failure: JSON and temp reason remain processing;
- sidecar finalization failure: JSON is quarantined and validated temp reason remains recoverable.

Conversion failures are never quarantined as invalid evidence.

## Crash Matrix

| Process Exit Point                          | Next Invocation                        |
| ------------------------------------------- | -------------------------------------- |
| Producer `.tmp`                             | Ignored                                |
| Published inbox JSON                        | Claimed                                |
| Processing JSON                             | Recovered before ready work            |
| Database commit before completion           | Duplicate, then complete               |
| Partial unpublished reason stage            | Remove stage; reprocess JSON           |
| Processing JSON plus reason temp            | Finish quarantine with original reason |
| Quarantine JSON plus processing reason temp | Finish sidecar move                    |
| Legacy quarantine JSON without reason       | Write `incomplete-quarantine`          |

Power-loss durability is not claimed.

## Data Flow

1. Producer publishes one redacted event file.
2. Filesystem adapter claims and decodes it behind `AgentHarnessInbox`.
3. Core application service applies quarantine/persistence policy.
4. SQLite returns recorded/duplicate/conflict.
5. Core directs completion/quarantine; CLI renders only the summary or fixed error category.

## Out of Scope

- Harness event producers or hook/plugin installation.
- Continuous directory watching.
- Remote/shared inboxes or network filesystems.
- Power-loss durability guarantees.
- Semantic secret scanning and authenticated producer identity; explicitly deferred and required
  before granting inbox access to untrusted producers.
- Automatic retries within one invocation.
- Reprocessing quarantine files.
- Derived facets, controls, inference, or enforcement.

## Risk Checklist

- [x] Breaking API change: additive core port/service and CLI command before `1.0.0`.
- [x] Persisted compatibility change: none.
- [x] New external dependency: pinned `rustix 1.1.5`, justified by lock/no-follow/no-replace safety.
- [x] Dependency features required: `rustix` `std,fs,process`; no Evidra feature flag.
- [x] Circular dependencies: none.
- [x] AI authority: none; deterministic evidence ingestion only.

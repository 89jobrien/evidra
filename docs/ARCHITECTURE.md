# Architecture

How Evidra is put together, and why it is put together that way. For agent workflow rules see
[`AGENTS.md`](../AGENTS.md); for storage internals see [`SCHEMA.md`](SCHEMA.md); for day-to-day
operation see [`OPERATIONS.md`](OPERATIONS.md).

## The one property everything else follows from

Evidra never mutates a record it has accepted. Every table is append-only and every `UPDATE` and
`DELETE` is aborted by a trigger. This is enforced rather than documented — see
[`SCHEMA.md`](SCHEMA.md) for the trigger inventory and the probe technique.

That single constraint has consequences that explain most of the codebase:

- **Nothing can be corrected in place.** A wrong record is superseded by a new one, not edited.
  `derivations.supersedes` and the `supersedes` relation exist so chains are explicit.
- **A reader cannot trust the file.** If no writer may modify a row, a modified row is
  indistinguishable from a forged one. So `open()` does not open — it _probes_. It checks identity,
  compares trigger definitions as normalized text, runs live mutation probes inside a rolled-back
  savepoint, and re-deserializes every stored document through its domain type. See
  [`SCHEMA.md`](SCHEMA.md#validation-on-open).
- **Derived values are re-derived, not trusted.** Indexed columns (`observed_at`, `kind`, facet
  `namespace`/`name`) are denormalized for query speed, so each one is cross-checked against the
  document it was derived from on every read. Drift is a hard error, not a silent wrong answer.
- **The clock is a caller input.** A record that stamped its own `recorded_at` could not be
  reproduced, and reproducibility is what makes derived output checkable. `DerivationDraft` and
  `Relationship` therefore take `recorded_at` from the caller; `evidra-engine` reads no clock at all
  and emits timestamp-free `FacetProjection` values for the caller to stamp. `Observation::record`
  is the single deliberate exception — it mints identity and `observed_at` itself — so the two
  layers do **not** share this property.

## Crates

Dependencies point inward. Nothing depends outward.

```text
evidra-cli     -> evidra-store    -> evidra-core
               -> evidra-adapters -> evidra-core
evidra-engine  -> evidra-core
```

| Crate             | Owns                                             | Never does                                    |
| ----------------- | ------------------------------------------------ | --------------------------------------------- |
| `evidra-core`     | Domain types, ports, ingestion policy            | SQLite, CLI, Git, HTTP, LLM, any I/O          |
| `evidra-engine`   | Banding, redaction inheritance, facet projection | I/O of any kind; depends on `evidra-store`    |
| `evidra-store`    | SQLite persistence implementing core ports       | Business rules; deciding what a value _means_ |
| `evidra-adapters` | Bounded JSONL normalization; inbox lifecycle     | Persisting observations                       |
| `evidra-cli`      | Composition root, argument parsing, rendering    | Business rules                                |

`AGENTS.md` records the rule that governs additions: **do not add a crate without a distinct
responsibility that cannot fit an existing boundary.** Two boundaries exist that no crate may
cross, and both are load-bearing rather than stylistic:

**`evidra-core` performs no I/O.** Ingestion _policy_ — which failure quarantines, which stops the
run, what a summary reports — lives in `evidra-core/src/ingest.rs` because it is a rule about
evidence, not about files. The CLI composes and renders it; the adapter supplies the filesystem
mechanics. Moving policy into the adapter would make it untestable without a filesystem.

**`evidra-engine` decides nothing about storage.** Banding is a judgement call about what a number
means, and redaction inheritance is a judgement about what may safely be emitted. The storage layer
must not make either. This is why `BandTable` and `Registry` live in the engine and the store only
receives finished `FacetProjection` values.

### Ports

`evidra-core/src/ports.rs` defines the seams. External systems sit behind them:

| Port                      | Implemented by            | Purpose                      |
| ------------------------- | ------------------------- | ---------------------------- |
| `ObservationStore`        | `SqliteObservationStore`  | Append and list observations |
| `DerivationStore`         | _not yet implemented_     | Append derived records       |
| `RelationshipStore`       | _not yet implemented_     | Append relationships         |
| `AgentHarnessInbox`       | `AgentHarnessFileInbox`   | Claim, complete, quarantine  |
| `AgentHarnessEventSource` | `AgentHarnessJsonlSource` | Produce normalized events    |

`DerivationStore` and `RelationshipStore` are declared but unimplemented — a ports-first stance.
`evidra-engine` is the intended first consumer.

## Layering in practice

Three layers stack above the ledger, each append-only and each linking back to observations:

```text
observations          what was recorded           (schema v1)
  + receipts          which harness identity produced it, and its digest   (v2)
  + derivations       banded facets, explicit uncertainty                  (v3)
  + ... facets/evidence  indexed projections of the above
  + relationships     typed edges: supports, refutes, caused-by, ...       (v3)
```

Derived reasoning must always be traceable to evidence. `derivation_evidence` rows carry a role —
`supporting` or `refuting` — and a `target_key` naming either an observation or another derivation.
A derivation with no evidence link cannot be resolved in any current view.

## Trust boundaries

**The inbox is a trust boundary, and it is a narrow one.** Producers publish into
`.evidra/inbox/`; they are same-user, trusted, and _not authenticated_. Harness names are attributed
claims, not verified identity. Never grant inbox write access to an untrusted producer. The inbox
validates file metadata — ownership, exact mode, no symlink, no hard link — but those checks reject
metadata anomalies, not a same-user process holding a writable descriptor.

**Redaction is a hard invariant on the way out.** A derived record inherits the strictest redaction
across its evidence set, and no emitted value may be a substring of, or derivable from, any source
excerpt. This is why the engine must see `RedactedExcerpt` rather than raw evidence, and why a
namespace that would describe observed content rather than a category cannot be registered at all.

**AI may propose but never grants authority.** Nothing in this system lets a model widen its own
scope. Band tables, registration, and policy are deterministic functions of registered inputs.

## Determinism

Every judgement the system makes is a pure function of explicit inputs. Banding is total and
monotonic over a dense range and bounded at `MAX_BANDS = 8`; registration is the choke point that
stops a misspelling creating a permanently unqueryable partition. Two runs over the same evidence
produce the same projections, so a derived record can be re-derived and compared rather than
trusted. Policy evaluation and enforcement are deterministic by construction — see
[ADR-005](adr/ADR-005-deterministic-control-boundaries.md).

## What is deliberately absent

These are unimplemented, not overlooked:

- **Git, Cargo, CI, and manual-input adapters.** Only the agent-harness adapter exists.
- **A CLI path to derived records.** Nothing depends on `evidra-engine` yet; Slice 1 is reachable
  only through the library API. `evidra ingest` and `evidra observation list` are the whole
  command surface.
- **Policy files.** `policies/` holds four categories (`assumptions`, `invariants`, `controls`,
  `accepted-risks`), all currently empty scaffolding awaiting
  [ADR-007](adr/ADR-007-sqlite-and-versioned-policy-files.md).
- **Continuous ingestion.** `evidra ingest` is a bounded single pass. It does not watch, does not
  retry within a run, and never re-consumes `quarantine/`.
- **Durability across power loss.** No `fsync` is issued anywhere in the inbox.

## Further reading

| Document                                   | Covers                                           |
| ------------------------------------------ | ------------------------------------------------ |
| [`SCHEMA.md`](SCHEMA.md)                   | Tables, triggers, migrations, validation on open |
| [`OPERATIONS.md`](OPERATIONS.md)           | Inbox lifecycle, quarantine, failure semantics   |
| [`CONVENTIONS.md`](CONVENTIONS.md)         | Invariants every contributor must preserve       |
| [`../docs/PRD.md`](../docs/PRD.md)         | Problem, users, requirements, non-goals          |
| [`../docs/ROADMAP.md`](../docs/ROADMAP.md) | Slice sequence and per-slice gates               |
| [`adr/`](adr/)                             | Decisions 001–011, with accepted consequences    |
| [`designs/`](designs/)                     | Per-slice designs                                |

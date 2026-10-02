# Product Requirements: Evidra

- Status: Draft
- Date: 2026-10-02
- Scope: v0.1.0 through v0.3.0

## Problem

Agent sessions end and the reasoning ends with them. What survives is a commit log, which records
that something changed and never why it changed. A failing automation leaves no record of the three
times it was hand-intervened before. A stale assumption outlives the reasoning that justified it and
is never revisited.

Recurring interventions, failed automation, stale assumptions, and incidents are the signal. They are
present in the transcript and discarded by it.

## Users

Single operator, local machine, single repository at a time. Not a team product and not a service.

The user is running long agent sessions across several repositories and wants the reasoning that
produced each outcome to survive the session, stay attributable, and accumulate into something they can
act on — rather than reconstructing intent from diffs.

## Product

An append-only observation ledger plus a derived layer that aggregates, revises, and eventually
attributes.

| Layer       | Purpose                                                                                | Status      |
| ----------- | -------------------------------------------------------------------------------------- | ----------- |
| Ingestion   | Accept bounded, redaction-attested evidence from an agent harness and manual notes     | Implemented |
| Ledger      | Persist observations immutably with source, subject, provenance, timestamps, integrity | Implemented |
| Derived     | Project faceted, banded signals and typed relationships over the ledger                | Designed    |
| Attribution | Assign cause to failures with banded confidence and evidence                           | Not started |
| Control     | Evaluate policy against derived knowledge and propose dispositions                     | Not started |

## Requirements

### R1. Evidence is never silently discarded

Every accepted record carries source, subject, provenance, and an integrity digest. A record that
cannot be attributed to a source is not admitted.

### R2. Storage is append-only

The ledger rejects mutation at the database level, not by convention. Corrections are new records
plus relationships (ADR-002, ADR-008).

### R3. Claims never overwrite their evidence

A derived record references the observations it came from and retains refuting evidence alongside
supporting evidence (ADR-003).

### R4. Uncertainty is explicit state

Quality, freshness, contradiction, scope, and confidence are stored fields, not inferred from a score
(ADR-004).

### R5. Ingestion is safe to re-run

A producer replaying the same event must not create a second record. Receipts are keyed on
`(harness, session_id, source_event_id)`; conflicts are quarantined with a reason rather than
overwritten.

### R6. Redaction is inherited, not assumed

A derived record may not weaken the redaction of its evidence, and may not reconstruct content that
redaction removed (ADR-010).

### R7. Ingestion is exclusive

A single writer holds an exclusive lock over the inbox, so concurrent producers cannot interleave a
claim and an intake.

### R8. Inference never holds authority

Assisted methods may cluster, propose, and summarise. Whether an action is allowed, denied, warned,
quarantined, or escalated is decided by deterministic code over versioned policy (ADR-005).

## Quality bar

| Property       | Threshold                                                                          |
| -------------- | ---------------------------------------------------------------------------------- |
| Rust toolchain | 1.85, edition 2024, inherited by every crate                                       |
| Warnings       | Zero at `-D warnings` across all targets                                           |
| `unsafe`       | Forbidden workspace-wide                                                           |
| Public API     | Documented; `missing_docs` warns                                                   |
| Panics         | `unwrap`, `expect`, `panic`, `todo`, `unimplemented` forbidden in production paths |
| Tests          | One conformance or property suite per invariant, not per example                   |
| Secrets        | Redaction-first; staged diffs scanned before commit                                |

## Non-goals

Carried from ADR-001 and the design record, not newly introduced here.

- Networked or SaaS deployment. The first deployment is local and single-operator (ADR-007).
- A policy engine that interprets unstructured prose. Policy intent is structured and
  Git-reviewed (ADR-007).
- Model-authored dispositions. See R8 and ADR-005.
- Replacing the task graph, CI, or version control as sources of operational truth. Evidra records
  what happened and why; it does not become the system of record for the work itself.
- Treating absence of an event as evidence that an action did not occur.

## Success criteria

Evidra is succeeding when a cold reader can answer, for a repository, without reading a transcript:

1. What failed, how often, and under what conditions.
2. Which decisions were made, what was rejected, and what would invalidate them.
3. What has not been true long enough to trust.
4. Which recurring interventions should become automation.

Criterion 1 depends on the derived layer. Criteria 2 and 3 depend on attribution. Criterion 4
depends on control. None of them is reachable from the ledger alone.

## Open questions

| Question                                                                   | Blocks        |
| -------------------------------------------------------------------------- | ------------- |
| Which harnesses are first-class sources beyond the current JSONL adapter?  | Ingestion     |
| Does redaction inheritance make some useful facets inadmissible under R6?  | Derived layer |
| What is the retention policy for derived records and superseded revisions? | Derived layer |
| Can attribution meet a usable accuracy bar, or does it stay advisory-only? | Attribution   |

The accuracy question is the load-bearing one. Published step-level attribution results put best-in-
class accuracy near 47% on synthetic benchmarks. If attribution cannot clear a usable threshold on
real traces, R8's split means it informs a human rather than gating a machine, and the product should
say so plainly rather than presenting advisory output as settled.

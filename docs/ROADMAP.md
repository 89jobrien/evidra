# Roadmap

- Status: Draft
- Date: 2026-10-02

Sequenced by dependency, not by importance. Each slice is gated on its conformance sections and
property invariants; a slice is done when its gates pass and no earlier gate regresses.

## Shipped

| Slice                           | Record                                                                    |
| ------------------------------- | ------------------------------------------------------------------------- |
| Observation ledger              | `docs/designs/2026-09-18-observation-ledger-slice-design.md`              |
| Agent-harness evidence contract | `docs/designs/2026-09-19-agent-harness-evidence-contract-design.md`       |
| Inbox ingestion                 | `docs/designs/2026-09-19-agent-harness-inbox-ingestion-design.md`         |
| Observation persistence         | `docs/designs/2026-09-19-agent-harness-observation-persistence-design.md` |

## Slice 1 — Derived facets and relationships

`docs/designs/2026-10-02-derived-facet-and-relationship-design.md`

Fills the reserved `evidra-engine` boundary and advances `SCHEMA_VERSION` to 3.

Banded facet projections over the ledger; append-only relationships carrying confidence and scope;
supporting and refuting evidence links; supersession by chain (ADR-008); banded numeric facets
(ADR-009); redaction inheritance (ADR-010); banded confidence (ADR-011).

**Gate:** eleven conformance sections and twenty-one property invariants, all passing.

## Slice 2 — Decision capture

Adds `DecisionClaim` as a derived kind: the decision, its rationale, the alternatives that were
rejected and why, and a validity condition carrying `holds_while` and `invalidates_when`.

The distinctive requirement is that a decision with no recorded alternative is inadmissible.
Rejected alternatives are the only primitive that makes a decision reviewable after the context that
produced it is gone.

**Gate:** a decision cannot be recorded without at least one rejected alternative; an assumption
without `invalidates_when` fails the policy-file contract.

## Slice 3 — Failure attribution

Adds attribution as a method-gated derivation producing `CausedBy` and `Prevented` relationships
with competing hypotheses rather than a single answer.

Two constraints are load-bearing. The step that manifests a failure is frequently not the step that
decided it, so a `CausedBy` link is a hypothesis rather than a finding. And when two steps fail only
together, single-link attribution is misleading in both directions, so no derivation may claim sole
responsibility without stating its method.

**Gate:** the joint-cause fixture must not produce a sole-cause claim; a `CausedBy` at `Moderate` or
above must carry at least two corroborating supporting links.

Whether attribution is ever permitted to gate anything is an open question in the PRD, not a premise
of this slice. Slice 3 informs a human first.

## Slice 4 — Policy evaluation and session insight

Policy files become a compiled, integrity-checked package set; named report definitions replace ad hoc
aggregation.

This is where R6, R8, and ADR-007 stop being constraints on the design and become load-bearing code.

**Gate:** every policy package compiles under a frozen lock; every assumption declares an invalidation
trigger; report definitions are deterministic and Git-tracked.

## Slice 5 — Trajectory export

Exports the ledger as an external trajectory format for training and evaluation use.

Export is a projection. It must never write back, and running it must leave every digest in the ledger
unchanged (ADR-003).

**Gate:** the export run leaves the ledger byte-identical.

## Deferred, not scheduled

| Item                                     | Why deferred                                                                                     |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Harness hook installation                | Needs a decision on which harnesses are first-class                                              |
| Git, Cargo, CI, and filesystem importers | Each needs its own ingestion contract and idempotency key                                        |
| Derived-record retention                 | Superseded revisions accumulate without bound; no policy yet                                     |
| Protocol-drift gate on the port surface  | `evidra-core/src/ports.rs` is the workspace's highest-value drift surface and has no taskit lock |

## Sequencing note

Slice 3 is the one most likely to change shape. Published accuracy for step-level attribution is near
47% at best, and lower outside synthetic benchmarks. Slices 4 and 5 both assume attribution output is
worth consuming; if it is not, Slice 4 still stands on the derived layer alone, and Slice 5 does not
depend on attribution at all. Neither is blocked by Slice 3 landing badly.

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

Banded facet projections over the ledger; append-only relationships carrying confidence and scope;
supporting and refuting evidence links; supersession by chain (ADR-008); banded numeric facets
(ADR-009); redaction inheritance (ADR-010); banded confidence (ADR-011).

Landed across `f80a00e` and `adc99cf`: the domain in `evidra-core`, schema v3 in `evidra-store`, and
band tables plus facet projection in `evidra-engine`. Not yet wired into `evidra-cli` — no crate
depends on `evidra-engine`, so the slice is reachable only through the library API today.

**Gate: not met.** The plan's stated gate was eleven conformance sections and twenty-one property
invariants. Neither exists in that quantity:

- **Conformance: 0 of 11.** There is no `docs/conformance.md` and no `tests/conformance_*.rs` in any
  crate. Tasks 4 and 5 of `docs/plans/2026-10-02-derived-facets-and-relationships.md` are unwritten
  and carry no Status line.
- **Properties: 9 of 21.** Five `proptest` cases in `evidra-core`, four in `evidra-engine`. Real
  coverage of the read path and of banding, but well short of the count the gate claimed.

The count was reported as met without the suites being written. Audit findings A-17 and A-18 in
[`AUDIT.md`](AUDIT.md) carry the detail; the second is the one that matters most, because
`no_facet_value_appears_in_source_excerpt` — named by ADR-010 and three other documents as the
property checking the redaction guarantee — does not exist.

**What actually blocks the slice.** `DerivationStore` and `RelationshipStore` have no implementor and
no consumer, and `ObservationStore` has no `get` or `select`. Schema v3's four derived tables are
populated only by raw-SQL helpers inside a test module. The remaining work is specified in
[`designs/2026-10-03-derived-cli-surface-design.md`](designs/2026-10-03-derived-cli-surface-design.md),
whose Step 1 is that port slice; Task 6 of the plan cannot begin before it.

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

| Item                                     | Why deferred                                                                                                                                            |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Harness hook installation                | Needs a decision on which harnesses are first-class                                                                                                     |
| Git, Cargo, CI, and filesystem importers | Each needs its own ingestion contract and idempotency key                                                                                               |
| Derived-record retention                 | Superseded revisions accumulate without bound; no policy yet                                                                                            |
| Protocol-drift gate on the port surface  | `evidra-core/src/ports.rs` is the workspace's highest-value drift surface and has no taskit lock ([#24](https://github.com/89jobrien/evidra/issues/24)) |

## Audit findings

Open findings from the workspace audit are indexed in [`AUDIT.md`](AUDIT.md). Four are load-bearing
for planning rather than for a particular slice:

- **The port slice (A-04 to A-07).** No implementor for either derived store, and no way to read a
  record or execute a scope. Specified in
  [`designs/2026-10-03-derived-cli-surface-design.md`](designs/2026-10-03-derived-cli-surface-design.md).
- **ADR-010's property (A-18).** The redaction guarantee is enforced in code and exercised by fixed
  cases, but the property four documents cite as its check does not exist.
- **Derived-record tamper detection (A-09).** `derivations` has no digest column, so the read-path
  probe cannot detect a swapped document. This bears on ADR-003 and on Slice 5's byte-identical
  gate.
- **Three unenforced lints (A-11).** `unwrap_used`, `expect_used`, and `missing_docs` are all clean
  and all disabled. The one production `expect` in the workspace exists because of this.

## Sequencing note

Slice 3 is the one most likely to change shape. Published accuracy for step-level attribution is near
47% at best, and lower outside synthetic benchmarks. Slices 4 and 5 both assume attribution output is
worth consuming; if it is not, Slice 4 still stands on the derived layer alone, and Slice 5 does not
depend on attribution at all. Neither is blocked by Slice 3 landing badly.

One dependency the earlier version of this note omitted: **Slice 2 cannot begin before the port slice
lands.** Decision capture adds a derived kind, and `DerivationKind::Cluster` already shows what
happens when a kind is added ahead of its store — the domain type exists, nothing can persist it, and
nothing notices. That is precisely the state Slice 1 is in now.

### Two gates outside the slice sequence

Neither is a slice, and both block something downstream of one.

**The ADRs are unratified.** ADR-008 through ADR-011 are all `Status: Proposed` with no approval
date, while their code, schema, and tests have landed. `docs/designs/2026-10-03-derived-cli-surface-design.md`
calls ratifying them "a precondition for building against them, not a formality to clear afterwards."
ADR-010 is the one that matters: it accepts a real coverage cost, and that trade currently has no
tested guard ([#22](https://github.com/89jobrien/evidra/issues/22)).

**A release cannot ship from this tree.** `obfsck` is a path-only dependency, so the workspace cannot be
published to crates.io at all, and the producer's Stage 2 depends on path-policy behaviour that differs
between obfsck versions ([#23](https://github.com/89jobrien/evidra/issues/23)).

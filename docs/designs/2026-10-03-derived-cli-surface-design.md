# Design: Derived CLI Surface

- Status: Draft
- Date: 2026-10-03
- ADRs: ADR-003, ADR-004, ADR-008, ADR-009, ADR-010, ADR-011
- Plan: `docs/plans/2026-10-02-derived-facets-and-relationships.md` (Task 6)
- Supersedes nothing. Extends the surface left unspecified by
  `docs/designs/2026-10-02-derived-facet-and-relationship-design.md`.

## Problem this solves

Task 6 of the derived-facets plan requires failing tests for `derive`, `facet`, `relate`, and
`explain`, "including exit codes and the `--json` schema." The Oct 2 design review recorded the same
gap: the CLI subcommand surface had no specifications for arguments, output, or exit codes.

This document specifies that surface. It also records a finding that changes Task 6's shape: **the
surface is not implementable against the current port set.** Two read paths the commands require do
not exist, and one command cannot be built without a new port. Those gaps are specified first,
because writing the argument and exit-code tables against ports that cannot serve them would produce
a document that reads complete and implements nothing.

The CLI remains a composition root. Every rule below is enforced in `evidra-core` or `evidra-engine`;
the CLI selects, forwards, and renders.

## Governing decision status

ADR-001 through ADR-007 are `Accepted`. ADR-008 through ADR-011 are all `Proposed` — the split falls
exactly at the derived layer.

That matters more than a status field usually does, because ADR-010's constraints are already
enforced in code. Task 3 of the plan records that the redaction-inheritance check runs on every
emitted value, and four fixed cases exercise it; ADR-009's registration check is enforced at
registration time. Schema v3 is migrated and `evidra-engine` implements banding and redaction
inheritance.

One correction to the record this document was drafted against. An earlier version cited the property
`no_facet_value_appears_in_source_excerpt` as the check on ADR-010. **That property does not exist.**
The check is real and runs on every emitted value, but it is covered only by named fixed cases, and
the property four documents cite on its behalf is unwritten. That is finding A-18 in
[`../AUDIT.md`](../AUDIT.md), and it is the most expensive single finding to leave open: ADR-010's
accepted coverage cost is a trade, and a trade needs its guard tested.

The decision is otherwise load-bearing in fact and provisional in record. If ADR-010 is amended
rather than ratified, the schema, the engine, and this specification all change together — and
ADR-010 carries a deliberate, acknowledged coverage cost ("records that cannot be redacted without
losing their analytic value must be dropped rather than derived") that is the most likely thing to be
contested on review.

Ratifying ADR-008 through ADR-011 is a precondition for building against them, not a formality to
clear afterwards. Until then this document specifies a surface whose foundations may still move.

## Audit findings that bear on this specification

The three blocking findings below were confirmed against the code by the workspace audit, along with
the seven conformance clauses and three port signatures. Audit ids are in
[`../AUDIT.md`](../AUDIT.md): Gap 1 is A-04 and A-07, Gap 2 is A-06, Gap 3 is unblocked by audit. A-13
records that three `kind_str`-style accessors exist for the adapters this specification calls for and
currently have no callers — they should be wired to the store rather than re-implemented as literals
once Step 1 lands.

## Blocking findings

### Gap 1 — no read-by-id on either store

`explain` exists to answer "what does this claim rest on, and what would make it wrong." Doing that
requires fetching a stored `Derivation` by id. No port offers that.

| Port                | Methods available                              |
| ------------------- | ---------------------------------------------- |
| `DerivationStore`   | `append`, `current_facets`, `aggregate`        |
| `RelationshipStore` | `append`, `neighbors`                          |
| `ObservationStore`  | `append`, `list`, `append_harness_observation` |

`current_facets` returns `CurrentFacet`, which carries `derivation`, `namespace`, `name`, `value`,
`profile`, and `current`. That is a facet projection plus an uncertainty profile — not the derivation
document. Kind, scope, method, rationale, and the `supersedes` pointer are all unreachable.
`neighbors` returns edges without their endpoint documents.

**Required:** `DerivationStore::get(&DerivationId) -> Result<Option<Derivation>>`, and
`ObservationStore::get(&ObservationId) -> Result<Option<Observation>>`.

`get` returns `Option` rather than erroring on absence, so a caller can distinguish "no such record"
from "the store is unreadable." `explain` maps `None` to exit code 6.

### Gap 2 — no observation selection

`DerivationScope` carries a subject, a closed date window, and a `Vec<FacetFilter>`, and
`DerivationStore::current_facets` and `aggregate` both take one. Nothing can _execute_ a scope
against observations. `ObservationStore::list(limit)` returns observations newest-first with no
filter on subject, kind, or window — it is a display query, not a selection primitive.

`derive` cannot select its evidence set without this, and neither can a scope be constructed
defensibly: a user-supplied `--from`/`--to` window with no server-side filter would silently derive
from whatever `list` returned.

**Required:** `ObservationStore::select(&DerivationScope) -> Result<Vec<Observation>>`, defined as
the exact filter `current_facets` applies when resolving a scope, so the evidence set of a derivation
and the rows that scope considers current cannot disagree.

This is the same class of defect as Task 2's: `Derivation::facets` was a tuple that could be neither
selected nor reported back. A scope that cannot be executed is a specification with no behaviour.

### Gap 3 — `relate` cannot express supersession

ADR-008 requires that a correction append a new derivation **plus** a `Supersedes` relationship, and
that `Derivation::supersedes` is denormalised onto the record and "must agree with the corresponding
relationship or the append is rejected."

`relate` writes a `Relationship` and does not own a `Derivation`. Allowing
`relate --relation supersedes` would therefore create an edge with no denormalised counterpart — or
worse, one that contradicts a stored derivation, which is exactly the drift ADR-008 forbids.

**Decision:** `relate` refuses `supersedes`. Supersession is expressed only by
`derive --supersedes <id>`, which appends both records in one call and can satisfy the agreement
check atomically. `relate` returns exit code 4 for this relation kind and says so in `stderr`.

This also sidesteps cycle detection at the port boundary. ADR-008 requires that a chain containing a
cycle be rejected at write. Because supersession can only be created alongside its derivation, the
chain is walked before the append and rejected if the new edge would close one. `neighbors` is
sufficient for that walk, but only because the walk terminates at a single edge-construction site.

## Port additions

The three methods above are specified here rather than left as names in a task list, because the next
step is domain work in `evidra-core` and `evidra-store` and a signature decided at the keyboard will
differ from one decided against these semantics.

### `ObservationStore::get`

```rust
fn get(&self, id: &ObservationId) -> Result<Option<Observation>, Self::Error>;
```

`Ok(None)` means no such observation. `Err` means the store was unreadable or a stored document failed
validation. A row whose document does not deserialize through `Observation`'s manual `Deserialize` and
pass its integrity check is an `Err`, never `None` — the same rule `validate_derived_rows` applies,
and the same reason: a corrupt store must not be indistinguishable from an empty one.

`Option` rather than a `NotFound` error variant because absence is an expected outcome for `explain`
and for evidence resolution. Exit 6 is a normal response, not a failure.

### `ObservationStore::select`

```rust
fn select(&self, scope: &DerivationScope, limit: usize) -> Result<Vec<Observation>, Self::Error>;
```

The predicate is the one `current_facets` already applies when resolving a scope:

- subject equality against `scope.subject`;
- `from_occurred_at <= occurred_at <= to_occurred_at`, closed, matching `DerivationScope::new`;
- every `FacetFilter` in `scope.selection` satisfied.

**This must not be a second implementation.** `select` and `current_facets` have to call one shared
predicate — ideally a scope-matching helper in `evidra-core` that both store methods delegate to — not
two SQL statements that happen to agree. If they diverge, a derivation's evidence set and the rows its
scope considers current disagree, and the derivation becomes unfalsifiable: it would cite evidence the
current view no longer recognises. That is Task 2's defect again, in a worse place, because the v3
validator cannot see it — both queries are individually well-formed.

Ordering is newest-first with ties broken by canonical `ObservationId` ascending, so repeated calls
over unchanged data return identical sequences. The name-ordered schema snapshot in Task 2 and the
lexicographic tie-breaking elsewhere in this workspace are both instances of the same lesson:
determinism that depends on storage order is not determinism.

`limit` is required, matching `list`. This is a selection feeding a derivation, so an unbounded
`Vec` is a memory risk against a store designed to grow without bound; the CLI takes the same
`1..=1000` range as `parse_limit`.

### `DerivationStore::get`

```rust
fn get(&self, id: &DerivationId) -> Result<Option<Derivation>, Self::Error>;
```

Same `Option` and validation-failure semantics as the observation accessor. `Derivation` carries
`facets: Vec<FacetProjection>` in its document, so one call returns the record together with its
projected facets and `explain` needs no second read. `current_facets` remains necessary for
scope-wide views that resolve the supersedes chain, and is not a substitute for `get`.

### Supersedes walk and cycle rejection

ADR-008 requires a chain containing a cycle to be rejected at write. Because `relate` refuses
`supersedes` (Gap 3), the only construction site is `derive --supersedes`, which appends the
derivation and its edge together.

Before the append, walk `neighbors(current, Some(RelationKind::Supersedes))` transitively from the
new derivation. Reaching the id being superseded is a rejection. The walk is depth-bounded by a
constant; exceeding the bound is also a rejection rather than an error, since a chain that deep is
already unusable and continuing would be an unbounded walk.

**The check must be repeated inside `append`.** Two concurrent `derive` invocations can each walk,
each find no cycle, and then both append — producing a two-cycle that no pre-append check prevented.
The pre-append walk is a fast path; the authoritative check belongs on the append path where it holds
the store's write lock, alongside the agreement check ADR-008 already requires between
`Derivation::supersedes` and the edge.

### Conformance additions

Numbered for `docs/conformance.md`, alongside the existing suites — with the caveat that
`docs/conformance.md` and every `tests/conformance_*.rs` file in the workspace are unwritten. Task 4 of
the derived-facets plan covers eleven sections that do not exist yet (A-17 in
[`../AUDIT.md`](../AUDIT.md)), so these seven are the first clauses the index would carry rather than
an addition to it.

- **STORE-01** `get` returns `Ok(None)` for an absent id and does not mutate the store.
- **STORE-02** `get` returns `Err` for a row whose document fails deserialization or its integrity
  check, and `Ok(None)` is never used to report corruption.
- **STORE-03** `select` and `current_facets` agree on scope membership for every fixture scope; a
  shared predicate is asserted structurally, not only behaviourally.
- **STORE-04** `select` returns newest-first with `ObservationId` tie-breaking, and repeated calls
  over unchanged data return identical sequences.
- **STORE-05** `select` honours `limit`, rejects zero, and rejects a limit above the maximum.
- **STORE-06** A supersedes append that would close a cycle is rejected inside `append`, verified by
  two interleaved appends rather than by a single sequential one.
- **STORE-07** `get` returns facets consistent with `derivation_facets` rows for the same id.

## Exit codes

The current `main` returns only `ExitCode::SUCCESS` and `ExitCode::FAILURE`. The derived commands add
refusals that are _successful outcomes_, and a caller needs to tell them apart from failures.

| Code | Name       | Meaning                                                                                                                            |
| ---- | ---------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| 0    | success    | The command completed and wrote its output.                                                                                        |
| 1    | failure    | Unclassified error. `Error: {chain:#}` on stderr.                                                                                  |
| 2    | usage      | Argument parsing. clap's existing behaviour, unchanged.                                                                            |
| 3    | store      | Store missing, unreadable, or a legacy schema. Carries migration guidance.                                                         |
| 4    | refused    | The domain rejected the request: unregistered facet, redaction breach, confidence ceiling, supersession via `relate`, blank input. |
| 5    | empty      | The selection is well-formed but yields nothing. A successful refusal, not an error.                                               |
| 6    | unresolved | A named record does not exist, or its evidence chain cannot be walked.                                                             |

**Why 5 exists.** ADR-010 accepts that some records "cannot be redacted without losing their analytic
value must be dropped rather than derived," and ADR-011 caps assisted derivations at `Weak`. Both make
"no derivation was produced" a legitimate result of a well-formed request. Collapsing it into 1 makes
it indistinguishable from a crash and from a bug. Automation that runs `derive` in a loop needs to
count refusals without counting failures.

Code 4 is likewise distinct from 1: a refusal is the system working. The distinction is the same one
between "the test failed" and "the test refused to run."

## JSON envelope

Every `--json` response is an object carrying a versioned schema tag, matching the wire-format
convention in `docs/SCHEMA.md`:

```json
{ "schema": "evidra.derive-result/v1", "derivation": { "id": "01J..." } }
```

| Command         | Tag                        |
| --------------- | -------------------------- |
| `derive`        | `evidra.derive-result/v1`  |
| `facet current` | `evidra.current-facets/v1` |
| `facet count`   | `evidra.facet-counts/v1`   |
| `relate`        | `evidra.relate-result/v1`  |
| `explain`       | `evidra.explain-result/v1` |

A `/v2` is unknown and rejected. Error codes are part of this contract the moment `--json` exposes
them, which is why the engine already asserts its 73 codes are namespaced and unique and why
cross-crate uniqueness belongs to the Task 4 conformance suites.

## Commands

### `derive`

```text
evidra derive --subject <SUBJECT> --from <RFC3339> --to <RFC3339>
              [--namespace <NS>]... [--facet <NAME>]...
              [--method <deterministic|assisted>]
              [--supersedes <DERIVATION_ID>] [--rationale <TEXT>] [--json]
```

Selects observations in the scope, projects each requested facet through
`Registry::project_banded` or `project_category`, constructs a `Derivation` from a `DerivationDraft`,
and appends it with an evidence link per contributing observation.

`--from` and `--to` are both required. There is no "now" default: `Derivation` takes `recorded_at`
from the caller and `evidra-engine` reads no clock, so identical inputs must produce an identical
record. A default of `Utc::now()` would make the same command produce different records on every
invocation and break the property `evidra-engine`'s determinism rests on.

`--method` defaults to `deterministic`, which may carry `Strong`. `--method assisted` is capped at
`Weak` by the domain (ADR-011), not by the CLI. The cap is enforced in `UncertaintyProfile` so an
assisted derivation cannot widen its own confidence.

`--namespace` and `--facet` pair positionally; a `--facet` without a matching `--namespace` is exit 4. An unregistered namespace or name is exit 4 at projection time, before any append — ADR-009 makes
registration the choke point so a typo cannot create an unqueryable partition.

Refuting evidence is retained and linked, never dropped (ADR-003, `EvidenceRole::Refuting`). If any
candidate facet value breaches redaction, the affected value is dropped rather than derived, and the
`--json` response lists what was dropped under `"dropped"`. Silent omission would make a coverage loss
look like a clean run.

Empty selection is exit 5 with no record written.

### `facet`

Two read-only subcommands, named for the ports they call.

```text
evidra facet current --subject <S> --from <T> --to <T> [--namespace <NS>] [--facet <NAME>] [--json]
evidra facet count  --namespace <NS> --facet <NAME> --subject <S> --from <T> --to <T> [--json]
```

`current` calls `current_facets`, which resolves the supersedes chain so each superseded derivation
is represented exactly once by its current successor. Superseded rows remain stored and individually
retrievable; this decides which count as current. `count` calls `aggregate` and returns `FacetCount`
rows.

Both render as tab-separated tables with a header row and `.escape_debug()` on every cell, matching
`observation list` and the existing `table_output_escapes_control_characters` test.

### `relate`

```text
evidra relate --from <KEY> --to <KEY> --relation <KIND> [--confidence <BAND>] [--json]
```

`KEY` is prefixed to name which union arm is meant, because `EvidenceTarget` is a union with no SQL
foreign key and neither endpoint is validated by the schema:

```text
obs:01J...    der:01J...
```

A missing or unrecognised prefix is exit 4. `--confidence` defaults to `Weak`: an edge asserted by a
person at a terminal is a claim, and the honest default for a human assertion is not `Strong`.

`--relation supersedes` is exit 4 per Gap 3.

### `explain`

```text
evidra explain <DERIVATION_ID> [--json]
```

Renders, for one derivation: the record; its facets with slot and value; the full
`UncertaintyProfile` across all four ADR-004 dimensions as separate fields, never a composite score;
supporting and refuting evidence; the supersedes chain in both directions; and all relationships
touching it.

This is the command that makes ADR-003 observable. A system that separates evidence from claims is
only honest if a reader can reach the evidence, so `explain` is specified as a first-class surface
rather than a debug affordance.

It renders only stored values. `RedactedExcerpt` is redacted on the way in and `explain` never
reconstructs a removed substring; a stored facet value has already passed the ADR-010 derivability
check against its evidence set, so the render path has nothing left to leak. `explain` therefore needs
no independent redaction filter, and adding one would imply the stored layer was not already safe.

An unknown id is exit 6.

## Invariants

Numbered for citation by the conformance suites, per `docs/conformance.md` — which, as noted above,
does not exist yet.

- **CLI-01** The CLI contains no business rule. Every refusal originates in `evidra-core` or
  `evidra-engine`; the CLI maps an error to an exit code and renders it.
- **CLI-02** `derive` reads no clock. `--from` and `--to` are both required.
- **CLI-03** `recorded_at` is supplied by the CLI from the caller, never defaulted to now.
- **CLI-04** `--method assisted` cannot produce a confidence above `Weak`.
- **CLI-05** An unregistered namespace or facet name is refused before any append.
- **CLI-06** Refuting evidence is written with the same completeness as supporting evidence.
- **CLI-07** A value refused by the ADR-010 derivability check is reported as dropped, never silently
  omitted.
- **CLI-08** `relate --relation supersedes` is refused.
- **CLI-09** A supersedes chain containing a cycle is refused at write.
- **CLI-10** Every `--json` response carries its versioned schema tag.
- **CLI-11** Every table-rendered cell passes `.escape_debug()`.
- **CLI-12** An empty selection exits 5 and writes no record.
- **CLI-13** A refusal exits 4 and is distinguishable from a failure.

## Test obligations

Following the conventions already in `crates/evidra-cli/tests/cli.rs` — a `Command` helper, a
`TempDir` per test, behaviour named in the test name.

```text
derive_rejects_unregistered_namespace          exit 4
derive_refuses_assisted_strong_confidence      exit 4
derive_reports_dropped_facet_values            dropped list populated
derive_on_empty_scope_exits_five_and_writes_nothing
derive_is_deterministic_across_invocations     identical bytes, same inputs
derive_retains_refuting_evidence
facet_current_resolves_supersedes_chain
facet_count_matches_current_projection
relate_supersedes_is_refused                   exit 4
relate_requires_prefixed_keys                  exit 4
relate_rejects_self_edge                       exit 4
explain_renders_all_four_uncertainty_dimensions_separately
explain_lists_refuting_evidence
explain_unknown_id_exits_six                   exit 6
json_envelope_carries_versioned_schema_tag
table_output_escapes_control_characters        extends the existing test
```

The last two mirror existing tests and must extend rather than duplicate them.

## Out of scope

- **Inference.** No LLM or heuristic attribution. `derive` is deterministic projection over a
  selection. `--method assisted` exists so ADR-011's ceiling is enforced from the first commit rather
  than retrofitted.
- **Clustering.** `DerivationKind::Cluster` exists; no command produces it.
- **Retention.** Deferred in the plan. Superseded revisions accumulate without bound, which is why
  exit 6 exists for unresolvable chains rather than a silent walk.
- **Filesystem and Git/Cargo/CI importers.** Out of the port surface entirely.
- **`note --occurred-at`.** `record_note` passes `Utc::now()` and exposes no override, so manually
  recorded observations cannot be backdated. Selection windows therefore only reach as far back as the
  ingestion pipeline. Worth fixing, but it is a change to an existing command's contract and belongs
  in its own decision.

## Dependency order

1. Add `DerivationStore::get`, `ObservationStore::get`, `ObservationStore::select` to
   `evidra-core/src/ports.rs`, with sqlite implementations and conformance assertions. The scope
   filter in `select` must be the same predicate `current_facets` uses.
2. Extend `main.rs` with the exit-code mapping and the five commands.
3. Write the tests above red, then implement.

Step 1 is domain work in `evidra-core` and `evidra-store`, not CLI work. Task 6 cannot begin before
it, which is the substantive finding here: the plan describes a CLI task that is blocked on three
missing ports.

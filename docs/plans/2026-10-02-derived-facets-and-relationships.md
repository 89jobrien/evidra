# Plan: Derived Facets and Relationships

- Status: Draft
- Date: 2026-10-02
- Design: `docs/designs/2026-10-02-derived-facet-and-relationship-design.md`
- ADRs: ADR-008, ADR-009, ADR-010, ADR-011

## Goal

Fill the reserved `evidra-engine` boundary with the derived layer: banded facet projections over the
observation ledger and append-only relationships carrying confidence, scope, and evidence, so
recurring failure patterns become queryable without re-parsing every payload.

## Architecture

- **Crates affected**: `evidra-core` (domain and ports), `evidra-store` (schema v2 to v3 and
  migration), `evidra-engine` (deterministic derivation, currently a stub), `evidra-cli` (composition)
- **New traits/types**: `DerivationStore`, `RelationshipStore`, `Derivation`, `Relationship`,
  `UncertaintyProfile`, `ConfidenceBand`, `Freshness`, `Contradiction`, `ScopeFidelity`,
  `RelationKind`, `EvidenceRole`, `EvidenceTarget`, `DerivationScope`, `FacetValueSlot`,
  `FacetProjection`, `CurrentFacet`, `FacetCount`
- **Data flow**: observation ledger to scoped selection to banded facet projection to derivation with
  evidence links to indexed facet columns; relationships connect derivations and observations in
  either direction

## Tech Stack

- **Rust edition**: 2024, inherited from the workspace; MSRV 1.98
- **New dependencies**: none. `evidra-engine` depends on `evidra-core` and `chrono` only, and must
  not depend on `evidra-store`
- **Property tests**: `proptest` as a `[dev-dependencies]` workspace entry. This is the workspace's
  first property-testing dependency; it never enters a production dependency path
- **Diagnostics**: `miette` with `default-features = false, features = ["derive"]`. `thiserror`
  remains for the `Error` impl, which `miette` does not provide

## Tasks

Each task follows red, green, clippy, commit.

### Task 1: Core derived domain

**Status**: complete
**Crate**: `evidra-core`
**File(s)**: `crates/evidra-core/src/derivation.rs`, `crates/evidra-core/src/lib.rs`,
`crates/evidra-core/src/ports.rs`
**Run**: `cargo nextest run -p evidra-core`

1. Write failing tests for the four uncertainty dimensions, band parsing, evidence-set role rules,
   redacted `Debug`, and unknown-field rejection.
2. Implement `UncertaintyProfile`, `ConfidenceBand`, `Freshness`, `Contradiction`, `ScopeFidelity`,
   `EvidenceRole`, `EvidenceTarget`, `RelationKind`, `DerivationKind`, and the record types with
   private fields and validating constructors.
3. Add `DerivationStore` and `RelationshipStore` to `ports.rs`, including `current_facets` for
   supersedes-chain resolution.
4. Clippy clean, then commit.

`Derivation::new` takes a `DerivationDraft`, matching the existing `ObservationDraft` and
`AgentHarnessEventDraft` convention rather than exceeding clippy's argument limit. The example at
`crates/evidra-core/examples/derived_facets.rs` runs the domain end to end.

Two defects were found and fixed while writing this task, both by running the code rather than by
reading it:

- Deriving `Deserialize` writes private fields directly, so a stored record skipped every
  construction invariant. `Derivation` and `Relationship` now deserialize through `RawDerivation`
  and `RawRelationship` and re-run the same `validate` the constructor uses, matching
  `observation.rs`. Three regressions pin the smuggling cases.
- `EvidenceTarget::key` rendered the identity with `{id:?}`, producing
  `derivation:DerivationId(Ulid(...))` as an index key. It now uses the canonical ULID. The first
  test only asserted the prefix, which is why it passed; the regression asserts the whole string.

Property coverage landed in this task rather than Task 5, because derive-`Deserialize` turning out
to bypass validation is direct evidence that the serde surface here is a live bug source. Five
properties range over the accepted input space in `derivation::tests::props`: JSON round trip for
both record types, the assisted confidence cap stated as an equivalence rather than one direction,
`FacetValueSlot::of` agreeing with the value variant, and `Debug` output carrying neither a facet
value nor a subject identifier. Writing them immediately found a third defect: the strategy was
generating a `DerivationKind::Facet` record with no facets, which the domain correctly rejects.

Fuzzing is deferred. `DerivationId::parse` and the manual `Deserialize` impls are parser boundaries
and the natural fuzz targets, but `cargo-fuzz` is not installed. Nightly toolchains are available, so
this is a missing tool rather than a missing capability.

`policies/assumptions`, `policies/controls`, `policies/invariants`, and
`policies/accepted-risks` were filled in during this task as documentation only. That content is
Slice 4 material in `docs/ROADMAP.md`, pulled forward because the derived domain introduced
assumptions and accepted risks that were better written down than left implicit. Slice 4 still owns
compiling and integrity-checking them; nothing here is load-bearing yet.

### Task 2: Schema v3 and migration

**Status**: complete
**Crate**: `evidra-store`
**File(s)**: `crates/evidra-store/src/sqlite.rs`
**Run**: `cargo nextest run -p evidra-store`

1. Write a failing test asserting `migrate_v2_to_v3` does not change any observation document digest.
2. Add `derivations`, `derivation_facets`, `derivation_evidence`, and `relationships` with the three
   immutability triggers each, the facet slot `CHECK`, and the `from_key`/`to_key`/`relation`
   projections on `relationships`.
3. Add `validate_v3`, extend the migration ladder, keep `evidra init` as the only entry point.
4. Clippy clean, then commit.

Designing the schema exposed an inconsistency in Task 1 and forced a domain change.
`Derivation::facets` was `Vec<(String, FacetValue)>` while both `FacetFilter` and `CurrentFacet` are
namespace-and-name pairs, so a projected facet could be neither selected nor reported back.
`DerivationDraft::facets` is now `Vec<FacetProjection>`, a named type with its own validating
constructor and its own manual `Deserialize` for the same reason the record types have one. The
tuple would have round-tripped just as well; it would not have been readable.

Three deliberate decisions in the schema:

- The facet slot `CHECK` also asserts that only the value column matching the slot is populated.
  This is stronger than the domain, which refuses to construct the mismatch in the first place. A
  bug in a writer then cannot produce a row the domain would have rejected.
- `relationships` has no foreign key on `from_key` or `to_key`. Those columns may name either an
  observation or a derivation, so no single SQL foreign key can express the union. This is a
  deliberate hole, not an oversight, and edge integrity is checked by walking both ends in
  `validate_derived_rows` instead.
- `derivations.supersedes` is indexed because ADR-008 resolves the current view by walking the
  chain, and a chain that cannot be walked cheaply cannot be walked at all.

Nothing writes derived rows yet, so `validate_derived_rows` would otherwise pass vacuously and prove
nothing. The tests insert derivations and projections directly and then try to defeat the
validator: drifted facet values, missing facet rows, an indexed column contradicting its document, a
document that is not a domain record, and a trigger emptied out under its own name. Without those
the validation would look thorough while checking nothing.

Two defects in this task's own work, both caught by running rather than reading: an append-only
probe labelled "duplicate insert" that used a different primary key, so the trigger correctly did
not fire and the probe wrongly reported an unguarded table; and a migration test that compared
schema snapshots positionally when the snapshot is name-ordered, so adding tables shifted every
later position and looked like a rewrite.

Separately, `cargo nextest run` intermittently reports one leaky test. It is in the two
`concurrent_*` append-race tests introduced in the baseline commit, not in anything added here.
Worth fixing before it becomes CI noise.

### Task 3: Deterministic engine

**Status**: complete
**Crate**: `evidra-engine`
**File(s)**: `crates/evidra-engine/src/lib.rs`
**Run**: `cargo nextest run -p evidra-engine`

1. Write failing tests for band totality, monotonicity, cardinality, and edge mapping.
2. Implement the band tables and facet projection. Enforce the registered-namespace check at
   registration time, and the redaction-inheritance check on every emitted value (ADR-010).
3. Clippy clean, then commit.

`thiserror` and `miette` coexist rather than replace each other. miette's `Diagnostic` derive does
not implement `std::error::Error`, so removing `thiserror` would break `?` propagation into
`Box<dyn Error>` and the CLI in Task 6. `thiserror` supplies `Error`; `miette` supplies the code,
severity, help text, and rendering. Library crates take `miette` with `default-features = false,
features = ["derive"]`, which keeps the `fancy` rendering stack out of every library; the CLI will
opt into `fancy` when it starts rendering.

All nine error types now carry a `Diagnostic` derive and a per-variant code, 73 in total, all
namespaced `evidra::<type>::<variant>` and unique. Codes are a public contract the moment `--json`
exposes them, so the engine asserts its own are namespaced and unique, and cross-crate uniqueness
belongs in the Task 4 conformance suites.

Two decisions in the redaction check worth naming, because both could have been made more
permissively:

- A band name must be a snake-case identifier. A band named `the token was present` is prose, and
  prose is what gets scraped off an excerpt, so the table is refused at declaration. This makes the
  cardinality bound a soundness property rather than only a storage saving.
- The derivability test requires _every_ content word of the value to appear in one excerpt, where a
  content word is four characters or longer. That refuses `remote-socket-waiting` derived from an
  excerpt about a remote socket, which is the exposure ADR-010 is about. The cost is a real loss of
  coverage: `oom_kill` is refused when the excerpt says `killed`, because a substring test would
  have cleared it. ADR-010 accepts that loss explicitly, and `partially_overlapping_category_is_refused`
  pins the behaviour so a future loosening is a deliberate change.

  The check runs on every emitted value, as ADR-010 requires. What does not exist is the property test
  the ADR names for it — `no_facet_value_appears_in_source_excerpt` — so the four fixed cases above
  are the whole of the coverage. See A-18 in [`../AUDIT.md`](../AUDIT.md).

### Task 4: Conformance suites

**Status**: not started
**Crate**: `godmode` convention, applied per crate
**File(s)**: `crates/evidra-core/tests/conformance_derivation_domain.rs`,
`crates/evidra-store/tests/conformance_derivation_store.rs`,
`crates/evidra-store/tests/conformance_relationship_store.rs`,
`crates/evidra-store/tests/conformance_schema_migration.rs`,
`crates/evidra-engine/tests/conformance_derivation.rs`,
`crates/evidra-core/tests/conformance_architecture.rs`,
`crates/evidra-store/tests/conformance_policy_files.rs`
**Run**: `cargo nextest run --workspace`

1. Write `docs/conformance.md` with the numbered clauses the suites cite.
2. Add one shared assertion body per port with one thin `#[test]` per implementation, and every
   assertion message naming its clause.
3. Confirm the architecture suite reads the manifests and asserts that `evidra-core` declares no
   storage dependency and `evidra-engine` depends only on core.
4. Clippy clean, then commit.

### Task 5: Property suites

**Status**: partial — nine of the stated invariants exist
**Crate**: all three
**File(s)**: `property_uncertainty_profile.rs`, `property_supersedes_chain.rs`,
`property_store_invariants.rs`, `property_banding.rs`, `property_determinism.rs`
**Run**: `cargo nextest run --workspace`

None of the five named files exists. What does exist is nine `proptest` cases inline in two source
files — five in `evidra-core`'s `derivation::tests::props` (written in Task 1) and four in
`evidra-engine`'s `tests::props`. Naming the invariants and putting them in suite files is still the
better shape; the gap is the count, not the approach.

**One invariant named elsewhere does not exist at all.**
`no_facet_value_appears_in_source_excerpt` is cited by ADR-010, by Task 3's record below, and by two
design documents as the property checking the redaction guarantee. It is not written. See A-18 in
[`../AUDIT.md`](../AUDIT.md).

1. `proptest` is already a workspace dependency and a dev-dependency of `evidra-core`, added in Task
   1. The derived-domain properties are already written in `derivation::tests::props`; do not
      duplicate them here.
2. Write each remaining invariant to hold for arbitrary inputs, generating primitives and
   constructing records through the public validating constructor rather than implementing
   `Arbitrary` on the records.
3. Commit every `proptest-regressions/` seed produced.
4. Clippy clean, then commit.

### Task 6: CLI composition

**Status**: blocked — see below
**Crate**: `evidra-cli`
**File(s)**: `crates/evidra-cli/src/main.rs`, `crates/evidra-cli/tests/cli.rs`
**Run**: `cargo nextest run -p evidra-cli`

Blocked on a port slice that is not in this plan. `DerivationStore` and `RelationshipStore` have no
implementor, and `ObservationStore` has neither `get` nor `select`, so `derive` cannot select its
evidence set and `explain` cannot fetch the record it is asked to explain. The surface is specified in
[`../designs/2026-10-03-derived-cli-surface-design.md`](../designs/2026-10-03-derived-cli-surface-design.md),
which also records why writing the argument and exit-code tables first would have produced a document
that reads complete and implements nothing.

That specification's Step 1 — `get` on both stores, `select` on `ObservationStore`, sqlite
implementations, and seven conformance clauses — belongs in this plan as Task 7, in `evidra-core` and
`evidra-store`. It is domain work, not CLI work, and Task 6 cannot begin before it.

1. Write failing tests for `derive`, `facet`, `relate`, and `explain`, including exit codes and the
   `--json` schema.
2. Implement the subcommands, keeping the CLI a composition root with no logic of its own.
3. Clippy clean, then commit.

## Sequencing

Tasks 1 and 2 are independent and can proceed in either order. Task 3 depends on Task 1 for the
domain types. Task 4 depends on Tasks 1 and 2. Task 5 depends on Tasks 1 through 3.

Task 7 (the port slice specified in
[`../designs/2026-10-03-derived-cli-surface-design.md`](../designs/2026-10-03-derived-cli-surface-design.md))
depends on Tasks 1 and 2, and Task 6 depends on Task 7. Task 6 therefore cannot be the last task that
names no predecessor — as originally drawn it listed no dependency that would have surfaced the gap.

## Done when

Eleven conformance sections and twenty-one property invariants pass, `cargo clippy --workspace
--all-targets -- -D warnings` is clean, and `validate_v3` asserts the append-only guarantee on every
derived table.

**Not met.** `validate_v3` does assert the append-only guarantee on every derived table, and clippy
is clean. The other two conditions are not satisfied: zero conformance sections exist against eleven
required, and nine property invariants exist against twenty-one. This condition was previously
reported as met; see A-17 in [`../AUDIT.md`](../AUDIT.md) for what was actually counted.

## Not in this plan

Decision capture, failure attribution, policy evaluation, and trajectory export are separate slices
in `docs/ROADMAP.md`. Retention for derived records is deferred and unaddressed here, which means
superseded revisions accumulate without bound until a policy exists.

Fuzz targets are named but not built. `DerivationId::parse` and both manual `Deserialize` impls parse
untrusted input and are where a hand-written validator is most likely to be wrong in a way the
property suites cannot reach. Installing `cargo-fuzz` is the whole of the blocker; nightly is
already present. This belongs with Task 5 rather than after it, since fuzzing the read path is the
cheapest way to find a validation gap before an append-only store depends on it.

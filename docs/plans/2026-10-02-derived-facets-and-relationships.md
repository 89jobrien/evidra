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
  `CurrentFacet`, `FacetCount`
- **Data flow**: observation ledger to scoped selection to banded facet projection to derivation with
  evidence links to indexed facet columns; relationships connect derivations and observations in
  either direction

## Tech Stack

- **Rust edition**: 2024, inherited from the workspace; MSRV 1.98
- **New dependencies**: none. `evidra-engine` depends on `evidra-core` and `chrono` only, and must
  not depend on `evidra-store`
- **Property tests**: `proptest` as a `[dev-dependencies]` workspace entry. This is the workspace's
  first property-testing dependency; it never enters a production dependency path

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

**Crate**: `evidra-store`
**File(s)**: `crates/evidra-store/src/sqlite.rs`
**Run**: `cargo nextest run -p evidra-store`

1. Write a failing test asserting `migrate_v2_to_v3` does not change any observation document digest.
2. Add `derivations`, `derivation_facets`, `derivation_evidence`, and `relationships` with the three
   immutability triggers each, the facet slot `CHECK`, and the `from_key`/`to_key`/`relation`
   projections on `relationships`.
3. Add `validate_v3`, extend the migration ladder, keep `evidra init` as the only entry point.
4. Clippy clean, then commit.

### Task 3: Deterministic engine

**Crate**: `evidra-engine`
**File(s)**: `crates/evidra-engine/src/lib.rs`
**Run**: `cargo nextest run -p evidra-engine`

1. Write failing tests for band totality, monotonicity, cardinality, and edge mapping.
2. Implement the band tables and facet projection. Enforce the registered-namespace check at
   registration time, and the redaction-inheritance check on every emitted value (ADR-010).
3. Clippy clean, then commit.

### Task 4: Conformance suites

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

**Crate**: all three
**File(s)**: `property_uncertainty_profile.rs`, `property_supersedes_chain.rs`,
`property_store_invariants.rs`, `property_banding.rs`, `property_determinism.rs`
**Run**: `cargo nextest run --workspace`

1. `proptest` is already a workspace dependency and a dev-dependency of `evidra-core`, added in Task
   1. The derived-domain properties are already written in `derivation::tests::props`; do not
      duplicate them here.
2. Write each remaining invariant to hold for arbitrary inputs, generating primitives and
   constructing records through the public validating constructor rather than implementing
   `Arbitrary` on the records.
3. Commit every `proptest-regressions/` seed produced.
4. Clippy clean, then commit.

### Task 6: CLI composition

**Crate**: `evidra-cli`
**File(s)**: `crates/evidra-cli/src/main.rs`, `crates/evidra-cli/tests/cli.rs`
**Run**: `cargo nextest run -p evidra-cli`

1. Write failing tests for `derive`, `facet`, `relate`, and `explain`, including exit codes and the
   `--json` schema.
2. Implement the subcommands, keeping the CLI a composition root with no logic of its own.
3. Clippy clean, then commit.

## Sequencing

Tasks 1 and 2 are independent and can proceed in either order. Task 3 depends on Task 1 for the
domain types. Task 4 depends on Tasks 1 and 2. Task 5 depends on Tasks 1 through 3. Task 6 depends on
all of them.

## Done when

Eleven conformance sections and twenty-one property invariants pass, `cargo clippy --workspace
--all-targets -- -D warnings` is clean, and `validate_v3` asserts the append-only guarantee on every
derived table.

## Not in this plan

Decision capture, failure attribution, policy evaluation, and trajectory export are separate slices
in `docs/ROADMAP.md`. Retention for derived records is deferred and unaddressed here, which means
superseded revisions accumulate without bound until a policy exists.

Fuzz targets are named but not built. `DerivationId::parse` and both manual `Deserialize` impls parse
untrusted input and are where a hand-written validator is most likely to be wrong in a way the
property suites cannot reach. Installing `cargo-fuzz` is the whole of the blocker; nightly is
already present. This belongs with Task 5 rather than after it, since fuzzing the read path is the
cheapest way to find a validation gap before an append-only store depends on it.

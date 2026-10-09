# Audit

Findings from a line-by-line reading of the workspace against `AGENTS.md` and
[`CONVENTIONS.md`](CONVENTIONS.md), with the disposition of each one.

The point of this document is that the findings used to exist only as `TODO` comments scattered
across nine source files. That is the worst of both worlds: they cannot be triaged, they cannot be
tracked to completion, and the three that mattered most sat next to code that looked correct. Each
finding below carries an id, a severity, the location it was found at, and what — if anything — was
done about it. The `TODO` comments remain at their sites; this document is the index.

Severity here means consequence if the code were reached, not effort to fix.

- **CRITICAL** — a correctness, confidentiality, or soundness hole on a path that is reachable or
  that a document already promises.
- **HIGH** — a soundness gap, a promise the code does not keep, or a convention resting on nothing
  but discipline where a lint exists.
- **MEDIUM** — a boundary contradiction, or dead surface that implies a guarantee that does not
  exist.
- **LOW** — duplication or a known structural limitation with no current consequence.

## Tracking

Each open finding is tracked as a GitHub issue, listed below. The mapping is not one-to-one: the
twenty findings are tracked by eighteen issues, because the three dead-accessor findings (A-13) and
the two duplicate-test findings (A-16) are each split by code location — a marker per site is the
workspace's convention, so the issue tracker mirrors the source rather than the work.

| Finding | Issue                                                                                                                                                      |
| ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A-04    | [#9](https://github.com/89jobrien/evidra/issues/9)                                                                                                         |
| A-05    | [#11](https://github.com/89jobrien/evidra/issues/11)                                                                                                       |
| A-06    | [#8](https://github.com/89jobrien/evidra/issues/8)                                                                                                         |
| A-07    | [#10](https://github.com/89jobrien/evidra/issues/10)                                                                                                       |
| A-08    | [#13](https://github.com/89jobrien/evidra/issues/13)                                                                                                       |
| A-09    | [#5](https://github.com/89jobrien/evidra/issues/5)                                                                                                         |
| A-10    | [#7](https://github.com/89jobrien/evidra/issues/7), [#12](https://github.com/89jobrien/evidra/issues/12)                                                   |
| A-11    | [#17](https://github.com/89jobrien/evidra/issues/17)                                                                                                       |
| A-12    | [#18](https://github.com/89jobrien/evidra/issues/18)                                                                                                       |
| A-13    | [#2](https://github.com/89jobrien/evidra/issues/2), [#3](https://github.com/89jobrien/evidra/issues/3), [#4](https://github.com/89jobrien/evidra/issues/4) |
| A-14    | [#1](https://github.com/89jobrien/evidra/issues/1)                                                                                                         |
| A-15    | [#6](https://github.com/89jobrien/evidra/issues/6)                                                                                                         |
| A-16    | [#15](https://github.com/89jobrien/evidra/issues/15), [#16](https://github.com/89jobrien/evidra/issues/16)                                                 |
| A-18    | [#14](https://github.com/89jobrien/evidra/issues/14)                                                                                                       |

Sixteen of those are auto-tracked: `taskit protocol todo-sync` reads the `TODO` markers in source and
maintains the mapping in `taskit-todo-sync.lock`. **That lockfile is the linkage and must be
committed** — without it a fresh clone reports all sixteen markers as new and opens duplicates.

A-11 and A-12 live in `Cargo.toml`, which the scanner does not read, so they are hand-written. Do not
add `(#N)` citations into marker text: the sync keys on the marker string, so editing it makes the
marker look new and offers to close the issue it cites.

### Beyond the audit

Six items were found while writing this document that are not audit findings — they predate it, or are
recorded in the plan and roadmap rather than in source. They have no code marker, so they are
hand-written like A-11 and A-12.

| Item                                                               | Issue                                                |
| ------------------------------------------------------------------ | ---------------------------------------------------- |
| Task 4, conformance suites — two of thirteen exist (§12, §13)      | [#19](https://github.com/89jobrien/evidra/issues/19) |
| Task 5, property suites — nine of twenty-one exist                 | [#20](https://github.com/89jobrien/evidra/issues/20) |
| `cargo-fuzz` targets exist for the ingestion boundary only         | [#21](https://github.com/89jobrien/evidra/issues/21) |
| ADR-008 through ADR-011 still `Proposed` with their code landed    | [#22](https://github.com/89jobrien/evidra/issues/22) |
| `obfsck` is a path-only dependency, blocking any crates.io release | [#23](https://github.com/89jobrien/evidra/issues/23) |
| No taskit protocol-drift surface over `ports.rs`                   | [#24](https://github.com/89jobrien/evidra/issues/24) |

The last one is worth noting against the port slice: `crates/evidra-core/src/ports.rs` is the
workspace's highest-value drift surface and `taskit protocol drift` reports "no surfaces configured,
skipping" because there is no `taskit.toml` at all. The port slice will change all three traits, and
without a lock a signature change is caught only by the compiler at the consuming site.

## Closed

Three findings were defects with contained fixes. Each is now pinned by a regression test, so the
same omission cannot return unnoticed.

### A-01 — `Provenance` printed producer identity and policy names (CRITICAL, closed)

`crates/evidra-core/src/observation.rs`. The type derived `Debug`, so
`Provenance { collector: "evidra-cli", transformations: ["gpt-redact"] }` reached logs, panic
messages, and test output verbatim. `AGENTS.md` names provenance explicitly as evidence-bearing and
requires a redacted `Debug`.

It survived a clean clippy run and a passing test suite because `Observation` and `ObservationDraft`
both redact every field, so no envelope-level assertion could observe it — the leak was one type
deep, and every test that touched provenance went through a redacting parent. The `Debug` audit is
only as good as the shallowest type it reaches.

Now a hand-written `Debug` emitting `Provenance { .. }`, with
`provenance_debug_redacts_collector_and_transformations` pinning it.

Worth noting: commit `62e9938` ("give AgentHarnessJsonlSource a redacted Debug") described this as
the sole remaining gap in the workspace. That claim was not verified, and it was false.

### A-02 — `CurrentFacet` skipped every construction invariant on read (CRITICAL, closed)

`crates/evidra-core/src/derivation.rs`. `Deserialize` was derived, so a stored `derivation_facets`
document with `"namespace": ""` or `"name": ""` deserialized into a value `CurrentFacet::new`
refuses. This is exactly the hole the `Raw`-shadow rule in `CONVENTIONS.md` §2 exists to close, and
every other persisted type in the module closes it.

Now `RawCurrentFacet` with `deny_unknown_fields`, re-entering `CurrentFacet::new`, with
`deserialization_cannot_smuggle_blank_current_facet` pinning it.

`FacetCount` is correctly exempt and is not flagged: its constructor is infallible, so it has no
invariant to re-run. That distinction is what makes the exemption for `FacetCount` defensible and
the omission in `CurrentFacet` a defect rather than a pattern.

### A-03 — `UnorderedBands` was unreachable (HIGH, closed)

`crates/evidra-engine/src/lib.rs`. `BandTable::new` documented that it returns
`EngineError::UnorderedBands`, and the variant carried a diagnostic code. No code path produced it:
`new` had no ordering check, so a descending table surfaced as `IncompleteCoverage` ("bands overlap"
or "bands leave a gap") or through the `i64::MIN` and unbounded-last edge checks. The code existed
only as a claim.

`new` now checks ascending lower bounds ahead of the coverage checks, so a mis-ordered table reports
its actual cause. `descending_bands_are_refused_as_unordered` pins it.

The test that enumerates every `EngineError` variant to assert unique diagnostic codes structurally
cannot distinguish a reachable variant from a merely-listed one, so it would not have caught this in
either direction. Reachability needs its own assertion.

## Documentation drift

Four claims in the documentation were false when checked. Recorded separately because the failure
mode is different: these are not code defects, they are places where a document told a reader to
trust something that was not there.

### A-17 — the Slice 1 gate is claimed as met and is not (CRITICAL, closed)

`docs/ROADMAP.md` states "**Gate:** eleven conformance sections and twenty-one property invariants,
all passing." The derived-facets plan repeats it under "Done when."

Neither half holds. There is no `docs/conformance.md` and no `tests/conformance_*.rs` in any crate —
zero conformance sections, against eleven claimed. The property suite is real but smaller: nine
`proptest` cases (five in `evidra-core`, four in `evidra-engine`) against twenty-one claimed.

The plan's own Task 4 lists seven conformance files as deliverables, and Task 5 lists five property
files. None exist. Tasks 4 and 5 carried no `**Status**` line at all, which is why the plan reads as
though only Tasks 1 through 3 exist — and Tasks 1 through 3 are the only ones with a Status line.

A gate that is recorded as passed and was never run is worse than an absent gate: it converts
"unverified" into "verified" for every reader who trusts the roadmap.

Corrected. ROADMAP and the plan now state what exists and what does not. The counts have moved since:
`docs/conformance.md` now exists and §12 (`ObservationStore`) and §13 (`RedactionPolicy`) are enforced
by one shared assertion body each, run against both the production implementation and a reference
implementation written from the clauses. §1–§11 remain unwritten.

### A-18 — ADR-010's acceptance criterion is cited by three documents and does not exist (HIGH, open)

`no_facet_value_appears_in_source_excerpt` is named as the property test that mechanically checks
ADR-010. It appears in ADR-010's own Consequences, in the plan's Task 3 record, and in the
`2026-10-03` and `2026-10-04` design docs, which both lean on it as evidence that the constraint is
enforced rather than merely stated.

No such property exists anywhere in the workspace.

The redaction check itself is real and is exercised — `verbatim_category_from_an_excerpt_is_refused`,
`derivable_category_from_an_excerpt_is_refused`,
`partially_overlapping_category_is_refused`, and `genuine_category_survives_redaction_review` are
fixed cases, and `evidra-engine` runs the check on every emitted value as ADR-010 requires. What is
missing is the property: the fixed cases cannot say the check holds for arbitrary evidence sets,
which is the claim four documents make on its behalf.

This is the most expensive single finding to leave open, because ADR-010's coverage cost —
"records that cannot be redacted without losing their analytic value must be dropped rather than
derived" — is a deliberate trade the project agreed to. A trade like that needs its guard tested.

### A-19 — `CONVENTIONS.md` §5 named the wrong type (MEDIUM, closed)

The append-only section credited `CurrentFacet::new` with refusing a self-supersession. That check
lives on `Derivation::new`; `CurrentFacet` has no `supersedes` field, so there is nothing for it to
refuse. Corrected.

### A-20 — `CONVENTIONS.md` §7 claimed zero `expect` (HIGH, closed)

It claimed production code has zero `unwrap` and zero `expect`. There is one `expect`, on the ADR-010
path — the same one as A-08. Corrected, and the section now names it.

This matters more than a stale sentence. A convention document that reports "zero" against an
unenforced rule is the reason the unenforced rule was never questioned: the document asserted the
state was clean.

### Open — blocking

These four are one piece of work, not four. They are specified in
[`designs/2026-10-03-derived-cli-surface-design.md`](designs/2026-10-03-derived-cli-surface-design.md)
as its Steps 1 through 3, and that document is the specification; this entry is the index into it.

### A-04 — `DerivationStore` has no implementor and no consumer (CRITICAL, open)

`crates/evidra-core/src/ports.rs`. Schema v3 carries `derivations`, `derivation_facets`, and
`derivation_evidence`, and only raw-SQL helpers inside `evidra-store`'s test module ever populate
them. The derived layer — the entire reason schema v3 exists — is persisted yet unreachable through
its own declared interface. `DerivationStore` is the only port in the workspace with no
implementation, and the derived slice is reachable only through the library API.

### A-05 — `RelationshipStore` has no implementor and no consumer (CRITICAL, open)

Same file. `relationships` exists in v3 and is populated only by a raw-SQL helper in a test module.
The `derive`, `relate`, and `explain` CLI work is blocked on this port as well as on `DerivationStore`.

### A-06 — `ObservationStore` cannot read one record or select a scope (CRITICAL, open)

Same file. There is no `get(&ObservationId)` and no `select(&DerivationScope)`. `list(limit)` is the
only read path — newest-first, unfiltered, a display query rather than a selection primitive.

`DerivationScope` carries a subject, a closed date window, and a `Vec<FacetFilter>`, and
`current_facets` and `aggregate` both take one, but nothing can _execute_ a scope against
observations. So `derive` cannot select its evidence set, and a user-supplied `--from`/`--to` window
would silently derive from whatever `list` returned.

`select` must be the same predicate `current_facets` uses, not a second SQL statement that happens
to agree. If they diverge, a derivation's evidence set and the rows its scope considers current
disagree, and the derivation cites evidence the current view no longer recognises — making it
unfalsifiable. Both queries would be individually well-formed, so the v3 validator cannot see it.

### A-07 — `current_facets` documents a retrieval guarantee the trait cannot honour (HIGH, open)

Same file. The rustdoc says superseded rows "remain stored and remain individually retrievable; this
method decides which ones count as current." There is no `get(&DerivationId)`, so a caller holding an
id cannot retrieve a single derivation at all. Both read paths are collection-shaped. Either add the
accessor or correct the doc.

## Open — soundness

### A-08 — the only production `expect` sits on the redaction-inheritance path (CRITICAL, open)

`crates/evidra-engine/src/lib.rs`, `Evidence::strictest_redaction`. ADR-010's constraint — that a
derived record may not weaken any redaction its evidence carries — is load-bearing in fact: the
engine runs the derivability check on every emitted value, and four fixed cases exercise it. (The
property that four documents claim pins it does not exist — see A-18.) But the union of the evidence's
attestations is rebuilt through
`RedactionRecord::new(...).expect("an attested redaction union stays valid")`. A failed invariant
aborts the process instead of surfacing a diagnosable error, on the one path whose failure means the
redaction guarantee did not hold.

The fallibility belongs in the signature: `Result<Option<RedactionRecord>, EngineError>`, propagated
so callers must decide what a broken redaction union means.

Why it survived review: `AGENTS.md` forbids five things — `unwrap`, `expect`, `panic`, `todo`,
`unimplemented` — and only three are lint-enforced. See A-11.

### A-09 — derived records have no tamper detection (HIGH, open)

`crates/evidra-core/src/derivation.rs`, `Derivation::content_hash`. It is called only from tests, the
proptests, and the example — never from production code. The `derivations` table has no digest column
and `evidra-store` never verifies the hash on read, whereas observations _are_ digest-verified on
every load through `Observation::verify_integrity`.

This is a soundness gap rather than dead code. `ARCHITECTURE.md` grounds the whole design in "a
reader cannot trust the file, so `open()` probes" — the probe re-deserializes every stored document
through its domain type and checks indexed columns against them, but a derived row whose document was
swapped for another _valid_ document is not detected. Either add a digest column verified on read, or
remove the method so it stops implying a guarantee the store does not provide.

### A-10 — two evidence-bearing types redact only by accident (HIGH, open)

`crates/evidra-core/src/ingest.rs`, `ClaimedHarnessContent`. Its `Event` variant wraps a
`Box<AgentHarnessEvent>`, so the type carries evidence and AGENTS.md requires a redacted `Debug`. It
prints only field types today because the inner event's own `Debug` redacts.

`crates/evidra-engine/src/lib.rs`, `Evidence<'a>`. Holds excerpts and redaction attestations. Nothing
leaks today for the same reason — both inner types redact themselves.

Both are safe by coincidence rather than by declaration, and both break the moment a third field is
added. `impl_redacted_debug!` covers the first case directly. The macro now lives in
`crates/evidra-core/src/macros.rs` — moved out of `harness.rs` and declared before the domain modules
so every module in `evidra-core` shares one audited definition — but it is still a crate-internal
`macro_rules!` with no `#[macro_export]`, so `evidra-engine` cannot reach it. The engine case needs
either an export or a hand-written impl, unconstrained on `'a`.

### A-11 — three forbidden constructs are unenforced (HIGH, open)

`Cargo.toml`. `AGENTS.md` forbids five things in production code. `unsafe_code` is `forbid`;
`todo`, `unimplemented`, and `dbg_macro` are `deny`. `unwrap_used`, `expect_used`, and `panic` are
held by discipline alone.

That is the direct cause of A-08: the single production `expect` passed a clean `-D warnings` clippy
run. The fix is to add the three lints and scope
`#![allow(clippy::unwrap_used, clippy::expect_used)]` to each `mod tests` — the workspace has 550+
`expect` calls, all but one of them in test code.

`missing_docs` is a second instance of the same disease. `PRD.md` claims it is enabled. It is not,
and none of the four lib crates declare it, so nothing checks the public API against its own docs.

`CONVENTIONS.md` §7 currently asserts production code has zero `unwrap` and zero `expect`. That claim
is false. Corrected as part of this pass.

### A-12 — `evidra-engine` is missing from `[workspace.dependencies]` (MEDIUM, open)

`Cargo.toml`. It is a workspace member, but no entry exists. Harmless today because no crate
consumes it; it will bite whoever wires the engine into the CLI and reaches for
`evidra-engine.workspace = true`.

## Open — dead surface

### A-13 — three accessors with zero call sites (MEDIUM, open)

`crates/evidra-core/src/derivation.rs`. `DerivationMethod::kind_str`, `EvidenceTarget::kind_str`, and
`DerivationScope::selection` have no callers anywhere in the workspace, tests included. All three
exist for a store adapter that does not exist yet (A-04, A-05), and all three currently duplicate
values the store carries as SQL literals — `kind IN ('facet','aggregate','cluster')` in the first
two cases.

The risk is not the dead code; it is that a future adapter may re-implement the spelling as literals
and leave these two sources of truth disagreeing. Either wire the adapter to call them, or delete them
before someone assumes they are in use because they are public and documented.

### A-14 — CLI test fixtures bypass the store port (MEDIUM, open)

`crates/evidra-cli/tests/cli.rs`. The v1/v2 migration fixtures open `rusqlite` directly, which is why
`rusqlite` is a dev-dependency of the CLI at all. Two consequences: it contradicts the AGENTS.md
rule that external systems stay behind ports defined in `evidra-core`, and the fixtures assert
against a `V1_SCHEMA` literal the test file itself defines — so a drift in `evidra-store`'s real
migration would not fail these tests.

A migration fixture that does not exercise the migration is worse than no fixture, because it reads as
coverage. These should build through `evidra-store`.

### A-15 — `FacetValue` cannot honour `deny_unknown_fields` (LOW, open)

`crates/evidra-core/src/harness.rs`. `#[serde(untagged)]` structurally cannot reject unknown fields,
making this the one place in the workspace where the §2 guarantee is impossible by construction.
Harmless while all three variants are scalars. A future non-scalar variant would begin silently
accepting documents that every other persisted type rejects — which is the coverage cost ADR-010
already accepts elsewhere, so the honest resolution is probably to document the exemption rather
than fight the derive.

### A-16 — two unit tests duplicate identically-named proptests (LOW, open)

`crates/evidra-engine/src/lib.rs`. `band_is_total_over_a_dense_range` and `banding_is_monotonic` are
fixed-range examples; `tests::props` has stronger property versions of the same two properties under
the same names. Keep the fixture-specific counts, let the proptests own the general properties.

## How this audit was produced, and what it missed

Recorded because an audit that overstates its coverage is worse than none.

This was a reading pass against the written conventions, not an execution pass. Every defect above was
found by comparing a claim in `AGENTS.md`, `CONVENTIONS.md`, or a rustdoc contract against the code
it described. That method has a specific blind spot: **it can only find violations of rules that were
written down.** Three of the four gaps here existed because nothing had claimed those paths were
supposed to work — the ports with no implementor are a gap in a spec, not a violation of one.

What that means in practice:

- The closed findings were all reachable. The open ones are mostly not, which is why none of them
  broke a test.
- A test suite passing is not evidence against anything in the Open sections. Those paths have no
  tests because they have no implementation.
- The strongest available check on the remaining parser paths is fuzzing. Two `cargo-fuzz` targets now
  cover the external harness-ingestion boundary — the bounded JSONL decoder and Stage 1 transcript
  minimisation — with a committed corpus so a finding stays replayable. `DerivationId::parse` and every
  manual `Deserialize` impl still parse untrusted input and are exactly where a hand-written validator
  is most likely to be wrong in a way the property suites cannot reach. Adding a target for them is
  now a matter of writing the file, not of installing `cargo-fuzz`.

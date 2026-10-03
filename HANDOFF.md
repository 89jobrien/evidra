# HANDOFF — Rust API audit of evidra

**Date:** 2026-10-03
**Branch:** `main` (tracks `origin/main`)
**HEAD:** `f80a00e` feat(evidra-engine): add band tables and facet projection
**Scope of this session:** read-only API audit of all 5 crates, plus 2 mechanical fixes.
**Test status at time of writing:** `cargo nextest run --workspace` → **198 passed, 0 failed**.

---

## ⚠️ Note on concurrent work during this session

A second agent was editing `evidra-*` crates throughout this audit. It has since finished. Two
commits landed while this document was open: `adc99cf` (schema v3 + derived tables) and `f80a00e`
(engine band tables and facet projection).

One consequence worth recording: **that worker reverted the `Debug` / `#[must_use]` change described
below** when it edited `agent_harness_jsonl.rs`. The edit was re-applied afterwards, once the worker
finished, and is the current uncommitted state. Everything else below reflects the committed tree at
`f80a00e`.

**Files currently modified — all of them mine:**

```
 M AGENTS.md                                     architecture, rules, persistence sections
 M README.md                                     evidra-engine row
 M docs/ROADMAP.md                               Slice 1 landed state
 M crates/evidra-adapters/src/agent_harness_jsonl.rs   Debug + #[must_use]
?? HANDOFF.md                                    this file
```

---

## What I changed

`crates/evidra-adapters/src/agent_harness_jsonl.rs` — two mechanical C-code fixes:

1. **C-DEBUG** — added `impl<R> fmt::Debug for AgentHarnessJsonlSource<R>` (line ~235). Deliberately
   **unconstrained on `R`** so a source stays debuggable over any reader, including readers that are
   not themselves `Debug` (`#[derive(Debug)]` would have added an unwanted `R: Debug` bound).
   Redacted via `finish_non_exhaustive()`, matching the existing pattern in
   `ClaimedHarnessFile` / `AgentHarnessFileInbox`.
2. **`#[must_use]`** on `AgentHarnessJsonlSource::new` — the only public constructor in the
   workspace missing it (the other ~140 accessors all have it).

No other files touched. Everything else was filed, not fixed.

---

## Doob todos filed (12)

Query with `doob todo list --project evidra`, tagged `rust-audit`.

| Pri    | UUID                                   | Finding                                                                                                                                                                                                                  |
| ------ | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| ~~P1~~ | `77de22de-eb9a-438a-9c58-9028e0fab24a` | **STALE — now resolved, close this.** `evidra-store` tests did not compile (missing `Relationship`, `RelationshipId`, `EvidenceTarget`, `RelationKind` imports). Fixed by the other worker in `adc99cf`; tests now pass. |
| P2     | `93914066-e82f-4dba-8b87-b2b903aa3776` | `SqliteObservationStore` is `pub` with no `Debug`. **Re-verified still valid.**                                                                                                                                          |
| P2     | `1423bb8b-c294-4138-9395-956ed13b576e` | `AgentHarnessEventIdentity::new` takes 3 same-typed positional args; swapping two compiles silently and corrupts idempotency identity.                                                                                   |
| P3     | `4f50e6db-5869-4049-b0a4-fe3fb3187c65` | `DerivationId::as_str` / `RelationshipId::as_str` return owned `String` despite `as_` implying a borrow. **Re-verified at :33 and :76.**                                                                                 |
| P3     | `74e7fb5c-f6da-400a-a618-34edbe8d528a` | `kind_str` non-idiomatic suffix. **Re-verified at :325, :646.**                                                                                                                                                          |
| P3     | `bc5409c0-8ff0-418e-819a-9030365347f1` | `FacetValueSlot::of` uses neither `as_` nor `from_`. **Re-verified at :483.**                                                                                                                                            |
| P3     | `908771c9-bee4-4d80-babe-48ddefa6bdc0` | `CurrentFacet::new` — 6 positional args incl. a bare `bool` and a swappable `(namespace, name)` pair.                                                                                                                    |
| P3     | `d47ef5d9-6998-441f-8c08-df75c8fe147a` | No crate-level rustdoc example in store / adapters / engine (core has one).                                                                                                                                              |
| P4     | `27236430-f5f5-4555-b30d-c0f08007f126` | `Relationship::new` takes 7 positional args (C-BUILDER).                                                                                                                                                                 |
| P4     | `aabc5fa4-0fbc-43e9-9d51-56403d70dd9f` | No `repository` / `keywords` / `categories` in any lib manifest (inert while `publish = false`).                                                                                                                         |
| P4     | `33e52871-2bea-4c81-b182-238516b2d8f9` | `Derivation::deserialize` / `Relationship::deserialize` lack the `///` doc every sibling impl carries.                                                                                                                   |
| P4     | `c7b09bf6-dce9-4489-b0e1-968c0634acf7` | `DerivationStore` / `RelationshipStore` declared but unimplemented anywhere — confirm ports-first intent or add a conformance fake.                                                                                      |

---

## Recommendations, in priority order

### 1. Turn on lints that already pass (near-zero cost, pure regression insurance)

`[workspace.lints]` in `Cargo.toml:33-39` has only **four** entries, while `AGENTS.md` states ~15
rules. I measured the gap — but note the scope: this ran against
`-p evidra-core -p evidra-store -p evidra-adapters` only. **`evidra-engine` (landed later, `f80a00e`)
and `evidra-cli` were not measured.**

```
missing_docs               → 0 findings
unreachable_pub             → 0 findings
clippy::unwrap_used         → 0 findings   (lib code)
clippy::expect_used        → 0 findings   (lib code)
clippy::panic              → 0 findings   (lib code)
```

(Confirmed the flags actually reach rustc — a bogus lint name errors, so these are real zeros and
not a silent no-op.)

```toml
[workspace.lints.rust]
missing_docs    = "deny"   # verified green
unreachable_pub = "deny"   # verified green

[workspace.lints.clippy]
unwrap_used  = "deny"   expect_used = "deny"   panic = "deny"
```

**Catch:** `unwrap_used`/`expect_used` must be scoped with
`#![cfg_attr(not(test), deny(...))]` or the ~200 legitimate `.expect(...)` calls inside
`#[cfg(test)]` modules will fail the build.

_Caveat:_ I also probed `missing_debug_implementations` and it came back clean, which contradicts
the confirmed absence of `Debug` on `SqliteObservationStore`. I could not get that lint to fire.
Check it manually before relying on it to catch the P2 finding.

### 2. Close the read-path validation gap (biggest real correctness risk)

`Derivation`, `Relationship`, `FacetProjection`, `SourceRef`, `SubjectRef`, `IntegrityRecord` and
every `Persisted*` type each hand-roll the same pattern: a `Raw` shadow struct +
`#[serde(deny_unknown_fields)]` + a manual `Deserialize` that re-runs invariants on the read path.

This is genuinely excellent work and the store tests cover it well
(`derived_unreadable_document_is_rejected`, `deserialization_cannot_smuggle_ungated_confidence`,
etc.). But it is enforced **only by review** — a new persisted type can skip it and silently
bypass every check on untrusted input.

- `#[serde(try_from = "...")]` gives the re-validating deserialize without the boilerplate.
- A single test asserting _every_ persisted type rejects a tampered round-trip would pin the
  convention mechanically.

### 3. Document the clock-seam asymmetry between the two layers

`derivation.rs:12-13` states the intent explicitly: _"`recorded_at` is supplied by the caller
rather than read from the clock, so that identical inputs produce an identical record and
determinism is testable rather than aspirational."_ The derivation layer delivers — `DerivationDraft`
takes `id` and `recorded_at`, and `identical_inputs_yield_identical_digest` proves it.

The observation layer takes neither:

```rust
// observation.rs:409-411
pub fn record(draft: ObservationDraft) -> Result<Self, ObservationError> {
    let id = ObservationId::new();          // Ulid::new() — clock + RNG
    let observed_at = Utc::now();           // wall clock
```

So `Observation` is **not** deterministically testable while `Derivation` is. Given ADR-008/009/011
are cited throughout `derivation.rs`, the asymmetry is probably deliberate — but it is undocumented,
so a future contributor will assume both layers share the property. Either add an
`ObservationDraft { id, observed_at }` seam or a doc note stating the difference.

### 4. Add compile-time Send/Sync assertions

`git grep 'assert_send\|assert_sync'` → **nothing in the entire workspace.**
`SqliteObservationStore` holds a `rusqlite::Connection`, which is `Send` but **not** `Sync`.
Nothing pins that. The first person to try sharing a store across threads gets a confusing error
far from the cause. Two lines in a test module settles it permanently.

---

## What the audit found _nothing_ wrong with

Worth recording, because it is unusual and it constrains future changes:

- **Zero `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, or `unsafe` in production code.**
  All ~200 `.expect()` calls are inside `#[cfg(test)]`. `unsafe_code = "forbid"` is enforced workspace-wide.
- **Redaction is systematic, not ad hoc.** A shared `impl_redacted_debug!` macro
  (`harness.rs:836-863`) covers all 10 evidence-bearing types; `SourceRef`, `SubjectRef`,
  `ObservationDraft`, `Observation` and `AgentHarnessEventIdentity` each have a hand-written
  redacting `Debug`. Two proptests assert `debug_never_contains_payload`.
- **Every error type** implements `std::error::Error` + `Debug` + `Display` via `thiserror`, with
  no evidence values in any message. C-GOOD-ERR is fully satisfied.
- **`#[must_use]`** on essentially every accessor (~140 occurrences) — which is exactly why the one
  missing instance was worth fixing.
- **`# Errors` documented** on every fallible public item. `# Panics` correctly absent (no panics).
- **Zero `Into`/`TryInto` impls.** Conversions use `From` correctly (`harness.rs:485`).
- **Redaction safety is tested, not assumed** — `adapter_errors_do_not_expose_source_values`,
  `invalid_references_are_reported_without_values`, `harness_debug_redacts_source_values`.

---

## Environment notes for whoever picks this up

The shell is **Nushell**, which bit me repeatedly:

- No `&&` — use `;` or `and`.
- No `2>&1` — use `o+e>` (or `complete | get stderr`).
- No `||` — use `try` / `or`.
- No backslash line continuation — put a command on one line.
- `ls -A`, `let-env` are gone.
- `cargo clean -p evidra-store` removed 8521 files / 620 MiB — expect a slow first rebuild.

Gates (from `AGENTS.md`, MSRV 1.98, use nextest not `cargo test`):

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
```

## Suggested next step

**Audit `evidra-engine`.** It landed as `f80a00e` (1191 lines) after this audit's snapshot and has
never been checked against the checklist. It is now the largest unaudited surface in the workspace.

Two consequences for the findings above:

- The lint measurements in §1 were taken with `-p evidra-core -p evidra-store -p evidra-adapters`
  only. **`evidra-engine` and `evidra-cli` were excluded.** Re-measure across the full workspace
  before turning on `missing_docs` / `unwrap_used`, or the gate may fail on code nobody has checked.
- The P4 todo on unimplemented `DerivationStore` / `RelationshipStore` (`c7b09bf6`) may now be
  resolved — the engine is presumably their intended first implementation. Worth confirming.

Also still open from this session: the `evidra-engine` public surface (`Band`, `BandTable`,
`FacetKind`, `Evidence<'a>`, `Registry`, `RedactionBreach`, `EngineError`, `MAX_BANDS`) has had no
C-DEBUG / C-ERRORS / C-EXAMPLE pass at all.

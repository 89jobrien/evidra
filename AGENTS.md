# Evidra Agent Guide

## Project

Evidra is a local-first operational intelligence system that turns observations into governed,
measurable controls. The implementation is an append-only observation ledger with typed, idempotent
persistence for redacted agent-harness evidence, plus an append-only layer of derived records
(banded facets, typed relationships, and explicit uncertainty) above it.

## Architecture

Dependencies point inward:

```text
evidra-cli     -> evidra-store    -> evidra-core
               -> evidra-adapters -> evidra-core
evidra-engine  -> evidra-core
```

- `evidra-core`: domain types and ports; no SQLite, CLI, Git, HTTP, or LLM dependencies.
- `evidra-engine`: deterministic banding (ADR-009) and redaction inheritance (ADR-010). Performs
  no I/O and does not depend on `evidra-store`. Not yet consumed by `evidra-cli`; it is a leaf
  consumer of `evidra-core`.
- `evidra-store`: persistence adapters implementing core ports.
- `evidra-adapters`: bounded agent-harness JSONL normalization into validated
  `AgentHarnessEvent` values plus secure repository-local inbox lifecycle; it does not persist
  observations. Git, Cargo, CI, and manual input adapters remain future work.
- `evidra-cli`: composition root, command parsing, and rendering; no business rules.

Longer-form documentation lives alongside this guide: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
for the crate layout and the constraints behind it, [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md)
for the invariants each rule below protects, [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for running
the CLI, and [`docs/SCHEMA.md`](docs/SCHEMA.md) for storage internals.

## Commands

The workspace MSRV is Rust 1.98. **Use `taskit` for workspace-level commands** — fmt, lint, compile,
and test all route through it so local runs and CI stay on one definition of each gate. Raw `cargo`
is for crate-scoped work (a single `--package`, a doc build, running the binary).

```bash
taskit check ci         # full gate: fmt, clippy, compile-tests, tests, unused deps
taskit check quick      # fast local loop: fmt-check + lint + tests, affected crates only
taskit check fmt        # cargo fmt --all -- --check
taskit check lint       # cargo clippy --all-targets --workspace -- -D warnings
taskit check compile    # compile every test binary without running it
taskit check deps       # cargo-machete, unused dependencies
taskit check pre-commit # pre-commit checks

taskit test run --affected         # tests for crates touched by the working tree
taskit test run --crate-name NAME # one crate

cargo run -p evidra-cli -- --help  # not a gate; taskit has no equivalent
```

`taskit check ci` runs more than the four gates below — it also compiles all test binaries, checks
for unused dependencies, and verifies build-cache integrity. Prefer it over assembling the cargo
commands by hand.

For reference, `taskit check ci` expands to:

```bash
cargo fmt --all -- --check
cargo clippy --locked --quiet --all-targets --workspace -- -D warnings
cargo nextest run --locked --workspace --all-targets
```

Two deliberate differences from the raw commands worth knowing:

- taskit passes `--locked` and `--all-targets` to the test run; a bare `cargo nextest run --workspace`
  does not, so it can silently skip test targets.
- taskit's clippy does **not** pass `--all-features`. No crate currently declares a `[features]`
  table, so this is equivalent today — but if features are ever added, `taskit check lint` will not
  cover them and CI must keep its own `--all-features` invocation until taskit is configured.

`taskit` needs no config file in this repo: it discovers the workspace from `Cargo.toml`. Running
`taskit init` would additionally generate `taskit.toml` and a `Cruxfile`, which is what enables
`protocol-drift` surfaces — it currently reports `no surfaces configured, skipping`.

Fuzzing is deliberately **outside** the workspace and outside every gate. `fuzz/` declares its own
empty `[workspace]` table, so `cargo nextest run --workspace` and `taskit check ci` cannot collect it;
it also needs nightly and unbounded runtime. The corpus is committed on purpose — a seed that lives
only in one working tree cannot reproduce a finding for anyone else.

```bash
cargo +nightly fuzz run agent_harness_jsonl     # bounded JSONL decoder
cargo +nightly fuzz run claude_code_minimize   # Stage 1 transcript allowlist
cargo +nightly fuzz run <target> -- -runs=50000 # bounded smoke run
```

Use `cargo nextest run` rather than `cargo test` if you invoke a runner directly.

## Rules

- Observations are append-only. Never update or replace an existing observation.
- Persist `AgentHarnessEvent` observations only through `append_harness_observation`; generic
  append must reject them because the observation and identity receipt must commit atomically.
- Treat equal harness identities with unequal semantic digests as conflicts, never duplicates.
- Inbox ingestion policy belongs in `evidra-core`; the CLI only composes and renders it.
- Claim ready files with no-follow/no-clobber operations under the exclusive ingest lock.
- Process recovered claims before ready files, and never overwrite quarantine evidence.
- On operational failure, retain the active processing claim and emit no partial summary.
- Inbox producers are trusted same-user processes; never grant untrusted writers access.
- Preserve original evidence and provenance; derived reasoning must link back to observations.
- Represent uncertainty, contradiction, scope, and freshness explicitly.
- Keep policy evaluation and enforcement deterministic.
- Never read the clock inside a constructor for a persisted record. `DerivationDraft` and
  `Relationship` take `recorded_at` from the caller so that identical inputs produce an identical
  record; `evidra-engine` reads no clock at all and emits timestamp-free `FacetProjection` values
  for the caller to stamp. `Observation::record` is the one deliberate exception — it stamps
  identity and `observed_at` from the clock — so do not assume the two layers share this property.
- Redacted `Debug` is mandatory on any type that holds evidence, identity, or provenance. Use
  `impl_redacted_debug!` in `harness.rs` for whole families, or write a `Debug` that emits only the
  type name via `finish_non_exhaustive()`. Never `#[derive(Debug)]` on such a type.
- Every persisted type must re-run its construction invariants in `Deserialize`. Deriving
  `Deserialize` on a type with private fields lets a stored document bypass validation entirely,
  and a record read back from disk is exactly as untrusted as one arriving from a producer. Follow
  the `Raw` shadow struct + `deny_unknown_fields` + `validate()` pattern in `derivation.rs`.
- AI may propose or summarize but may not silently grant authority.
- Keep external systems behind ports defined in `evidra-core`.
- Do not add new crates without a distinct responsibility that cannot fit an existing boundary.
- Avoid `unwrap`, `expect`, `panic`, `todo`, and `unimplemented` in production code.
- Add tests before implementing behavior.

## Persistence

SQLite is the initial local event store. Schema v3 holds six append-only tables: `observations`,
`agent_harness_receipts`, `derivations`, `derivation_facets`, `derivation_evidence`, and
`relationships`. Every one is guarded by SQLite triggers that reject `UPDATE` and `DELETE`, and
`evidra init` probes that those triggers are present and effective rather than merely defined.

Persisted observation JSON is a compatibility surface; schema or serialized-type changes require
an explicit migration. `evidra init` is the only migration entry point, and migration must not
rewrite existing observation documents. `evidra open` refuses to read a legacy database and
directs the caller to back it up and migrate.

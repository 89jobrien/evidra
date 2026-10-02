# Evidra Agent Guide

## Project

Evidra is a local-first operational intelligence system that turns observations into governed,
measurable controls. The current implementation is an append-only observation ledger with typed,
idempotent persistence for redacted agent-harness evidence.

## Architecture

Dependencies point inward:

```text
evidra-cli -> evidra-store -> evidra-core
          \-> evidra-core
evidra-engine      (reserved boundary)
evidra-adapters -> evidra-core
```

- `evidra-core`: domain types and ports; no SQLite, CLI, Git, HTTP, or LLM dependencies.
- `evidra-engine`: reserved for future correlation, inference, validation, and scoring logic.
- `evidra-store`: persistence adapters implementing core ports.
- `evidra-adapters`: bounded agent-harness JSONL normalization into validated
  `AgentHarnessEvent` values plus secure repository-local inbox lifecycle; it does not persist
  observations. Git, Cargo, CI, and manual input adapters remain future work.
- `evidra-cli`: composition root, command parsing, and rendering; no business rules.

## Commands

The workspace MSRV is Rust 1.98. Install `cargo-nextest` for the preferred test runner.

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
cargo run -p evidra-cli -- --help
```

Use `cargo nextest run` instead of `cargo test` for normal test execution.

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
- AI may propose or summarize but may not silently grant authority.
- Keep external systems behind ports defined in `evidra-core`.
- Do not add new crates without a distinct responsibility that cannot fit an existing boundary.
- Avoid `unwrap`, `expect`, `panic`, `todo`, and `unimplemented` in production code.
- Add tests before implementing behavior.

## Persistence

SQLite is the initial local event store. Schema v2 adds append-only harness identity receipts.
Persisted observation JSON is a compatibility surface; schema or serialized-type changes require
an explicit migration. `evidra init` is the only migration entry point, and migration must not
rewrite existing observation documents.

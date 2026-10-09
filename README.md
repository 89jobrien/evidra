# Evidra

**Evidence into controls.**

Evidra is a local-first operational intelligence system. It preserves operational evidence and
turns recurring interventions, failed automation, stale assumptions, and incidents into explicit,
reviewable system knowledge.

The current foundation provides an append-only SQLite observation ledger plus validated,
idempotent persistence for redacted agent-harness evidence.

## Status

Evidra is at the start of version `0.1.0`. The implemented scope is intentionally narrow:

- Initialize repository-local storage.
- Record manual intervention observations.
- List observations as a table or JSON.
- Preserve source, subject, provenance, timestamps, and integrity metadata.
- Normalize bounded, redaction-attested agent-harness JSONL into validated in-memory events.
- Persist typed harness observations with semantic digests and append-only identity receipts.
- Atomically claim, ingest, and quarantine repository-local harness event files.

Harness-specific hook installation, Git/Cargo/CI/filesystem importers, inference, governed
assumptions, controls, policy enforcement, and entropy analysis remain future work.

## Quickstart

```bash
cargo run -p evidra-cli -- init
cargo run -p evidra-cli -- note --summary "Removed stale generated artifacts"
cargo run -p evidra-cli -- observation list
cargo run -p evidra-cli -- observation list --json
cargo run -p evidra-cli -- ingest
cargo run -p evidra-cli -- ingest --json
```

## Changelog

The repository uses `git-cliff` with conventional commits:

```bash
git cliff --unreleased
git cliff --tag v0.1.0 --prepend CHANGELOG.md
```

Release entries are grouped by commit type and contain no decorative symbols.

Repository-local state is stored in `.evidra/evidra.db` and is ignored by Git.

The current database schema is v3. Re-run `evidra init` to migrate a v1 or v2 repository explicitly;
ordinary commands do not migrate storage. Before migration, stop Evidra writers and create a
WAL-safe SQLite backup rather than copying only the main database file. Migration is transactional,
but there is no automatic downgrade for older binaries.

Manual note text is passed as a command-line argument and may be retained in shell history. Do not
record secrets. Observation digests detect accidental or uncoordinated modification; they are not
cryptographic signatures against an attacker who can rewrite the database.

Harness redaction is attested by the producing integration. Evidra validates the attestation and
payload structure but does not yet perform semantic secret detection before persistence.

Harness producers publish one redacted JSON event by writing a temporary file under
`.evidra/inbox/` and atomically renaming it to a UTF-8 basename matching
`[A-Za-z0-9][A-Za-z0-9._-]*\.json` (6–128 bytes). `evidra ingest` claims a bounded snapshot,
processes recovered claims first, deletes recorded/duplicate files, and retains invalid or
conflicting evidence under `.evidra/quarantine/` with a fixed-code `.reason` sidecar. Text output
reports recorded/duplicate/quarantined counts; `--json` emits the same counters plus sorted reason
counts. Recovery covers process interruption, not power loss.

Inbox writers are trusted same-user processes. They are not authenticated, and source harness names
remain attributed claims. Do not grant inbox write access to untrusted producers.

## Workspace

| Crate             | Responsibility                                               |
| ----------------- | ------------------------------------------------------------ |
| `evidra-core`     | Domain types and ports                                       |
| `evidra-engine`   | Deterministic facet banding and redaction inheritance        |
| `evidra-store`    | SQLite persistence adapter                                   |
| `evidra-adapters` | Bounded JSONL normalization and secure local inbox lifecycle |
| `evidra-cli`      | CLI composition root and rendering                           |

## Documentation

| Document                                       | Contents                                                      |
| ---------------------------------------------- | ------------------------------------------------------------- |
| [`docs/PRD.md`](docs/PRD.md)                   | Problem, users, requirements, quality bar, non-goals          |
| [`docs/ROADMAP.md`](docs/ROADMAP.md)           | Slice sequence and per-slice gates                            |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Crate layout and the constraints that shape it                |
| [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md)   | Invariants every contributor must preserve                    |
| [`docs/OPERATIONS.md`](docs/OPERATIONS.md)     | Running the CLI, inbox lifecycle, quarantine                  |
| [`docs/SCHEMA.md`](docs/SCHEMA.md)             | Tables, triggers, migrations, validation                      |
| [`docs/conformance.md`](docs/conformance.md)   | Numbered contract clauses cited by every conformance suite    |
| [`docs/AUDIT.md`](docs/AUDIT.md)               | Audit findings and their dispositions                         |
| [`docs/adr/`](docs/adr/)                       | Architecture decision records                                 |
| [`docs/designs/`](docs/designs/)               | Slice and feature designs                                     |
| [`policies/`](policies/)                       | Git-tracked assumptions, invariants, controls, accepted risks |

Decisions are recorded as ADRs numbered `001`–`011`. Each states its context, the decision, and the
consequences accepted; none is edited after acceptance, and a change of mind is a new record.

## Principles

- Evidence before inference.
- Local-first by default.
- Immutable observations and revisable reasoning.
- Uncertainty is data.
- Deterministic enforcement.
- Bounded, reversible controls.

## Development

Evidra requires Rust 1.98 or newer. Install `cargo-nextest` before running the preferred test
command.

```bash
cargo install cargo-nextest --locked
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
```

## License

Licensed under either the Apache License, Version 2.0 or the MIT License at your option.

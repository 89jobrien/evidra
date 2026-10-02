# ADR-007: Use SQLite and versioned policy files

- Status: Accepted
- Date: 2026-09-18

## Context

The first deployment is local and single-operator, while policy intent needs human-readable review
and durable history.

## Decision

Use SQLite for append-only observations and evaluated local state. Keep human-authored assumptions,
invariants, controls, and accepted risks in Git-tracked files under `policies/`.

## Consequences

SQLite remains behind the `ObservationStore` port. Policy files are declarative intent; the database
will hold imports, evaluations, evidence links, and historical outcomes when those features exist.

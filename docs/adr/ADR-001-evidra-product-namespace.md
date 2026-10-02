# ADR-001: Use Evidra as the product namespace

- Status: Accepted
- Date: 2026-09-18

## Context

The product needs one recognizable name across its repository, executable, documentation, and
eventual crate family.

## Decision

Use `evidra` as the product, repository, binary, and proposed crates.io namespace. Verify live
registry availability immediately before any publication; this decision does not reserve the name.

## Consequences

Internal crates use the `evidra-*` prefix, while the user-facing executable is `evidra`.

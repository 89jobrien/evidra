# ADR-006: Begin with Rust workspace operations

- Status: Accepted
- Date: 2026-09-18

## Context

Evidra needs a narrow initial domain with deterministic inputs, rapid feedback, and low safety risk.

## Decision

Target local Rust workspaces, Git, Cargo, CI artifacts, generated files, and engineering-agent
operations before expanding into physical telemetry or distributed collaboration.

## Consequences

Initial adapters and controls optimize for repository-local workflows. Physical and `myrv`
integration remain later phases and must not distort the first domain model.

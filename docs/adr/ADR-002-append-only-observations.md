# ADR-002: Store observations append-only

- Status: Accepted
- Date: 2026-09-18

## Context

Reasoning and policy can change as evidence accumulates, but auditability requires the original
operational record to remain inspectable.

## Decision

Observations are immutable, timestamped, source-attributed records. Persistence adapters reject a
duplicate identity rather than updating or replacing an existing observation.

## Consequences

Corrections and contradictions must be represented as additional observations and relationships.
Future retention mechanisms must preserve the distinction between deletion policy and mutation.

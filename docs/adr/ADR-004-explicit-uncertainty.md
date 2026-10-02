# ADR-004: Model uncertainty explicitly

- Status: Accepted
- Date: 2026-09-18

## Context

Operational failures often arise when inferred, stale, contradicted, or out-of-scope beliefs are
presented as current facts.

## Decision

Evidence quality, freshness, contradiction, scope, and confidence dimensions are first-class state.
A derived aggregate score may rank work but cannot replace the underlying profile.

## Consequences

Future assumptions and controls must expose why they are trusted, where they apply, what weakens
them, and which events invalidate them.

# ADR-003: Separate evidence from claims

- Status: Accepted
- Date: 2026-09-18

## Context

Summaries and conclusions can become detached from the events that justified them, making later
review and contradiction handling unreliable.

## Decision

Claims and policies may reference evidence but may not overwrite, replace, or summarize away their
source observations.

## Consequences

Future claim APIs must retain evidence links and support both supporting and refuting evidence.
Derived text is not accepted as a substitute for provenance.

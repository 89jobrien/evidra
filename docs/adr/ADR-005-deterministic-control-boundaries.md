# ADR-005: Use deterministic control boundaries

- Status: Accepted
- Date: 2026-09-18

## Context

Semantic models can help discover patterns but are not reproducible authority boundaries and may
produce unsupported conclusions.

## Decision

AI may cluster, propose, summarize, and explain. Deterministic code decides whether an action is
allowed, denied, warned, quarantined, or escalated for approval.

## Consequences

Model output must remain labeled as inference and linked to evidence. AI cannot silently create,
widen, or enforce authority.

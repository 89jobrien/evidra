# ADR-008: Derived records are append-only and revised by supersession

- Status: Proposed
- Date: 2026-10-02

## Context

The derived layer must be revisable. Evidence goes stale, refuting evidence arrives, and a
derivation computed from the wrong window has to be corrected. ADR-002 requires that corrections be
represented as additional observations and relationships rather than mutations, and the observation
ledger enforces that with three SQLite triggers rejecting `UPDATE`, `DELETE`, and duplicate `INSERT`.

Revisability therefore cannot mean row mutation, and mixing revisable rows into the observation
tables is not possible without weakening that guarantee for the ledger itself.

## Decision

Derived records live in their own append-only tables — `derivations`, `derivation_facets`,
`derivation_evidence`, and `relationships` — each carrying the same three immutability triggers as
`observations`. No `UPDATE` or `DELETE` is issued anywhere in schema v3.

A correction appends a new derivation plus a `Supersedes` relationship pointing at the prior one.
The current view is resolved by walking that chain, not by reading a mutable `is_current` flag.
`Derivation::supersedes` is denormalised onto the record for query efficiency and must agree with the
corresponding relationship or the append is rejected.

## Consequences

Schema v3 contains no mutation path, so derived storage has the same tamper resistance as the ledger
and `validate_v3` can assert the property directly.

Every consumer of a derived fact needs chain resolution, and a chain that contains a cycle must be
rejected at write rather than resolved into an unbounded walk. Because cycles can only arise from
concurrent or replayed ingestion, the check belongs on the append path.

The cost is that "the current value" is a query rather than a column read, and superseded rows
remain stored indefinitely. Retention for the derived layer is not defined by this decision.

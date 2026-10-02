# ADR-011: Derived uncertainty is banded, not continuous

- Status: Proposed
- Date: 2026-10-02

## Context

ADR-004 requires quality, freshness, contradiction, scope, and confidence to be first-class state
rather than something a reader infers from a score.

For the derived layer the confidence dimension needs a concrete representation. Published results on
agent failure attribution put best-in-class step-level accuracy near 47% on synthetic benchmarks and
near 29% on hand-crafted ones, with LLM-judge baselines near 14%. A continuous float on that
distribution implies a precision the method does not have, and a single confident wrong causal claim
is worse than no memory because retrieval trusts it and compounds the error.

ADR-004's dimensions are also independent of one another. A claim can be well-evidenced and stale,
or fresh and contested. Collapsing them into one number loses the distinction that matters most.

## Decision

`ConfidenceBand` replaces a continuous score. It has four variants: `Speculative`, `Weak`,
`Moderate`, and `Strong`. `Strong` is reserved for derivations that are deterministically
reproducible from recorded evidence.

`Freshness`, `Contradiction`, and `ScopeFidelity` remain independent stored dimensions. A derived
aggregate may rank work by these fields, but it never replaces the profile on the record it ranks.

A derivation produced by an assisted method may only carry `Speculative` or `Weak`. Banding applies to
the confidence dimension only; the other three are states, not scores, and are not banded.

## Consequences

A reader cannot rank on a precise confidence and must respond to a class instead, which is honest
about what the number means. Consumers filter on `Freshness::stale` without parsing a composite.

Band ceilings are enforced in code rather than trusted: an assisted method cannot widen its own
confidence. That makes the property `assisted_is_never_confident` checkable over arbitrary inputs,
which is what keeps the invariant from depending on review discipline.

The cost is that two derivations in the same band are not ordered. Where ordering is genuinely needed,
it must come from a deterministic field such as evidence count, not from confidence.

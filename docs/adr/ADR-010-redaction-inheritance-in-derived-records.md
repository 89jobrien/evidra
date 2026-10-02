# ADR-010: Derived records inherit the strictest redaction of their evidence

- Status: Proposed
- Date: 2026-10-02

## Context

The harness source is redaction-first, and the redaction is attested per observation in a
`RedactionRecord`. But redaction removes a substring, not the shape around it.

A facet computed from a redacted tool excerpt can reconstruct what redaction removed. Classifying an
error as `auth-token-in-command` preserves the fact that a token was present even when the token
itself was stripped. Published research on redaction in agent tooling reaches the same conclusion:
pattern redaction cannot guarantee removal of arbitrary sensitive content, and a derived artifact that
summarises a redacted excerpt inherits the exposure.

The observation ledger already carries this risk for `description` and `title` fields. The derived
layer multiplies it, because a facet is derived from many observations and is then aggregated and
exported.

## Decision

A derived record inherits the strictest `RedactionRecord` across its evidence set. It may not weaken
any redaction its evidence carries.

`evidra-engine` must not emit a facet value that is a substring of, or trivially derivable from, any
source excerpt in that evidence set. Facet namespaces are chosen so that classification describes
category, not content: `friction.category` is admissible where `friction.observed_value` would not
be.

## Consequences

The property is mechanically checkable and becomes a property test:
`no_facet_value_appears_in_source_excerpt` holds for arbitrary evidence sets.

Derivation becomes more expensive, because each candidate facet value must be checked against every
excerpt it was derived from. That cost is paid once per derivation rather than per query, which is
the cheaper place to pay it.

A facet namespace added later must be reviewed against this rule before it is registered in the band
table. Registration is the choke point, so the check lives there rather than in review discipline.

Records that cannot be redacted without losing their analytic value must be dropped rather than
derived. That is a real loss of coverage and is accepted deliberately.

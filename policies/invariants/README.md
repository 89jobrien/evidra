# Invariants

Properties Evidra must never violate. Unlike assumptions, these are not beliefs that could be
retired — they are guarantees the architecture exists to provide, and violating one is a defect.

Format: one file per invariant, named for the invariant in kebab-case.

---

## I-001: The observation ledger is append-only

- **Statement**: No observation is ever updated or deleted.
- **Enforced by**: Three SQLite triggers in `evidra-store` rejecting `UPDATE`, `DELETE`, and duplicate
  `INSERT` (ADR-002).
- **Verified by**: A trigger test asserting each of the three statements aborts on a populated table.
- **If violated**: The ledger can no longer be trusted as evidence, and every derived record that
  cites it inherits the doubt.

## I-002: Derived records are append-only

- **Statement**: No derived record is ever updated or deleted. A revision appends a new record plus a
  `Supersedes` relationship.
- **Enforced by**: The same three triggers, on the derived tables (ADR-008).
- **Verified by**: A conformance clause asserting `UPDATE` raises for every stored derived row.
- **If violated**: "Current" becomes ambiguous, because nothing would distinguish a live value from a
  superseded one.

## I-003: No claim loses its evidence

- **Statement**: A derived record retains both supporting and refuting evidence links, and a refuting
  link is never dropped to make a record look better.
- **Enforced by**: `derivation_evidence` carries a role, and refuting links have no deletion path
  because nothing is ever deleted (ADR-003).
- **Verified by**: A conformance clause asserting a record with refuting evidence is still readable
  after its contradiction is recorded.
- **If violated**: Every derived record becomes an argument rather than a finding.

## I-004: Inference never holds authority

- **Statement**: Only deterministic code decides whether an action is allowed, denied, warned,
  quarantined, or escalated (ADR-005).
- **Enforced by**: `DerivationMethod::may_dispose` returns `true` only for `Deterministic`, and
  `Derivation::new` refuses an assisted method holding a band above `Weak`.
- **Verified by**: A property test asserting `assisted_band_cap_holds_for_every_band`.
- **If violated**: A model's opinion becomes an enforcement decision.

## I-005: Derived records cannot weaken redaction

- **Statement**: A derived record inherits the strictest redaction of its evidence, and no facet
  value may reconstruct content that redaction removed (ADR-010).
- **Enforced by**: Namespace registration is the choke point; a namespace that would expose content
  rather than category is not registered.
- **Verified by**: A property test asserting no facet value appears in any source excerpt of its
  evidence set.
- **If violated**: Summarising a redacted transcript re-leaks it.

## I-006: Absence is not evidence

- **Statement**: A missing event is treated as absence of evidence, never as proof an action did not
  occur.
- **Enforced by**: Facet queries report counts over what was recorded. Nothing infers a negative from
  an absence, and no disposition is derived from a count of zero.
- **Verified by**: Review of every disposition path once control evaluation exists.
- **If violated**: Silence becomes accusation.

## I-007: Error messages never carry source content

- **Statement**: No error display string includes an observation payload, a redacted excerpt, a facet
  value from a source document, or a locator string.
- **Enforced by**: Errors carry Evidra-authored vocabulary only. `Debug` for domain types is redacted
  or non-exhaustive.
- **Verified by**: Existing tests asserting `Debug` omits payload and provenance.
- **If violated**: Writing an error to a log leaks what the redaction pass removed.

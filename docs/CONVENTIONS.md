# Conventions

The invariants every contributor must preserve, and why each exists. `AGENTS.md` states the rules
tersely for agents; this document explains them. Most conventions below are enforced _somewhere_ — in
a macro, a trigger, a lint, or a test — because a convention enforced only by review is a convention
that will be broken.

Three are not, and they are marked **No lint** in the
[enforcement summary](#enforcement-summary). An audit against this document
([`AUDIT.md`](AUDIT.md)) found two of them violated in the workspace, which is the argument for
treating that row group as unfinished work rather than as a settled convention.

## 1. Redacted `Debug` on anything evidence-bearing

**The rule.** Any type holding evidence, identity, or provenance implements `Debug` by hand and
emits only its type name. Never `#[derive(Debug)]` on such a type.

**Why.** `Debug` output reaches logs, panic messages, and test failure output — places that are
routinely captured and shared. A derived `Debug` on `Observation` would print the entire evidence
payload. This is a confidentiality boundary, not a style preference.

**How.** For a family of related types, the shared macro in `crates/evidra-core/src/macros.rs`,
declared before the domain modules so one audited definition covers the whole crate:

```rust
macro_rules! impl_redacted_debug {
    ($($type:ty),+ $(,)?) => { $(
        impl fmt::Debug for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_struct(stringify!($type)).finish_non_exhaustive()
            }
        }
    )+ }
}
```

For a standalone generic type, write the impl by hand and keep it **unconstrained on the type
parameter**, so the type stays debuggable over readers that are not themselves `Debug`:

```rust
impl<R> std::fmt::Debug for AgentHarnessJsonlSource<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentHarnessJsonlSource")
            .finish_non_exhaustive()
    }
}
```

`finish_non_exhaustive()` is the load-bearing part: it emits the type name and `..` and nothing
else, so adding a field later cannot silently start leaking it.

**Enforced by.** Two property tests, `debug_never_contains_payload` and `debug_never_contains_source`,
plus per-type tests such as `observation_debug_omits_payload_and_provenance`,
`source_and_subject_debug_redacts_values`, `harness_debug_redacts_source_values`, and
`provenance_debug_redacts_collector_and_transformations`.

**Known gaps.** Two types derive `Debug` while holding evidence, and redact only because their inner
fields happen to: `ClaimedHarnessContent` and `evidra_engine::Evidence<'a>`. Neither leaks today;
both break the moment a third field is added, because nothing about them declares the requirement.
See A-10 in [`AUDIT.md`](AUDIT.md).

**A caution this convention has already earned.** A redacting parent hides a leaking child. Both
`Observation` and `ObservationDraft` redact every field, so `Provenance` derived `Debug` for the
lifetime of the workspace with no visible symptom: every test that touched provenance went through
an envelope that printed nothing. A per-type assertion is the only thing that catches this, which is
why the list above is per-type rather than per-envelope.

## 2. Persisted types re-run their invariants on read

**The rule.** Every type that is deserialized from storage implements `Deserialize` manually and
re-runs its construction invariants. The pattern is a `Raw` shadow struct plus
`deny_unknown_fields` plus a call to the real constructor:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFacetProjection {
    namespace: String,
    name: String,
    value: FacetValue,
}

impl<'de> Deserialize<'de> for FacetProjection {
    /// Deserializes a projection while re-checking that both address parts are present.
    ///
    /// Deriving this would let a stored record carry a blank namespace or name, because serde
    /// writes private fields directly and never calls [`FacetProjection::new`].
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawFacetProjection::deserialize(deserializer)?;
        Self::new(raw.namespace, raw.name, raw.value).map_err(serde::de::Error::custom)
    }
}
```

**Why.** `#[derive(Deserialize)]` writes private fields directly and **never calls the
constructor**. Every validation a type performs at construction — non-blank strings, slot/value
agreement, non-self relationships, band caps — is therefore bypassed on the read path. Since storage
is append-only and a modified row is indistinguishable from a forged one, a record read back from
disk is exactly as untrusted as one arriving from a producer.

`deny_unknown_fields` is part of the same rule: without it, a stored document carrying an
unrecognized field deserializes successfully, which makes adding a field non-breaking by accident.
With it, adding a field to a persisted type _requires_ a migration.

**Enforced by.** Property tests `roundtrip_preserves_record` and `relationship_roundtrip_preserves_record`;
negative tests `deserialization_cannot_smuggle_self_relationship`,
`deserialization_cannot_smuggle_self_supersession`, `deserialization_cannot_smuggle_ungated_confidence`,
`deserialization_cannot_smuggle_blank_current_facet`; and store-level tests
`derived_unreadable_document_is_rejected`, `derived_facet_drift_is_rejected`.

**The two legitimate exemptions.** `FacetCount` derives `Deserialize`, because its constructor is
infallible and there is no invariant to re-run. `FacetValue` derives it under `#[serde(untagged)]`,
which structurally cannot honour `deny_unknown_fields`; harmless while every variant is a scalar, and
a future non-scalar variant would end the exemption. Neither exemption extends to a type with a
fallible constructor — `CurrentFacet` had one, derived `Deserialize`, and was the workspace's only
instance of this violation. See A-02 and A-15 in [`AUDIT.md`](AUDIT.md).

**One limit worth stating.** Re-running construction invariants proves the record is one the domain
_would_ have accepted. It does not prove it is the record that was written. An append-only table
means a swapped document is indistinguishable from a forged one, and no amount of read-path
validation closes that on its own — `derivations` currently has no digest column, so
`Derivation::content_hash` is computed and never verified. See A-09.

## 3. Never read the clock in a persisted-record constructor

**The rule.** `recorded_at` and identity are supplied by the caller for anything derived.

**Why.** A record that stamps its own timestamp cannot be reproduced, and reproducibility is what
makes derived output checkable — a derivation can be recomputed and compared rather than trusted.

**The one exception.** `Observation::record` mints both `ObservationId` and `observed_at` itself,
because an observation is a claim about when something happened and that time is part of the claim,
not metadata about it. **The two layers do not share this property** — do not assume a constructor
in `evidra-core/src/observation.rs` behaves like one in `derivation.rs` or `evidra-engine`.

## 4. Errors carry no evidence, no paths, no errno

**The rule.** Error types use `thiserror`, implement `std::error::Error`, and their messages are
**fixed strings**. No source chaining, no paths, no filenames, no underlying OS error text.

```rust
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum AgentHarnessIngestError {
    /// An inbox lifecycle operation failed.
    #[error("ingest failed: inbox")]
    #[diagnostic(code(evidra::agent_harness_ingest::inbox))]
    Inbox,
    // …
}
```

**Why.** The path a failed file was found at, or the errno a syscall returned, is itself evidence —
sometimes of a path an attacker controls. An error message is the most widely propagated part of a
failure, and the least controlled. Three categories is enough to act on; the specifics belong in a
log the operator already trusts.

**How.** When an error genuinely needs context, narrow the category rather than widening the
message. `quarantine_reason.sidecar too large` becomes `UnsafeEntry`; the specifics stay in the
on-disk state.

**Enforced by.** `adapter_errors_do_not_expose_source_values`, `invalid_references_are_reported_without_values`,
`harness_observation_debug_and_errors_are_redacted`, `integrity_serialization_error_has_no_source`,
`table_output_escapes_control_characters`.

## 5. Append-only, in the database and in the type system

**The rule.** Never update or replace an accepted record. Correct it by superseding it.

**How it is enforced, in three independent places:**

- **SQLite triggers** abort every `UPDATE` and `DELETE` on all six tables. See
  [`SCHEMA.md`](SCHEMA.md#append-only-enforcement).
- **Constructors** validate what would be an illegal transition — `Relationship::new` refuses a
  self-edge, `Derivation::new` refuses a self-supersession, only deterministic methods may dispose.
- **Read-path validation** cross-checks indexed columns against their documents, so drift is an
  error rather than a wrong answer.

## 6. `#[must_use]` on every accessor

Roughly 140 accessors carry it. Constructors and pure queries return values whose only effect is the
return value, so discarding one is always a mistake.

`AgentHarnessJsonlSource::new` was the sole public constructor missing it — found by audit, not by
compiler, because `#[must_use]` has no lint. Add it by hand.

## 7. No `unwrap`, `expect`, or `panic` outside tests

`unsafe_code` is `forbid` workspace-wide, and `todo`, `unimplemented`, and `dbg_macro` are `deny`.
Production code currently has **zero** `unwrap`, **one** `expect`, **zero** `panic!`, `todo!`, and
`unimplemented!`, and zero `unsafe`. The several hundred other `.expect(...)` calls are all inside
`#[cfg(test)]`.

The one is `Evidence::strictest_redaction` in `evidra-engine`, rebuilding the union of a derived
record's redaction attestations. It is on the ADR-010 path — the check that stops a derived record
weakening any redaction its evidence carries — so a failed invariant there aborts the process instead
of surfacing a diagnosable error. The fallibility belongs in the signature. See A-08 in
[`AUDIT.md`](AUDIT.md).

This is a convention _without_ a lint, which is exactly why it needs restating. `clippy::unwrap_used`
and `clippy::expect_used` both pass clean today and can be turned on — but they need
`#![cfg_attr(not(test), deny(...))]` so the test modules keep their assertions. That single missing
lint is why the `expect` above survived a clean `-D warnings` run: `AGENTS.md` forbids five
constructs and the workspace lints three of them.

## 8. Conversions and identifiers

- **`as_` / `to_` / `into_` by cost and ownership.** `as_` for a cheap borrow, `to_` for a
  fallible or allocating conversion, `into_` for consuming. `DerivationId::as_str` and
  `RelationshipId::as_str` both return an owned `String` despite the `as_` prefix, which is an open
  audit finding.
- **`From` / `TryFrom`, never `Into` directly.** There are zero direct `Into`/`TryInto` impls.
- **Identifiers are newtypes.** `ObservationId`, `DerivationId`, `RelationshipId`,
  `HarnessSessionId`, `SourceEventId` are distinct types over `Ulid`. That is what makes
  `AgentHarnessEventIdentity::new(harness, session_id, source_event_id)` a meaningful call rather
  than three interchangeable strings — though the three same-typed positionals are themselves an
  open audit finding, because two can be swapped without a compile error.
- **No `get_` prefix on field access.** Use the field name. `get_` remains acceptable for a fallible
  keyed fetch (`get_todo(id) -> Result<Todo>`), which is a different operation from an accessor.

## 9. Document the contract, including what is refused

Every public item carries rustdoc. `# Errors` appears on every fallible one; `# Panics` correctly
appears nowhere, because nothing panics. Non-obvious refusals are documented at the point they
happen — `Self::new` refusing a self-relationship, `Registry` refusing a content namespace,
`FacetProjection` refusing a value that disagrees with its slot.

A refusal that is not documented reads as a bug when a caller hits it.

## Enforcement summary

| Convention               | Enforced by                                                          |
| ------------------------ | -------------------------------------------------------------------- |
| Redacted `Debug`         | `impl_redacted_debug!` + property tests                              |
| Read-path revalidation   | Manual `Deserialize` + round-trip and smuggling tests                |
| No clock in constructors | Design; `identical_inputs_yield_identical_digest`                    |
| Error opacity            | Category-only enums + leak tests                                     |
| Append-only              | SQLite triggers + constructors + read validation                     |
| `#[must_use]`            | **No lint** — by hand                                                |
| No `unwrap`/`expect`     | **No lint** — by hand (`clippy::unwrap_used` is clean and available) |
| Conversion naming        | **No lint** — by hand                                                |
| Public API documented    | **No lint** — `missing_docs` is commented out in `Cargo.toml`        |

The bottom row of "no lint" entries is where this codebase is most exposed: four load-bearing
conventions rest on discipline alone, and an audit against this document found two of them violated —
a redacting-parent hiding a leaking `Provenance`, and a production `expect` that no lint could see
(AUDIT A-01, A-08). Turning the clean Clippy lints on converts two of them into compiler-enforced
rules at no current cost. Enabling `missing_docs` converts the fourth; it is already claimed by
`PRD.md` and currently not set.

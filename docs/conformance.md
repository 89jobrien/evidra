# Evidra conformance contract index

This is the numbered index every conformance test cites. Each clause names the workspace rule it
protects; a suite asserts the clause, not the implementation, so a second implementation of the same
port cannot quietly diverge from the first.

Suites live at `crates/<crate>/tests/conformance_<surface>.rs` and are plain `#[test]` functions. There
is no conformance crate and no registration list — `cargo nextest run --workspace` already collects
`crates/*/tests/`, so a forgotten suite is impossible only if the file exists at all, and the
`#[test]` attribute is otherwise the registration.

Three conventions carry the whole strategy:

1. **One file per contract surface; the filename is the index.** A new port gets a new file.
2. **One shared assertion body, one thin `#[test]` per implementation.** The body is written once and
   never duplicated. This is the property that distinguishes conformance from an ordinary unit test:
   it is a _breadth_ guarantee, not a depth one.
3. **Every assertion message names its clause number.** A failure names the contract it broke, not
   just the line.

Each port suite also ships a **reference implementation** written against the clauses alone. It is not
the subject of the suite — it is the control: if the reference implementation fails a clause, the
clause is wrong, not the production implementation.

## Sections

| §    | File                                                           | Contract surface                                                                      |
| ---- | -------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| 1–11 | _unwritten_ (see below)                                        | Derived-domain, derived-store, migration, engine, layering, and policy-file contracts |
| 12   | `crates/evidra-store/tests/conformance_observation_store.rs`   | `ObservationStore` port substitutability                                              |
| 13   | `crates/evidra-adapters/tests/conformance_redaction_policy.rs` | `RedactionPolicy` port substitutability                                               |

§1–§11 are specified in
[`designs/2026-10-02-derived-facet-and-relationship-design.md`](designs/2026-10-02-derived-facet-and-relationship-design.md#conformance-strategy)
and remain unwritten. They are listed so the numbering stays stable: §12 and §13 were added above the
planned range rather than shifting the eleven sections the design already cites.

---

## §12 — `ObservationStore`

Protects the append-only ledger rule in `AGENTS.md`: _"Observations are append-only. Never update or
replace an existing observation."_ and _"Persist `AgentHarnessEvent` observations only through
`append_harness_observation`; generic append must reject them because the observation and identity
receipt must commit atomically."_

Implementations under this clause: `SqliteObservationStore` (production) and `MemoryStore` (reference).

| Clause | Statement                                                                                                                                                                                                                                                                             |
| ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| §12.1  | Appending the same observation twice never changes what is stored. After the second `append`, `list` returns the identical record set — same identities, same payloads, same integrity digests. A store may reject the second append or report success; it may not replace or mutate. |
| §12.2  | Every appended observation is retrievable. `list` returns each one with an equal `id`, `kind`, `subject`, `payload`, and `integrity().digest()`, and with `verify_integrity()` returning `true`.                                                                                      |
| §12.3  | `list` is bounded and newest-first. `list(0)` is empty, `list(1)` returns at most one record, and results are ordered by non-increasing `observed_at`.                                                                                                                                |
| §12.4  | `append` rejects harness observations. Passing an `ObservationKind::AgentHarnessEvent` to `append` returns an error and writes nothing, because the identity receipt must commit atomically with the observation.                                                                     |
| §12.5  | An identical harness append is a duplicate, not a second record. Appending the same `AgentHarnessObservation` twice returns `Recorded` then `Duplicate`, and `list` grows by exactly one record.                                                                                      |
| §12.6  | A reused identity with different content is a conflict, not a duplicate. The second append returns `IdentityConflict`, and the stored observation set is unchanged — which is what proves no observation was committed without its receipt.                                           |

## §13 — `RedactionPolicy`

Protects the producer rule in `AGENTS.md`: _"AI may propose or summarize but may not silently grant
authority"_, together with the evidence rules that make a redaction attestation worth anything.

Implementations under this clause: `ObfsckPolicy` (production) and `StubPolicy` (reference).

| Clause | Statement                                                                                                                                                                                                                                                                  |
| ------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| §13.1  | A retained reduction preserves the requested `kind`. The `kind` argument passed to `redact` is the `Reduction::kind()` returned; a policy may not relabel the candidate category.                                                                                          |
| §13.2  | Every emitted transformation name is `family:action` and appears at most once in one log. First-occurrence order is guaranteed structurally by `Reduction::new`, so the suite asserts the two properties it can observe rather than restating the constructor.             |
| §13.3  | An attestation does not lie. If a reduction reports at least one transformation, its `text()` differs from the input. A policy may not claim to have transformed a candidate and return it verbatim.                                                                       |
| §13.4  | Identical input yields an identical attestation. Redacting the same `(kind, text)` twice on one policy produces equal `text()` and equal `transformations()`. This is what makes republication a duplicate rather than a spurious identity conflict.                       |
| §13.5  | Candidates do not share state. Redacting a sensitive candidate must not change the reduction of a later benign one, nor the reverse. A policy that accumulates detections across candidates makes every attestation after the first unattributable.                        |
| §13.6  | A candidate naming a credential location is never retained uncleared. `redact` must not return `Some(reduction)` whose `text()` contains the sensitive path. The upstream obfuscator returns such paths verbatim, so the adapter refuses rather than inherits the default. |
| §13.7  | Errors are evidence-free. Any `Err` from `redact` must render, through both `Display` and `Debug`, without containing the candidate text. A caller cannot otherwise log a failure without leaking the evidence it failed to redact.                                        |

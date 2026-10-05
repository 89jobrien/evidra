# Design: Claude Code Session Producer

- Status: Proposed
- Proposed: 2026-10-04
- Approved:
- Implemented:

## Goal

Provide a trusted same-user producer that translates Claude Code session transcripts for the
current repository into redacted `evidra.agent-harness-event/v1` records and publishes them to the
repository inbox, so `evidra ingest` has something to consume from a real harness.

This design implements the producer explicitly deferred by
`2026-09-19-agent-harness-inbox-ingestion-design.md` ("Out of Scope: Harness event producers or
hook/plugin installation").

## Selected Approach

Use a separate producer binary that performs **structural minimization before content redaction**,
pre-screens candidates for credential-path literals, runs what survives through `obfsck` as a second
gate, records every withheld field as a named transformation in the redaction attestation, and
publishes one atomically renamed ready file per event. The producer never opens the inbox for
reading, never persists observations, and never emits a record whose redaction status is uncertain
or whose attestation would overstate the redaction actually performed.

This design extends:

- `2026-09-19-agent-harness-evidence-contract-design.md`
- `2026-09-19-agent-harness-inbox-ingestion-design.md`

## Contents

- [Context Map](#context-map)
- [What Is Actually On Disk](#what-is-actually-on-disk)
- [Attribution Is Weaker Than It Looks](#attribution-is-weaker-than-it-looks)
- [Redaction Policy](#redaction-policy)
- [Identity and Idempotency](#identity-and-idempotency)
- [Core API](#core-api)
- [Producer Adapter](#producer-adapter)
- [Wire Mapping](#wire-mapping)
- [CLI Surface](#cli-surface)
- [Failure Semantics](#failure-semantics)
- [Out of Scope](#out-of-scope)
- [Risk Checklist](#risk-checklist)

## Context Map

### Files to Modify

| File                                        | Purpose               | Changes Needed                                          |
| ------------------------------------------- | --------------------- | ------------------------------------------------------- |
| `crates/evidra-core/src/producer.rs`        | New core port         | `RedactionPolicy`, `Reduction`/reduction-log types      |
| `crates/evidra-core/src/lib.rs`             | Public API            | Export producer policy types                            |
| `crates/evidra-adapters/src/claude_code.rs` | New adapter           | Transcript reader, field minimization, obfsck redaction |
| `crates/evidra-adapters/src/lib.rs`         | Adapter exports       | Export source and reduction adapter                     |
| `crates/evidra-adapters/Cargo.toml`         | Adapter dependencies  | Add `obfsck`, `tempfile`                                |
| `crates/evidra-producer/src/main.rs`        | New binary            | Composition root, cursor, atomic publication            |
| `crates/evidra-producer/Cargo.toml`         | New crate             | Depend on adapters and core                             |
| `Cargo.toml`                                | Workspace             | Add `obfsck` and producer path entries                  |
| `.github/workflows/ci.yml`                  | Platform verification | Run producer tests on macOS and Linux                   |
| `README.md`                                 | User workflow         | Document the producer command and its guarantees        |
| `AGENTS.md`                                 | Agent guidance        | Record the minimization-before-redaction invariant      |
| `docs/OPERATIONS.md`                        | Operations            | Document producer invocation and re-run behaviour       |

### Dependencies

```text
evidra-producer -> evidra-adapters -> evidra-core
                              \-> obfsck
```

`evidra-store` is not consumed by the producer. The producer writes files; `evidra ingest` reads
them. This keeps the producer out of the observation path entirely.

`obfsck` is consumed as a **library** (`obfsck::Obfuscator`, `obfsck::ObfuscationLevel`), not as a
subprocess. It **is published** — `0.1.0` on crates.io (2026-04-12, MIT, `has_lib: true`) — and
`0.1.0` already exports the three items this design needs.

The local tree is `0.2.0`, i.e. **ahead of the published version**, so there is a real version gap to
decide rather than an "unpublished crate" problem:

- `obfsck = "0.1"` takes the published release and loses local changes.
- A path dependency takes `0.2.0` and couples two local repositories.

Either way, **the behaviour this design relies on must be re-confirmed against the version actually
pinned**. The published `0.1.0` exposes `path-policy-home-user-redact` and
`path-policy-non-allowlisted-redact` features, whereas local `0.2.0` hardcodes `is_sensitive_path`,
so the two have already diverged. The `is_sensitive_path` passthrough, the `with_pii` /
`with_allowlist` gates, and the `ObfuscationMapExport` shape are all version-sensitive and all
load-bearing.

### Required Coverage

- Redaction policy: every allowlisted field's keep/drop decision, and that dropped fields appear in
  the transformation log.
- Reduction: that no un-redacted candidate text can reach serialization by any path; that every
  record type and content type observed in a real session has an explicit disposition.
- Sensitive-path pre-screen: that a candidate naming a credential location is dropped whole, that
  `drop:sensitive-path-literal` appears in the log, and that the producer's duplicated predicate
  still agrees with `obfsck`'s `is_sensitive_path`.
- obfsck integration: clean input passes with no transformation entries; detected input yields
  obfuscated text plus correctly named category entries; the producer never constructs an obfuscator
  with an allowlist or with PII disabled; engine error fails closed; a fresh obfuscator per candidate
  so none inherits another's detections.
- Identity: same transcript published twice yields a duplicate, never a second record.
- Publication: temporary file is not claimable; ready name matches the inbox grammar; a partial
  write never becomes visible.
- Attribution: a session directory that does not correspond to this repository produces no output.

### Risk

- **Irreversibility.** A published excerpt cannot be recalled. Every design decision below errs
  toward emitting less evidence rather than more.
- **Transcript schema drift.** Claude Code's record set is not contractually fixed by this project
  and new record types should be expected. (This document does not assert _when_ any particular type
  was introduced — no changelog was consulted — only that the reader must not assume a closed set.)
- **Attribution collision.** The project-directory encoding is lossy (below).
- **Redaction engine upgrade.** A new pattern set changes output for unchanged input, which interacts
  badly with identity (below).

## Crate Ownership

- **`evidra-core`** owns the `RedactionPolicy` port and the reduction vocabulary. It knows nothing
  about Claude Code or `obfsck`.
- **`evidra-adapters`** owns the Claude Code transcript reader, the field allowlist, and the
  `obfsck`-backed policy implementation.
- **`evidra-producer`** composes them, tracks the publication cursor, and publishes atomically. It
  holds no business rules.
- **`evidra-cli`** is unchanged. It still only ingests.

## What Is Actually On Disk

Verified by inspection, not assumption:

```text
~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl
```

A real transcript record:

```json
{ "type": "last-prompt", "leafUuid": "0741f841-…", "sessionId": "0b1bdf06-…" }
```

An assistant record with tool use:

```json
{
  "parentUuid": "…",
  "isSidechain": false,
  "message": {
    "model": "claude-sonnet-5",
    "id": "msg_…",
    "type": "message",
    "role": "assistant",
    "content": [{ "type": "thinking", "thinking": "", "signature": "…" }]
  }
}
```

Record types observed in one 328-line session, with counts: `attachment` 111, `assistant` 89,
`user` 53, `last-prompt` 14, `mode` 13, `permission-mode` 13, `ai-title` 12, `file-history-snapshot`
9, `system` 8, `queue-operation` 6. All 328 lines parse; none are blank or untyped.

`assistant.content[]` is a `type`-discriminated union of `thinking` 17, `tool_use` 49,
`tool_result` 49, `text` 24. Item shapes:

| Item          | Keys                                                    |
| ------------- | ------------------------------------------------------- |
| `tool_use`    | `type`, `id`, `name`, `input`, `caller`                 |
| `tool_result` | `tool_use_id`, `type`, `content` (structured, not text) |
| `text`        | `type`, `text`                                          |

`file-history-snapshot` carries `messageId`, `snapshot`, `isSnapshotUpdate`; `snapshot` holds
`messageId`, `timestamp`, and `trackedFileBackups`. In the observed session, 7 of 9 records carried
28 tracked-file entries keyed by **relative repository path** (`crates/doob/Cargo.toml`), each entry
holding `backupFileName`, `version`, and `backupTime`. **It does not inline file content** — the
content lives in a separate backup file named by `backupFileName`. The disclosure surface is
therefore repository file paths, backup filenames, and timestamps. The producer must not follow
`backupFileName` to read backup content; that is a second store with its own sensitivity.

Consequences for the wire mapping:

- The transcript is **not** `evidra.agent-harness-event/v1`. Every record lacks `schema`, so the
  existing decoder rejects it as `UnsupportedSchema` at line 1. A translating producer is the only
  way in.
- `tool_use.input` carries command arguments and prompt text — the highest-risk field in the
  system.
- No record carries a `cwd`.

## Attribution Is Weaker Than It Looks

The design decision was "current repo only", implemented by deriving the encoded project directory
from the repository root. Inspection shows that is not sound on its own.

Observed encodings:

| Real path                                    | Directory name                                |
| -------------------------------------------- | --------------------------------------------- |
| `/Users/joe/dev/doob`                        | `-Users-joe-dev-doob`                         |
| `/Users/joe/.claude`                         | `-Users-joe--claude`                          |
| `/Users/joe/dev/minibox/worktrees/moa-xtask` | `-Users-joe-dev-minibox--worktrees-moa-xtask` |

Both `/` and `.` become `-`, and the third row contains a double dash that a naive
replace cannot produce. The transform is therefore **not fully determined by inspection of these
three rows alone**, and the mapping must be assumed lossy until upstream documents it.

A **collision has not been demonstrated**. The reasoning above shows the transform discards
information — `.` and `/` both collapse to `-`, so `/a.b/c` and `/a/b.c` are not distinguishable —
which makes a collision possible, and this document does **not** claim two specific colliding
directories have been observed. It is stated as a risk, not as a defect.

Compounding this: **no transcript record carries `cwd`** (verified across all 328 lines of a real
session). There is no per-record signal to fall back on. The directory name is the _only_ statement
of which repository a session belongs to.

Required posture:

1. The producer derives the candidate directory name from the repository root using the observed
   transform, and treats a **missing directory as a normal, silent no-op** — not an error. A user
   who has never run Claude Code in this repository has nothing to publish, which is not a failure.
2. Because the transform is lossy and may attribute another repository's session to this one, the
   producer **attributes conservatively and records the encoding in `source.locator`** as the
   session's directory name only. The ledger must never assert a filesystem path it did not verify.
3. Should per-record `cwd` ever appear upstream, attribution should be re-verified per record and
   this section revisited. Until then the collision risk is accepted and documented, not solved.
4. A `--from` override is deliberately **not** provided in this design; see Out of Scope.

## Redaction Policy

This is the substance of the design. Two stages, in this order.

### Stage 1 — Structural Minimization

The producer reduces a transcript record to a small set of candidate strings **before** any content
inspection. Raw transcript text is never a candidate. The allowlist is closed: a field not named
here is dropped.

| Transcript field                                       | Disposition | Rationale                                          |
| ------------------------------------------------------ | ----------- | -------------------------------------------------- |
| `message.content[].type == thinking`                   | **drop**    | Model reasoning, not operational evidence          |
| `message.content[].type == text`                       | candidate   | Prose; highest-value evidence, so it survives      |
| `message.content[].type == tool_use`, `.name`          | candidate   | Tool identity is the operationally useful part     |
| `message.content[].type == tool_use`, `.input`         | **drop**    | Carries command arguments, paths, and secrets      |
| `message.content[].type == tool_use`, `.id`, `.caller` | **drop**    | Opaque identifiers, no evidentiary value           |
| `message.content[].type == tool_result`                | **drop**    | Command **output**; the highest-risk content class |
| `user.message.content` (string)                        | candidate   | The instruction that caused the action             |
| `system` records                                       | **drop**    | Environment metadata, not evidence of action       |
| `file-history-snapshot`                                | **drop**    | File paths plus backup metadata (see below)        |
| `attachment`                                           | **drop**    | Attachment payloads are opaque and unbounded       |
| `mode`, `permission-mode`, `queue-operation`           | **drop**    | UI/lifecycle state, not harness events             |
| `ai-title`                                             | **drop**    | Generated prose, not an event                      |
| `last-prompt`                                          | **drop**    | Duplicate of the `user` record                     |
| `sessionId`, `uuid`, `timestamp`                       | identity    | Becomes `session_id` / `source_event_id` / time    |

This table is exhaustive for the types observed in a real session. It was originally missing the
`tool_result` and `tool_use.caller` rows; both are now explicit. Behaviour for an unlisted field is
still _drop_, so the omission was safe in effect, but a closed allowlist presented as exhaustive
must actually be exhaustive or it cannot be reviewed.

`tool_use.input` is the decisive choice. Tool _names_ (`Bash`, `Read`, `Edit`) describe what kind of
operation occurred and are the basis for any future control. Tool _inputs_ are the commands
themselves, which routinely contain secrets, paths outside the repository, and customer data. The
observed input shape is free-text prompt material (`description`, `prompt`, `run_in_background`),
which confirms the field holds sub-agent prompt content rather than a bounded argument list.
Dropping the input while keeping the name is what makes this useful without being unsafe.

Stage 1 also enforces a **per-excerpt byte bound** before any excerpt is constructed. This bounds the
blast radius of a Stage 2 false negative independently of the 128 KiB record limit, which is a
protocol bound rather than a disclosure control.

### Stage 2 — obfsck Content Redaction

Every Stage 1 candidate is passed through `obfsck::Obfuscator` at a configured `ObfuscationLevel`
(`Minimal`, `Standard`, or `Paranoid`). The default is **`Standard`**, chosen as the level at which
a false negative is unlikely to matter for the short, low-entropy fields this design admits.

obfsck **obfuscates rather than redacts to a placeholder**: a detected value is replaced by a stable
category token of the form `[LABEL-N]`, e.g. `[EMAIL-1]`. The candidate is retained, not discarded.

Stage 2 fails closed on **engine error**:

- **Detections** produce obfuscated text. The event is still published.
- **A clean verdict** passes the candidate through, with no transformation entry.
- **An engine error**, or any failure to obtain a verdict, aborts publication for that event. The
  producer does not emit a record whose redaction status is unknown, and it never falls back to
  passing text through unexamined.

#### API Constraints That Shape the Design

Four properties of the current `obfsck` API are load-bearing and were verified against the crate
rather than assumed.

**1. Detection categories are fixed and coarse.** `ObfuscationMapExport` exposes `ips`, `hostnames`,
`users`, `containers`, `paths`, `emails` (each a `HashMap<String, String>`) plus a single
`secrets_count: usize`. The underlying `secrets` field is a private `HashSet<String>` and is never
surfaced. **The producer therefore cannot record which secret pattern matched** — only that
N secret matches occurred. The transformation log must reflect that limit rather than invent
pattern names.

The token labels themselves are a fixed set — `IP-INTERNAL`, `IP-EXTERNAL`, `HOST`, `USER`,
`CONTAINER`, `EMAIL`. There is **no `PATH` token category**: paths are handled by a separate
`obfuscate_paths` pass that does not use the token helper. So `obfsck:path` in the transformation
log describes a _reportable_ map, not a token label, and the two must not be conflated.

**2. Three mechanisms pass content through unchanged, and all three must be understood before
relying on this stage.**

| Mechanism                 | Effect                                                            |
| ------------------------- | ----------------------------------------------------------------- |
| `with_allowlist(entries)` | Listed values are "never redacted even when they match a pattern" |
| `with_pii(false)`         | Skips structural PII (emails, IPs, users); secrets unaffected     |
| `is_sensitive_path(path)` | Paths matching a hardcoded list are returned **verbatim**         |

The first two are opt-in, and the producer uses neither; a test asserts that it does not.

The third is **not opt-in and cannot be disabled through the public API**. `is_sensitive_path` matches
any path containing `/etc/shadow`, `/etc/passwd`, `/etc/sudoers`, `/etc/ssh/`, `/.ssh/`, `/id_rsa`,
`/id_ed25519`, `/.aws/credentials`, `/.kube/config`, `/secrets/`, `/vault/`, `/.env`, or the Windows
SAM/system/security equivalents, and such paths are returned unchanged.

This is a deliberate and defensible choice for log redaction — preserving the _name_ of a sensitive
path preserves the signal, and `[ID-RSA-1]` would hide it. **It is the wrong default for an
evidence ledger**, whose purpose is to minimize what persists. A transcript excerpt mentioning
`~/.aws/credentials` would be stored verbatim while the producer's transformation log recorded
nothing, which would make the attestation actively misleading.

Required posture, in preference order:

1. **Preferred — pre-screen and drop.** Before Stage 2, scan each candidate for any
   `is_sensitive_path` match and **drop the whole candidate**, recording
   `drop:sensitive-path-literal`. A path that names a credential location is not evidence of an
   operation; it is a secret location. This restores a truthful attestation at the cost of some
   excerpts.
2. **Acceptable — record it.** Keep the candidate but emit an explicit
   `obfsck:sensitive-path-preserved` entry so the ledger shows the text was _not_ obfuscated. This
   is honest but persists the literal.
3. **Rejected — inherit the default silently.** This is what an unwritten producer would do, and it
   is what the fail-closed framing in this document originally implied. It must not ship.

Option 1 is the design. The predicate must be duplicated in the producer (the obfsck function is
`pub(super)`), which is a coupling cost accepted deliberately and pinned by a test that asserts the
two lists agree.

**3. `Obfuscator` and `ObfuscationMap` derive `Debug` and that `Debug` leaks the originals** — the
maps hold `original -> token` pairs, so a single `{:?}` writes the un-redacted values into logs.
The producer must never format either type. This is the same class of defect as the `Provenance`
leak the workspace audit found and closed (A-01 in [`../AUDIT.md`](../AUDIT.md)) — `Provenance`
printed `collector` and `transformations` verbatim, one layer down from here — and it is why the
redacted-`Debug` requirement below is stated as a hard constraint rather than a convention.

The generalisable lesson, since this document will be read as precedent for the harness producers
still to come: **a redacting parent hides a leaking child.** `Provenance` derived `Debug` for the
lifetime of the workspace with no visible symptom, because `Observation` and `ObservationDraft` both
redact every field and every test touching provenance went through one of those envelopes. The audit
could only catch it by asserting per type. A producer that redacts its own output type but routes a
redacting type's values into a logging call inherits the same blind spot, one level further out.

**4. `Obfuscator` accumulates detections across calls.** The producer therefore constructs **a
fresh obfuscator per candidate**. A single shared instance would make `export()` cumulative and a
candidate's transformation log would credit it with detections belonging to an earlier one.

### The Transformation Log Is the Audit Trail

`RedactionRecord::transformations` is a `Vec<String>`. This design uses it as an **ordered, named
reduction log** rather than a decorative label. Every withheld field is recorded, whether or not
content redaction ran:

```json
{
  "policy": "obfsck",
  "version": "0.2.0+standard",
  "transformations": [
    "drop:thinking",
    "drop:tool_result",
    "drop:tool_use.input",
    "drop:file-history-snapshot",
    "drop:sensitive-path-literal",
    "obfsck:email",
    "obfsck:path",
    "obfsck:secret"
  ]
}
```

`obfsck:` entries name only the categories actually reported by `ObfuscationMapExport`, and are
emitted in a fixed order. `obfsck:path` reflects the reported `paths` map and is **not** a token
label. `obfsck:secret` records that secret detection fired without claiming how many patterns or
which ones, because — per the API constraint above — that information is not available. A count
belongs in the producer's stdout summary, not in the attestation, since `RedactionRecord` has no
count field and inventing one would change the wire contract.

`drop:sensitive-path-literal` is the visible consequence of the preferred posture in Stage 2: any
candidate naming a credential location is dropped whole, and the ledger says so rather than
silently persisting it.

This buys three things a boolean cannot:

1. **Auditability.** A reader of the ledger can see that reduction happened, and what was withheld,
   without re-running the producer.
2. **ADR-010 correctness.** Redaction inheritance unions transformations across evidence. A derived
   record can therefore never appear _less_ redacted than its inputs, and the union is meaningful
   because the names are stable.
3. **Defect detection.** If a future transcript field is added and accidentally admitted, the
   producer's own output changes and the missing `drop:` entry is visible in the ledger.

Ordering is deterministic: `drop:*` entries in Stage 1 declaration order, then `obfsck:*` entries
in the fixed category order above.

The `version` field pins the engine **and** the level (`0.2.0+standard`), so a level change is
visible as a distinct attestation rather than a silent behavior change.

### What the Policy Explicitly Does Not Do

- **No semantic secret scanning of admitted text.** Stage 2 detects configured secret patterns. It
  does not judge whether a prose excerpt is sensitive. The mitigation is Stage 1 keeping excerpts
  short and structural, not a claim of comprehensive detection.
- **No redaction of `source.locator` or `session_id`.** These are structural identifiers, not
  evidence content, and redacting them would break idempotency.
- **No attempt to detect a Stage 2 false negative.** It is mitigated by bounds and pattern breadth,
  not eliminated.

## Identity and Idempotency

`AGENTS.md` requires that equal harness identities with unequal semantic digests are treated as
conflicts, never duplicates. Combined with a changing redaction engine, this produces a specific
hazard that the design must defuse rather than discover later.

**The hazard:** if `source_event_id` were derived only from the transcript's own event UUID, then
upgrading `obfsck` (or changing its level) changes the redacted text, which changes the semantic
digest, which turns every re-published event into an `IdentityConflict` — mass-quarantining
evidence that never actually changed.

Two mechanisms prevent it:

1. **Policy-qualified identity.** `source_event_id` is derived from the transcript event UUID **and**
   the attestation version (`obfsck 0.2.0+standard`). A redaction-policy change therefore yields a
   _new_ identity rather than a conflicting one. Different policy, different event; same policy,
   byte-identical record.
2. **A publication cursor.** The producer records the last published position per session file
   (byte offset, or record index for robustness against rewrites) in its own state file outside the
   repository. A re-run resumes rather than re-emitting. This makes the common case zero work and
   means the cursor — not digest comparison — is what normally prevents republication.

The cursor is an optimization, not the correctness mechanism: correctness comes from
policy-qualified identity, and holds even with the cursor deleted.

## Core API

```rust
/// One candidate string considered for retention, and the transformations applied to it.
pub struct Reduction {
    kind: String,
    text: String,
    transformations: Vec<String>,
}

/// Produces a redacted excerpt or withholds the candidate entirely.
pub trait RedactionPolicy {
    type Error: std::error::Error + Send + Sync + 'static;

    fn redact(&self, kind: &str, text: &str) -> Result<Option<Reduction>, Self::Error>;
}
```

`RedactionPolicy` is deliberately narrow. It receives one candidate at a time and cannot see the
rest of the record, so a policy cannot accidentally exfiltrate unrelated fields, and its behaviour
is trivially testable in isolation.

The `Drop` list is **not** a policy decision — it is a property of the transcript reader, which
knows the field grammar. Stage 1 therefore lives in the adapter, and the policy owns only Stage 2.
This split keeps the allowlist auditable in one place and testable without constructing a transcript.

Error handling: any `Err` aborts publication for that event and is reported with a fixed,
source-free category. The producer never downgrades an error into a pass-through.

## Producer Adapter

### Transcript Reader

Incremental, bounded, and tolerant of schema drift:

- Records are read line by line with a per-record byte bound and a per-file record count bound.
- An unparseable line, an unknown record `type`, or a missing expected field causes that record to be
  **skipped and counted**, never to fail the file. Claude Code's record set is open, and a producer
  that halts on an unknown type would stop working the moment a new one ships.
- Only records whose `type` is on the Stage 1 allowlist contribute. Everything else increments a
  skipped counter.
- The reader never holds more than one record in memory at a time.

### Publication

For each retained event, in session-file order:

1. Construct the complete `evidra.agent-harness-event/v1` record, including the transformation log.
2. Serialize and verify it is one line ending in a newline.
3. Write to `.evidra/inbox/<name>.tmp` with mode `0600`, in the same directory as the eventual ready
   file.
4. Close the file descriptor.
5. Atomically rename to `.evidra/inbox/<name>.json`.

The ready name is derived from the session id and a per-session sequence number so that repeated
publication of the same record targets the same name:
`<session-id-short>-<sequence>.json`. It must satisfy the inbox's published grammar — 6–128 bytes,
`.json` suffix, `[A-Za-z0-9][A-Za-z0-9._-]*` — and the producer validates that locally before
renaming rather than discovering the violation as a quarantine.

Steps 3 through 5 mirror the producer contract in the inbox design exactly. The producer is an
ordinary trusted producer with no privileged access.

### Redacted Debug

Every type in this slice that can hold transcript-derived content implements a redacting `Debug`
via `impl_redacted_debug!`. This is not optional: the audit that motivated this work found
`Provenance` printing producer identity verbatim (A-01 in [`../AUDIT.md`](../AUDIT.md)), and the same
failure mode applies with more force to raw transcript content.

Two practical constraints on that requirement, both learned from how A-01 hid:

- The macro currently lives in `evidra-core` and is not exported, so a producer in `evidra-adapters`
  cannot use it without either `#[macro_export]` or a hand-written `Debug`. Hand-written is fine and
  is what `Observation` and `ObservationDraft` already do; use `finish_non_exhaustive()`.
- Assert per type. `Provenance` was only findable because the audit compared a type against the
  convention rather than looking for a symptom, since nothing observable was wrong.

The `obfsck` types are an explicit and separate obligation. `Obfuscator` and `ObfuscationMap` derive
`Debug`, and their debug output contains `original -> token` pairs — that is, the un-redacted source
values. The producer must therefore never format either type, including indirectly through a
`#[derive(Debug)]` struct that holds one. Wrapping them is the intended mitigation; a hand-written
`Debug` that delegates would reintroduce the leak.

## Wire Mapping

| Wire field           | Source                                                            |
| -------------------- | ----------------------------------------------------------------- |
| `schema`             | Constant `evidra.agent-harness-event/v1`                          |
| `source_event_id`    | Transcript record `uuid`, policy-qualified by attestation version |
| `occurred_at`        | Record `timestamp`; absent or unparseable ⇒ record skipped        |
| `source.kind`        | Constant `claude-code-session`                                    |
| `source.locator`     | Session directory name only — never a filesystem path             |
| `subject.kind`       | `repository`                                                      |
| `subject.identifier` | Repository root path — see the attribution caveat                 |
| `harness.name`       | `claude-code`                                                     |
| `harness.version`    | `null`                                                            |
| `session_id`         | Record `sessionId`, or the transcript filename stem               |
| `event_type`         | `tool_use` / `text` / `user_prompt`                               |
| `redaction`          | Stage 1 + Stage 2 outcome, with the full transformation log       |
| `excerpts`           | Stage 2 output, bounded                                           |
| `facets`             | `tool_name` for `tool_use`; empty otherwise                       |

No wire schema change is required. `HarnessEventType::new` accepts any non-blank string, so
`tool_use` and friends need no new domain variant.

`subject.identifier` is the one remaining disclosure question. The repository root path is already
recorded by `evidra note` (`SourceRef::new("manual", "evidra note")` with the repository path as the
subject), so this introduces no new class of stored path. It is recorded for attribution, and it is
the same value the existing `note` command already persists.

## CLI Surface

```text
evidra-producer [--dry-run] [--level minimal|standard|paranoid]
```

- Default level is `standard`.
- `--dry-run` performs full translation and redaction, prints the summary and the transformation
  log totals, and publishes nothing. This is the safe way to inspect what a session would yield.

Success output is a fixed summary with no session names, paths, or excerpt content:

```text
Published 12 recorded, 0 skipped, 3 redacted
```

Failures print one fixed category line to stderr and exit nonzero:

```text
producer failed: redaction
producer failed: transcript
producer failed: publish
```

## Failure Semantics

- **Redaction engine error or unknown verdict** ⇒ `producer failed: redaction`. Nothing is published
  for that event. There is no pass-through fallback.
- **Transcript read error** ⇒ `producer failed: transcript`. The cursor is not advanced past the
  failed record, so the next run retries it.
- **Publish error** ⇒ `producer failed: publish`. A `.tmp` file may remain; it is counted and ignored
  by the inbox, exactly as the producer contract specifies.
- **Unknown record type, malformed record, missing timestamp** ⇒ skipped and counted. Never fatal.

The producer holds no lock and shares no state with `evidra ingest` beyond the inbox directory. A
concurrent `ingest` may claim a record between the producer's rename and the next run; the cursor
makes that harmless, because the record is already recorded and re-derivation yields a duplicate.

## Out of Scope

- **Hook or plugin installation.** No Claude Code `SessionEnd`/`Stop` wiring. Invocation is
  explicit. Automatic publication couples the ledger to a specific harness lifecycle and makes the
  trust boundary harder to reason about.
- **`--from` / arbitrary transcript selection.** Deferred deliberately. "Current repo only" is the
  only supported mode until the attribution collision above has a real answer. A `--from` flag would
  let a user attribute one repository's session to another with no verification at all.
- **Cross-repository or bulk publication.**
- **Continuous watching or incremental tailing.** The producer is a batch command. The cursor makes
  re-runs cheap but does not make it a daemon.
- **Semantic secret scanning beyond `obfsck` pattern detection.** This is the deferred governance
  question from the inbox design and remains deferred; a separate governance design is still
  required before it is reopened.
- **Retroactive re-derivation.** Changing the redaction level produces new identities; it does not
  rewrite or purge prior records. The ledger is append-only.
- **Authenticated producer identity.** Unchanged from the inbox design: producers are trusted
  same-user processes, not authenticated.
- **Windows.** The producer inherits the inbox's platform posture: fail closed.

## Risk Checklist

- [ ] Breaking API change: additive core port and a new crate/binary, both before `1.0.0`.
- [ ] Persisted compatibility change: none. No schema or serialized-type change; wire format is
      unchanged `v1`.
- [ ] New external dependency: `obfsck`, consumed as a library. **Published as `0.1.0`; local tree is
      `0.2.0`.** The published-vs-path decision is unresolved and must be made deliberately, and
      every load-bearing obfsck behaviour in this design must be re-verified against the pinned
      version, since the two have already diverged on path policy.
- [ ] Dependency features required: none beyond `obfsck`'s defaults.
- [ ] Circular dependencies: none. `obfsck` does not depend on Evidra.
- [ ] AI authority: none. Translation and redaction are deterministic. No model participates in
      deciding what is retained.
- [ ] **Irreversible disclosure:** accepted, mitigated by structural minimization before content
      redaction, a sensitive-path pre-screen, bounded candidates, and a transformation log that makes
      every withholding visible. Requires explicit approval before implementation.
- [ ] **`is_sensitive_path` duplication:** the producer must copy obfsck's private predicate, which
      is an unversioned coupling to another crate's internals. An upstream change could silently
      diverge. Pinned by a test asserting the two lists agree, and revisited on every obfsck bump.
- [ ] **Attribution risk:** the project-directory transform is lossy, so misattribution is possible.
      Not demonstrated to occur, and not solvable from the transcript side while no record carries
      `cwd`. Mitigated by silent no-op when absent and by never asserting an unverified filesystem
      path. Revisit if upstream records ever carry `cwd`.
- [ ] **Redaction upgrade mass-conflict:** defused by policy-qualified `source_event_id`.

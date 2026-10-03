# Operations

Running Evidra day to day: the inbox lifecycle, what to do when something is quarantined, and what
failure guarantees you can rely on. For design rationale see [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Commands

```bash
evidra init                        # create or migrate the store; the only migration entry point
evidra ingest [--json]             # claim and record pending harness events
evidra note --summary "…"          # record a manual observation
evidra observation list [--limit N] [--json]   # list, newest first (default 50, max 1000)
```

Every command resolves paths from the **nearest ancestor containing `.git`**, not the working
directory, so nested invocations operate on the same repository.

Output is stable and safe to parse. `ingest` prints one summary line plus one line per nonzero
quarantine reason:

```text
Ingested 3 recorded, 1 duplicate, 1 quarantined
Quarantine invalid-json: 1
```

`--json` emits exactly:

```json
{
  "recorded": 3,
  "duplicate": 1,
  "quarantined": 1,
  "reasons": {
    "invalid-json": 1
  }
}
```

Reason keys are sorted and zero counts never appear. On any failure **stdout stays byte-empty** and
exit status is `1`; the summary is only rendered after ingestion returns successfully, so a partial
run never reports partial success.

## Store layout

State lives in `.evidra/` at the repository root and is Git-ignored:

```text
.evidra/
  evidra.db                      SQLite store
  .gitignore                     contents exactly "*\n!.gitignore\n"
  ingest.lock                    advisory lock, mode 0600
  inbox/                         producer-published ready files (and producer .tmp files)
  processing/                    claimed evidence awaiting a decision
  quarantine/                    retained rejected evidence
```

`evidra init` creates `.evidra/`; the inbox creates the other three directories at mode `0700`. All
four must be owned by the effective UID, mode exactly `0700`, and on the same device. `.evidra`
itself must already exist — the inbox refuses to create it.

### File naming

Names are validated, not sanitized. Anything that does not match is rejected as an unsafe entry
rather than repaired.

| Directory     | Accepted name                  | Rule                                                                      |
| ------------- | ------------------------------ | ------------------------------------------------------------------------- |
| `inbox/`      | `<ready>.json`                 | 6–128 bytes, ends `.json`, first byte alphanumeric, only `[A-Za-z0-9._-]` |
| `inbox/`      | `<producer>.tmp`               | 5–128 bytes, ends `.tmp`, same charset rules                              |
| `processing/` | `<ULID>.json`                  | remainder parses as a ULID                                                |
| `processing/` | `<ULID>.reason.tmp`            | remainder parses as a ULID                                                |
| `processing/` | `<ULID>.reason.stage.<ULID>`   | **both** halves parse as ULIDs                                            |
| `quarantine/` | `<ULID>.json`, `<ULID>.reason` | remainder parses as a ULID                                                |

A single counter spans all three directories, capped at **10,000 entries**. Exceeding it fails
initialization. `.tmp` files in `inbox/` are counted but never claimed.

## Ingestion lifecycle

### 1. Lock

`evidra ingest` takes an exclusive advisory `flock` on `.evidra/ingest.lock` **before** it opens any
directory or scans anything. A second concurrent invocation fails immediately with
`ingest failed: inbox` — it never reaches the scan.

There is no explicit unlock; the lock is released when the process closes the descriptor. It is a BSD
`flock` on an open file description, so it is not portable to NFS-style locking.

### 2. Scan and repair

Initialization scans all three directories, validates every entry, and repairs three classes of
interrupted work:

- **Orphan stage files** — a `.reason.stage.*` with no claim in `processing/` or `quarantine/` is an
  unsafe entry. Otherwise the stage file is removed and the JSON reprocessed.
- **Published temp reasons** — a `.reason.tmp` whose claim has not yet moved to `quarantine/` is
  completed, preserving the _original_ reason.
- **Quarantine inconsistency** — a quarantined JSON missing its reason gets one written with reason
  `incomplete-quarantine`. A reason with no JSON, or a stage name appearing in `quarantine/`, is an
  unsafe entry. Existing sidecars are re-parsed against the known reason set, so a tampered code
  fails initialization rather than being trusted.

This repair exists because quarantine is a **multi-file, multi-step** operation. A crash between the
two renames is normal and recoverable.

### 3. Claim

Recovered claims are queued **strictly before** newly published ready files, and both sub-queues are
sorted lexically. Interrupted work resumes before new evidence is admitted.

Each claim:

1. Generates a fresh ULID claim id.
2. Pre-checks the destination for existence using `AT_SYMLINK_NOFOLLOW`, so a dangling symlink counts
   as present and forces a retry.
3. Renames the ready file into `processing/` with `RENAME_NOREPLACE` (`renameat2` on Linux,
   `renameatx_np` on macOS). Up to **16 attempts** on collision.

This ordering is a **per-invocation, no-rescan** guarantee. The scan happens once at startup;
files published afterwards are not seen until the next invocation.

Filesystem limitations surface as `inbox filesystem is unsupported` — notably a cross-device rename
(`EXDEV`), and `renameat2`/`renameatx_np` being unavailable.

### 4. Process

For each claim: read, normalize, convert to a domain record, append, complete.

The claim is **retained on failure**. If conversion or the store fails, the file stays in
`processing/` and the next run picks it up through the recovery path and retries. If the store
committed but completion failed, the retry re-appends the identical identity and digest, the store
reports `Duplicate`, and the claim is then completed — ingestion is idempotent.

### 5. Complete or quarantine

A successfully recorded claim is removed from `processing/`. A rejected claim moves to
`quarantine/`.

## Quarantine

A quarantined claim is exactly two files:

```text
quarantine/<ULID>.json      the original evidence, byte for byte, mode 0600
quarantine/<ULID>.reason    the bare reason code, no trailing newline, mode 0600
```

The reason is staged inside `processing/` first (`<ULID>.reason.stage.<ULID>` → `<ULID>.reason.tmp`)
so that an interrupted quarantine can be finished on the next run while the original evidence is
never disturbed.

**Retained evidence is never overwritten.** Three independent mechanisms guarantee it: every
quarantine transition uses `RENAME_NOREPLACE`; every reason file is created `O_CREAT|O_EXCL` rather
than opened for truncate; and recovery _adds_ a missing reason rather than rewriting an existing one.

### Reason codes

Exactly ten, stable kebab-case, part of the on-disk contract:

| Code                    | Meaning                                           |
| ----------------------- | ------------------------------------------------- |
| `invalid-json`          | A record was not valid JSON                       |
| `unsupported-schema`    | Record schema tag is not the supported version    |
| `invalid-reference`     | A reference within the record does not resolve    |
| `invalid-event`         | Content violates the harness event domain         |
| `record-too-large`      | Record exceeds 128 KiB                            |
| `blank-input-limit`     | Excessive blank input exceeded the bound          |
| `empty-event`           | File contained zero events                        |
| `multiple-events`       | File contained more than one event                |
| `identity-conflict`     | Same harness identity, different semantic digest  |
| `incomplete-quarantine` | Repair marker for a quarantine missing its reason |

### What is _not_ quarantined

- **Conversion failures.** These stop the run with `ingest failed: conversion` and retain the claim.
  They indicate a bug, not bad input.
- **I/O errors while reading a claim.** These stop the run with `ingest failed: inbox`.

Only input-content failures and store-level identity conflicts are quarantined. A quarantined file is
**never automatically reprocessed** — it is retained evidence for a human.

## Failure semantics

Three error categories, each a fixed string with no source chaining and no paths, filenames, or
underlying OS errors:

| Category                    | Output | Meaning                              |
| --------------------------- | ------ | ------------------------------------ |
| `ingest failed: inbox`      | stderr | Filesystem or lifecycle failure      |
| `ingest failed: conversion` | stderr | Domain invariant rejected the record |
| `ingest failed: store`      | stderr | Persistence failure                  |

**Guarantees on any failure:**

- The active claim is retained and retryable, unless its lifecycle operation completed.
- No partial summary is emitted, and stdout is empty.
- Quarantine counters are incremented only _after_ the lifecycle operation succeeds, so a failed
  quarantine cannot inflate a count.

## Operational notes

**Do not grant inbox write access to untrusted producers.** Producers are trusted same-user
processes and are not authenticated; harness names are attributed claims. The metadata checks
reject wrong owner, wrong device, wrong mode, symlinks, and hard links — they do not defend against
a same-user process holding a writable descriptor.

**Back up with a WAL-safe method.** Before migrating, stop Evidra writers and use a SQLite backup,
not a plain file copy of `evidra.db` — the write-ahead log may hold committed data.

**Ordinary commands never migrate.** `evidra open` validates a legacy database and then refuses
with a message naming `evidra init`. Only `evidra init` migrates. See
[`SCHEMA.md`](SCHEMA.md#migration).

**Durability across power loss is not claimed.** No `fsync` is issued in the inbox. Directory-entry
durability depends on the filesystem.

**Platform support.** The inbox is implemented on Linux and macOS via `rustix`. Elsewhere it
compiles to a **fail-closed stub**: `initialize` and every inbox operation return
`inbox filesystem is unsupported`, so `evidra ingest` exits `1` with `ingest failed: inbox` rather
than silently doing nothing. The Windows CI job exists only to prove the workspace still compiles.

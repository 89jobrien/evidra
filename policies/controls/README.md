# Controls

Bounded, reversible actions Evidra can take. Each control is deliberately small enough to undo.

A control is not a disposition. Dispositions are computed by deterministic code from policy intent
(ADR-005); controls are the actions those dispositions resolve to.

Format: one file per control, named for the control in kebab-case.

---

## C-001: Quarantine on digest conflict

- **Action**: Move an inbox item to quarantine with a `.reason` sidecar instead of admitting it.
- **Trigger**: A receipt exists for `(harness, session_id, source_event_id)` but the semantic digest
  differs, which means the same identity carried different content.
- **Bounded by**: Nothing else is touched. The claim is completed, the next item is processed, and
  the conflicting bytes are preserved verbatim for inspection.
- **Reversible by**: Moving the file back and deleting its receipt.
- **Why not deny**: Rejecting the batch would lose good evidence alongside the conflict.

## C-002: Claim before read

- **Action**: Take an exclusive, non-blocking lock on a private lock file, rename the item into the
  claim namespace, and only then read it.
- **Trigger**: Every inbox item, unconditionally.
- **Bounded by**: A failure at any step releases the lock and completes the claim, so a crashed run
  leaves an item claimed rather than half-written.
- **Reversible by**: Completing or quarantining the claim.
- **Why it matters**: Rename-before-read means a reader can never observe a partially written file.

## C-003: Fail closed on schema mismatch

- **Action**: Refuse to open a database whose schema version is not exactly the expected one.
- **Trigger**: `PRAGMA user_version` differs from `SCHEMA_VERSION`.
- **Bounded by**: Read-only. Nothing is migrated or written on a version mismatch.
- **Reversible by**: Running `evidra init`, which migrates through the ladder explicitly.
- **Why it matters**: Silently accepting a newer or partial schema would let a query read a ledger
  whose integrity guarantees it no longer has.

## C-004: Refuse an unregistered facet namespace

- **Action**: Reject a derivation carrying a namespace not present in the band table.
- **Trigger**: Every derived record write.
- **Bounded by**: The record is refused; the ledger is untouched.
- **Reversible by**: Registering the namespace, which is a reviewed change.
- **Why it matters**: A misspelled namespace would otherwise create a permanently unqueryable
  partition, and an unregistered one might expose content the redaction rule forbids (I-005).

## C-005: Refuse an assisted derivation above weak confidence

- **Action**: Reject a record whose method is assisted and whose confidence exceeds `Weak`.
- **Trigger**: Every derived record and relationship write.
- **Bounded by**: The record is refused; nothing else changes.
- **Reversible by**: Recording the derivation as deterministic, which requires the evidence to support
  reproducibility.
- **Why it matters**: Without it, a model's opinion could be cited as a finding (I-004).

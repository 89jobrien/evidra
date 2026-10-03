# Storage schema

The SQLite store, its append-only guarantees, its migration contract, and how it decides a database
is trustworthy. For operational procedure see [`OPERATIONS.md`](OPERATIONS.md); for the rationale
behind append-only storage see [ADR-002](adr/ADR-002-append-only-observations.md) and
[`ARCHITECTURE.md`](ARCHITECTURE.md).

## Identity and versioning

| Constant                 | Value         | Meaning                         |
| ------------------------ | ------------- | ------------------------------- |
| `APPLICATION_ID`         | `0x4556_4452` | Identifies the file as Evidra's |
| `LEGACY_SCHEMA_VERSION`  | `1`           | Observations only               |
| `RECEIPT_SCHEMA_VERSION` | `2`           | Adds harness identity receipts  |
| `SCHEMA_VERSION`         | `3`           | Current. Adds derived records   |

Identity lives in SQLite's `application_id`; version lives in `user_version`. Both are checked on
every open. A file that is not Evidra's, or is Evidra's at a version this binary does not know, is
rejected as `UnexpectedDatabase`. **Unknown versions are terminal, not downgradable** — no code path
lowers `user_version`.

## Tables

Schema v3 holds six append-only tables. `document` columns hold the serialized domain record; the
remaining columns are denormalized indexes over it, validated against it on every read.

### `observations` (v1)

```sql
CREATE TABLE IF NOT EXISTS observations (
    id TEXT PRIMARY KEY NOT NULL,
    observed_at TEXT NOT NULL,
    document TEXT NOT NULL
);
```

`observed_at` duplicates `document.observed_at` for ordering. It is not trusted independently —
`open()` rejects any row where the column and the document disagree.

### `agent_harness_receipts` (v2)

```sql
CREATE TABLE IF NOT EXISTS agent_harness_receipts (
    harness TEXT NOT NULL,
    session_id TEXT NOT NULL,
    source_event_id TEXT NOT NULL,
    event_digest TEXT NOT NULL,
    observation_id TEXT NOT NULL UNIQUE,
    PRIMARY KEY (harness, session_id, source_event_id),
    FOREIGN KEY (observation_id) REFERENCES observations(id)
);
```

The composite primary key is the **idempotency identity**; `event_digest` is what makes two events
under one identity a _conflict_ rather than a _duplicate_. The column-level `UNIQUE` on
`observation_id` enforces one receipt per observation.

### `derivations` (v3)

```sql
CREATE TABLE IF NOT EXISTS derivations (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('facet', 'aggregate', 'cluster')),
    recorded_at TEXT NOT NULL,
    supersedes TEXT,
    document TEXT NOT NULL
);
```

`supersedes` carries no foreign key. It is validated as a nullable string and cross-checked against
the document, and its chain is walked by the `supersedes` index.

### `derivation_facets` (v3)

```sql
CREATE TABLE IF NOT EXISTS derivation_facets (
    derivation_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    slot TEXT NOT NULL CHECK (slot IN ('text', 'integer', 'boolean')),
    text_value TEXT,
    integer_value INTEGER,
    boolean_value INTEGER CHECK (boolean_value IS NULL OR boolean_value IN (0, 1)),
    confidence TEXT NOT NULL
        CHECK (confidence IN ('speculative', 'weak', 'moderate', 'strong')),
    freshness TEXT NOT NULL CHECK (freshness IN ('current', 'aging', 'stale')),
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (derivation_id, namespace, name),
    FOREIGN KEY (derivation_id) REFERENCES derivations(id),
    CHECK (
        (slot = 'text'    AND text_value IS NOT NULL    AND integer_value IS NULL AND boolean_value IS NULL)
     OR (slot = 'integer' AND text_value IS NULL        AND integer_value IS NOT NULL AND boolean_value IS NULL)
     OR (slot = 'boolean' AND text_value IS NULL        AND integer_value IS NULL AND boolean_value IS NOT NULL)
    )
);
```

Three value columns and a `slot` discriminator is deliberate: a typed slot lets current-view
resolution filter without reparsing JSON, and the trailing `CHECK` is **stronger than the
application layer** — it holds even for a writer that bypasses the constructor. `open()` proves
this by attempting a slot/value disagreement and requiring rejection.

### `derivation_evidence` (v3)

```sql
CREATE TABLE IF NOT EXISTS derivation_evidence (
    derivation_id TEXT NOT NULL,
    target_key TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('supporting', 'refuting')),
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (derivation_id, target_key),
    FOREIGN KEY (derivation_id) REFERENCES derivations(id)
);
```

A pure index — no `document`, and no confidence or freshness, since confidence is a property of the
_derivation_, not of each piece of evidence. `target_key` is an opaque string that may name either
an observation or another derivation.

### `relationships` (v3)

```sql
CREATE TABLE IF NOT EXISTS relationships (
    id TEXT PRIMARY KEY NOT NULL,
    from_key TEXT NOT NULL,
    to_key TEXT NOT NULL,
    relation TEXT NOT NULL CHECK (relation IN (
        'supports', 'refutes', 'caused-by', 'enabled', 'prevented', 'supersedes',
        'derives-from', 'co-occurs-with', 'no-effect'
    )),
    confidence TEXT NOT NULL
        CHECK (confidence IN ('speculative', 'weak', 'moderate', 'strong')),
    recorded_at TEXT NOT NULL,
    document TEXT NOT NULL,
    CHECK (from_key <> to_key)
);
```

**No foreign keys here, and that is deliberate.** `from_key` and `to_key` may each name an
observation _or_ a derivation, so no single SQL foreign key can express the union. Referential
integrity for edges is checked by walking both ends during validation instead.

## Append-only enforcement

**18 triggers — three per table, all rejecting mutation:**

| Table                    | `_reject_update` | `_reject_delete` | `_reject_duplicate_insert`                 |
| ------------------------ | ---------------- | ---------------- | ------------------------------------------ |
| `observations`           | yes              | yes              | on `id`                                    |
| `agent_harness_receipts` | yes              | yes              | on identity triple **or** `observation_id` |
| `derivations`            | yes              | yes              | on `id`                                    |
| `derivation_facets`      | yes              | yes              | on full primary key                        |
| `derivation_evidence`    | yes              | yes              | on full primary key                        |
| `relationships`          | yes              | yes              | on `id`                                    |

`UPDATE` and `DELETE` triggers are unconditional — even a no-op `SET col = col` is aborted. The
duplicate-insert triggers are redundant with the primary key; they exist so an attempted replacement
reads as _append-only_ rather than as a constraint violation. `INSERT OR REPLACE` is caught too,
since it fires the delete path internally.

There are no other triggers — no maintenance, validation, or faceting triggers.

## Validation on open

`open()` does not open. It **probes**, because a store that may never be modified cannot
distinguish a modified row from a forged one without checking.

### Two techniques, both required

**Normalized DDL text comparison** catches a trigger that exists but has been neutered — same
name, same table, still syntactically valid, no longer raising. Names alone cannot detect this, and
neither can behavior:

```sql
CREATE TRIGGER observations_reject_update
BEFORE UPDATE ON observations
WHEN OLD.id = '__evidra_append_only_probe__'   -- passes a naive behavioral probe
BEGIN SELECT RAISE(ABORT, 'observations are append-only'); END;
```

So `open()` canonicalizes each trigger's stored SQL — collapse whitespace, lowercase, strip
`IF NOT EXISTS` — and compares it against the expected text.

**Live behavioral probes** inside a savepoint that is always rolled back. `SAVEPOINT`, attempt real
mutations, record which failed, `ROLLBACK TO` and `RELEASE`. No probe row survives.

Neither technique is sufficient alone: text comparison cannot verify foreign-key enforcement or
`CHECK` constraints, which depend on `PRAGMA foreign_keys` being on and on row data rather than on
schema text.

### What is checked

| Group       | Checks                                                                                                                                                    |
| ----------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Identity    | `application_id` and `user_version` both match                                                                                                            |
| Schema      | Every table, index, and trigger exists; trigger text matches; column order, type, nullability, and primary-key ordinal match                              |
| Controls    | Live probes: update, delete, insert-or-replace, duplicate insert, and — for receipts — same observation under a different identity, and an orphan insert  |
| Constraints | Facet slot/value disagreement, unknown slot, self-edge, unknown relation kind                                                                             |
| Rows        | Every stored document deserializes through its domain type, passes its integrity check, and agrees with its indexed columns                               |
| Referential | `foreign_key_check` returns no rows; no harness observation lacks a receipt; every receipt's identity and digest re-verify against the joined observation |

Validation is **nested**, not layered: the v3 path re-asserts all v2 invariants. A database
satisfying only the derived checks is not a v3 database, because the derived tables reference
`observations`.

### Why the read path re-derives

Because indexed columns duplicate data stored inside the document, they can disagree. Validation
re-derives each index from its document and rejects drift rather than returning a wrong answer. A
row whose `observed_at` column no longer matches its document is a hard failure.

## Migration

**`evidra init` is the only migration entry point.** Ordinary commands never migrate: `open()`
validates a legacy database, then refuses with `MigrationRequired`, naming the command and the target
version:

```text
database .evidra/evidra.db uses schema v1; back it up and run `evidra init` to migrate to v3
```

Validation runs **before** the refusal, so a corrupt legacy database reports its corruption instead
of misleadingly asking for a migration.

| Path    | Behavior                                                                                      |
| ------- | --------------------------------------------------------------------------------------------- |
| Fresh   | Applies all three DDL batches in one `IMMEDIATE` transaction, validates, then stamps identity |
| v1 → v2 | Adds the receipts table and its triggers                                                      |
| v2 → v3 | Adds the four derived tables, their triggers, and their indexes                               |
| v1 → v3 | Both steps, chained                                                                           |

### Migrations never rewrite stored documents

Four independent guarantees:

1. **The DDL is purely additive.** Every statement is `CREATE ... IF NOT EXISTS`. There is no
   `ALTER`, `DROP`, `UPDATE`, or `DELETE` in any schema constant or migration function.
2. **An explicit byte comparison inside the transaction.** `(id, document)` pairs are snapshotted in
   stable id order before the transaction and again inside it; any difference aborts the migration.
   The check runs before `user_version` is stamped and before commit, so a violation rolls back
   rather than being detected afterward.
3. **Full row validation** inside the transaction re-deserializes every stored document.
4. **Schema objects are preserved.** A migration test snapshots `sqlite_master` before and after and
   asserts every pre-existing object is still present byte for byte.

> The guard compares `id` and `document`, not every column. `observed_at` is protected indirectly —
> it is cross-checked against the unchanged document, so it cannot drift without the document
> drifting too.

### Failure is atomic

Every failure path returns while the transaction is uncommitted, so `Drop` reverts the DDL and the
version stamp together. This is proven for the case that matters most: a pre-existing malformed
table makes validation fail _after_ the new DDL has already executed, and the test asserts the
newly created triggers **do not exist** afterward. A failed v1→v2 leaves `user_version = 1`; a failed
v2→v3 leaves it at `2`.

## Compatibility surface

Stored JSON is a long-lived contract. Three rules govern it:

**Adding a field to a persisted type is a breaking change.** Persisted types use
`#[serde(deny_unknown_fields)]` (18 sites across `observation.rs`, `harness.rs`, `derivation.rs`), so
a stored document carrying an unrecognized field fails to deserialize. Adding a field requires
either regenerating stored documents or a migration that accepts both shapes.

**The serialized shape is read by SQL.** Validation uses SQLite's JSON1 functions — for example it
queries `json_extract(document, '$.kind')` to find harness observations missing a receipt. So
`document.kind` must keep its exact string value `'agent-harness-event'`. Changing it breaks
validation in a way that looks like data corruption.

**Wire formats are explicitly versioned** by string tag:

| Tag                                    | Where                       |
| -------------------------------------- | --------------------------- |
| `evidra.agent-harness-observation/v1`  | Stored harness observation  |
| `evidra.agent-harness-event-digest/v1` | Semantic digest computation |
| `evidra.agent-harness-event/v1`        | Wire format producers emit  |

A `/v2` is treated as unknown and rejected, and a test pins that the rendered output never contains
`/v2`.

## Indexes

| Index                                                     | Serves                                                                                                              |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `observations_observed_at_idx`                            | The reverse-chronological listing — the only index whose column definition is validated, including descending order |
| `derivations_recorded_at_idx`                             | Derived-layer listing                                                                                               |
| `derivations_supersedes_idx`                              | Supersession chain traversal                                                                                        |
| `derivation_facets_lookup_idx`                            | Current-view facet lookup                                                                                           |
| `derivation_evidence_target_idx`                          | Reverse evidence lookup                                                                                             |
| `relationships_from_key_idx` / `relationships_to_key_idx` | Out- and in-edges                                                                                                   |
| `relationships_relation_idx`                              | Relation-kind scan                                                                                                  |

The six derived indexes are **forward-looking**: no query in the store currently uses them, and
their column definitions are existence-checked but not verified the way
`observations_observed_at_idx` is.

## Further reading

| Document                                                    | Covers                                                 |
| ----------------------------------------------------------- | ------------------------------------------------------ |
| [`OPERATIONS.md`](OPERATIONS.md)                            | Backing up before a migration; CLI behavior            |
| [`ARCHITECTURE.md`](ARCHITECTURE.md)                        | Why append-only forces read-time validation            |
| [`CONVENTIONS.md`](CONVENTIONS.md)                          | The read-path revalidation rule these checks depend on |
| [ADR-002](adr/ADR-002-append-only-observations.md)          | Append-only decision                                   |
| [ADR-007](adr/ADR-007-sqlite-and-versioned-policy-files.md) | SQLite choice and policy files                         |

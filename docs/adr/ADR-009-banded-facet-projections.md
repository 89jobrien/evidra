# ADR-009: Facet values are banded, never raw

- Status: Proposed
- Date: 2026-10-02

## Context

Every stored payload is an opaque `serde_json::Value`. Answering the question the ledger exists to
answer — which failure classes recur in this crate across the last forty sessions — means parsing
every document on every query.

Projecting facets into indexed columns fixes the cost, but projection alone does not fix the
question. A raw `duration_ms` facet cannot support "is this a fast failure or a slow one"; only a
banded value can. Raw numerics also make bands unbounded in cardinality, which defeats grouping
entirely.

## Decision

Projected facets are stored in indexed columns beside the JSON document, so aggregation is a
`GROUP BY` rather than a re-parse.

Every numeric facet is banded. A banded integer facet may emit no more than eight distinct values.
Bands are declared in `evidra-engine` and versioned with the code that produces them. A namespace or
facet name that is not registered is rejected on write rather than stored, so a misspelling cannot
create a permanently unqueryable partition.

Raw values may be retained inside the JSON document for reference, but the indexed column always
holds the band.

## Consequences

Aggregation becomes cheap enough to be the default query path, and facet values are stable across
sessions, which is what makes "this recurred" answerable at all.

Banding is lossy by construction. A reader who needs the precise value reads the document, and any
consumer that wants thresholds must reason in band terms.

Because bands are versioned with the engine, re-tuning a band is not a free configuration change.
Previously derived rows hold the old band, so historical and current values become incomparable
without a migration. That is the intended trade: comparability over precision.

# Assumptions

Statements Evidra depends on to be true. Each one names what would invalidate it, so it can be
retired when that happens rather than outliving its evidence.

An assumption with no `invalidates_when` is not admissible. A recorded belief nobody has agreed how
to kill is how a stale assumption survives longer than the reasoning that justified it.

Format: one file per assumption, named for the assumption in kebab-case.

---

## A-001: The harness lifecycle event stream is sufficient evidence

- **Statement**: Agent harness lifecycle events carry enough signal to attribute an outcome to a
  cause without ingesting raw transcripts.
- **Held because**: Lifecycle events name the tool, its outcome, and the session boundary, which is
  what a failure attribution needs to distinguish a decision step from an execution step.
- **Invalidates when**: An attribution query cannot be answered from lifecycle events alone and raw
  transcript ingestion becomes necessary.
- **Review by**: 2026-12-01
- **Owner**: maintainer
- **Status**: accepted

## A-002: A single writer per repository is the expected operating model

- **Statement**: One operator, one process, one repository at a time is the normal case.
- **Held because**: The inbox takes an exclusive lock over a private lock file, which is correct for
  one writer and undefined for several.
- **Invalidates when**: Two processes are observed contending on the same inbox, or a multi-writer
  mode is requested.
- **Review by**: 2026-11-01
- **Owner**: maintainer
- **Status**: accepted

## A-003: Banded facets are sufficient for the questions Evidra is asked

- **Statement**: Questions about recurring operational patterns are answerable from banded facet
  values rather than exact measurements.
- **Held because**: The alternative, raw numerics, does not group: a millisecond value cannot
  distinguish a fast failure from a slow one (ADR-009).
- **Invalidates when**: A consumer requires an exact threshold comparison over a banded facet, or a
  band is re-tuned more than once, making historical values incomparable.
- **Review by**: 2026-12-01
- **Owner**: maintainer
- **Status**: accepted

## A-004: Derived records may be dropped without losing the ledger

- **Statement**: Superseded derivations are recomputable from the observation ledger, so retaining
  them indefinitely is unnecessary.
- **Held because**: Every derived record stores the scope it ran over, so a recomputation is a
  function of stored inputs.
- **Invalidates when**: A derived record is produced from evidence that has since been deleted from
  the ledger, or from an assisted method whose prompt version is no longer available.
- **Review by**: 2026-12-01
- **Owner**: maintainer
- **Status**: proposed

**Note**: A-004 is what makes a retention policy safe, and no retention policy exists yet. Until one
does, derived records accumulate without bound. This is recorded as a risk rather than a blocker
because the growth is slow and the data is not wrong, only more than needed.

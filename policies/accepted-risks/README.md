# Accepted Risks

Known weaknesses carried deliberately. Each entry states what was accepted, why, and what would
justify revisiting it.

A risk that is merely tolerated is not recorded here. This file is for risks someone chose.

Format: one file per risk, named for the risk in kebab-case.

---

## R-001: Attribution is expected to be wrong often

- **Risk**: Failure attribution will produce incorrect root causes at a substantial rate.
- **Accepted because**: Published step-level attribution results put best-in-class accuracy near 47%
  on synthetic benchmarks and near 29% on hand-crafted ones, with LLM-judge baselines near 14%. Any
  system claiming otherwise is not measuring what it claims to measure.
- **Mitigations**: Confidence is banded rather than continuous (ADR-011); every attribution carries
  its evidence chain; `CausedBy` is documented as a hypothesis, not a finding; attribution is
  advisory to a human and never gates a machine (ADR-005).
- **Revisit if**: Real-trace accuracy is measured and falls short of the threshold Slice 3 sets.
- **Accepted on**: 2026-10-02

## R-002: The step that manifests a failure is usually not the step that decided it

- **Risk**: Attribution that names the failing action rather than the deciding step is a category
  error, and it will be the common case rather than the exception.
- **Accepted because**: A failing call is frequently the mechanical consequence of a decision made
  earlier. Any attribution that stops at the failing call is measuring something easier than cause.
- **Mitigations**: The design permits multiple `CausedBy` edges into one node and records the
  method, so a consumer must read the full inbound set before concluding a single cause.
- **Revisit if**: Slice 3 measurements show single-step attribution is adequate on real traces.
- **Accepted on**: 2026-10-02

## R-003: Joint causes are not decomposable

- **Risk**: When two steps fail only together, single-link attribution is misleading in both
  directions — each looks either fully responsible or entirely irrelevant.
- **Accepted because**: Resolving this requires Shapley-style interaction analysis over every
  permutation, and caching intermediate values across permutations manufactures false confidence.
- **Mitigations**: Slice 3's gate forbids a sole-cause claim from the joint-cause fixture. The cost is
  a weaker claim: "these two together", rather than "this one".
- **Revisit if**: Performance allows full interaction analysis on the target trace volume.
- **Accepted on**: 2026-10-02

## R-004: Derived records grow without bound

- **Risk**: Superseded derivations are never deleted, because nothing in schema v3 may be deleted, and
  no retention policy exists.
- **Accepted because**: Deleting derived records would break the guarantee that a revision can always
  be traced to what it replaced. Assumption A-004 asserts recomputability, which is what would make
  retention safe, but that assumption is `proposed` and untested.
- **Mitigations**: None yet. Growth is slow relative to ledger growth because derivations are only
  produced deliberately.
- **Revisit if**: Derived volume becomes a measurable fraction of storage, or A-004 is retired.
- **Accepted on**: 2026-10-02

## R-005: Banding is lossy

- **Risk**: A facet band discards the precision that an exact threshold query would need.
- **Accepted because**: Raw numerics do not group, so the alternative is a query that answers no
  question (ADR-009). Retaining the raw value inside the document and banding only the indexed column
  bounds the loss.
- **Mitigations**: Raw values remain in the JSON document for reference. The loss is confined to the
  indexed projection.
- **Revisit if**: A consumer needs exact thresholds over a banded facet often enough to justify a
  second indexed representation.
- **Accepted on**: 2026-10-02

## R-006: A tracked handoff-style snapshot collides with branch switching

- **Risk**: A file that a hook rewrites and that is tracked will block `git checkout` whenever the
  working copy differs from the target branch.
- **Observed in**: The `godmode` repository, where promoting between branches failed with `local
changes would be overwritten by checkout`.
- **Accepted because**: Evidra does not write tracked files during hooks today, so this risk is
  prospective rather than active.
- **Mitigations**: None required yet. If Evidra later writes a tracked artifact from a hook, the
  restore belongs on the merge and checkout paths, not only the commit path.
- **Revisit if**: Evidra gains a hook that writes a tracked file.
- **Accepted on**: 2026-10-02

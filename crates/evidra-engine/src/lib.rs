//! Deterministic analysis operations for Evidra.
//!
//! This crate turns recorded evidence into derived records. It owns two decisions that the storage
//! layer must not make, because both are judgement calls about what a number means:
//!
//! - **Banding.** A raw `duration_ms` of 1234 cannot answer whether a failure is fast or slow, and
//!   an unbounded set of raw values makes grouping impossible (ADR-009). Every numeric facet is
//!   therefore reduced to one of at most [`MAX_BANDS`] declared bands, and bands are versioned with
//!   the code that produces them.
//! - **Redaction inheritance.** A facet computed from a redacted excerpt can reconstruct what the
//!   redaction removed (ADR-010). A derived record inherits the strictest redaction across its
//!   evidence set, and no emitted value may be a substring of, or derivable from, any source
//!   excerpt.
//!
//! Registration is the choke point for both. A namespace that is not registered is never projected,
//! so a misspelling cannot create a permanently unqueryable partition, and a namespace that would
//! describe observed content rather than a category cannot be registered at all.
//!
//! Everything here is deterministic: the same evidence and the same raw value always produce the
//! same projection, and `recorded_at` is supplied by the caller rather than read from the clock.
//! Nothing here performs I/O and this crate does not depend on `evidra-store`.

use std::fmt;

use evidra_core::{FacetProjection, FacetValue, RedactedExcerpt, RedactionRecord};
use miette::Diagnostic;
use thiserror::Error as ThisError;

/// The largest number of distinct values a banded integer facet may emit (ADR-009).
///
/// Eight is the number the ADR fixes. It is a cardinality bound, not a tuning knob: bands past it
/// stop being a grouping and start being the raw value again.
pub const MAX_BANDS: usize = 8;

/// One named, half-open interval over raw integer values.
///
/// Half-open on the right, so a value equal to a boundary belongs to the band above it and no
/// integer falls between two bands. An absent upper bound means the band extends without limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Band {
    name: String,
    lower: i64,
    upper: Option<i64>,
}

impl Band {
    /// Declares one band. Ordering and coverage are checked by [`BandTable::new`], not here, so
    /// that a caller may assemble bands in any order and have the table reject or accept the set
    /// as a whole.
    #[must_use]
    pub fn new(name: impl Into<String>, lower: i64, upper: Option<i64>) -> Self {
        Self {
            name: name.into(),
            lower,
            upper,
        }
    }

    /// Returns the stable persisted spelling of the band.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the inclusive lower bound.
    #[must_use]
    pub fn lower(&self) -> i64 {
        self.lower
    }

    /// Returns the exclusive upper bound, or `None` when the band is unbounded above.
    #[must_use]
    pub fn upper(&self) -> Option<i64> {
        self.upper
    }
}

/// What a facet namespace is permitted to describe.
///
/// The distinction is the whole of ADR-010. A classification names a category, so it says nothing
/// an excerpt did not already disclose and is admissible. A content facet names something the
/// excerpt contained, which is exactly what redaction was supposed to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetKind {
    /// Describes a category rather than content. Admissible for derived records.
    Classification,
    /// Describes observed content. Refused at registration and never projected.
    Content,
}

/// A versioned band table for one registered numeric facet.
///
/// The version is part of the table because bands are versioned with the code that produces them
/// (ADR-009). Re-tuning a band is therefore not a configuration change: rows written by an earlier
/// version hold the earlier band, and historical values become incomparable without a migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandTable {
    namespace: String,
    name: String,
    version: String,
    kind: FacetKind,
    bands: Vec<Band>,
}

impl BandTable {
    /// Declares a band table, rejecting any that is incomplete, overlapping, or too fine.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::BlankField`] when any identifier or band name is blank,
    /// [`EngineError::EmptyBandTable`] when no bands are declared, [`EngineError::TooManyBands`]
    /// when more than [`MAX_BANDS`] are declared, [`EngineError::UndeclaredBandName`] when a band
    /// name is not a snake-case identifier, [`EngineError::DuplicateBandName`] when two bands share
    /// a name, [`EngineError::UnorderedBands`] when bands are not in ascending order, and
    /// [`EngineError::IncompleteCoverage`] when the bands leave a gap, an overlap, or an unbounded
    /// edge.
    pub fn new(
        namespace: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
        kind: FacetKind,
        bands: Vec<Band>,
    ) -> Result<Self, EngineError> {
        let namespace = namespace.into();
        let name = name.into();
        let version = version.into();

        for (field, value) in [
            ("namespace", &namespace),
            ("facet name", &name),
            ("band table version", &version),
        ] {
            if value.trim().is_empty() {
                return Err(EngineError::BlankField {
                    field: field.to_owned(),
                });
            }
        }

        if bands.is_empty() {
            return Err(EngineError::EmptyBandTable);
        }
        if bands.len() > MAX_BANDS {
            return Err(EngineError::TooManyBands {
                declared: bands.len(),
                maximum: MAX_BANDS,
            });
        }

        for band in &bands {
            if band.name.trim().is_empty() {
                return Err(EngineError::BlankField {
                    field: "band name".to_owned(),
                });
            }
            if !is_snake_case_identifier(band.name()) {
                return Err(EngineError::UndeclaredBandName {
                    name: band.name().to_owned(),
                });
            }
        }
        for (index, band) in bands.iter().enumerate() {
            if bands[..index]
                .iter()
                .any(|earlier| earlier.name() == band.name())
            {
                return Err(EngineError::DuplicateBandName {
                    name: band.name().to_owned(),
                });
            }
        }

        for pair in bands.windows(2) {
            if pair[1].lower() <= pair[0].lower() {
                return Err(EngineError::UnorderedBands);
            }
        }

        if bands[0].lower() != i64::MIN {
            return Err(EngineError::IncompleteCoverage {
                reason: "the lowest band must extend without a lower bound",
            });
        }
        for pair in bands.windows(2) {
            let (below, above) = (&pair[0], &pair[1]);
            match (below.upper(), above.lower()) {
                (None, _) => {
                    return Err(EngineError::IncompleteCoverage {
                        reason: "an unbounded band must be the highest one",
                    });
                }
                (Some(upper), lower) if upper < lower => {
                    return Err(EngineError::IncompleteCoverage {
                        reason: "bands overlap",
                    });
                }
                (Some(upper), lower) if upper > lower => {
                    return Err(EngineError::IncompleteCoverage {
                        reason: "bands leave a gap",
                    });
                }
                (Some(_), _) => {}
            }
        }
        if bands.last().and_then(Band::upper).is_some() {
            return Err(EngineError::IncompleteCoverage {
                reason: "the highest band must extend without an upper bound",
            });
        }

        Ok(Self {
            namespace,
            name,
            version,
            kind,
            bands,
        })
    }

    /// Returns the namespace this table is registered under.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the facet name this table is registered under.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the band table version, which is the engine version that produced these bands.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns what this table is permitted to describe.
    #[must_use]
    pub fn kind(&self) -> FacetKind {
        self.kind
    }

    /// Returns the declared bands in ascending order.
    #[must_use]
    pub fn bands(&self) -> &[Band] {
        &self.bands
    }

    /// Returns the band a raw integer falls into.
    ///
    /// Total by construction: [`BandTable::new`] has already proved the bands cover every `i64`.
    /// The band with the greatest lower bound at or below `raw` is the only candidate, because the
    /// bounds are contiguous.
    #[must_use]
    pub fn band_of(&self, raw: i64) -> &Band {
        let index = self
            .bands
            .partition_point(|band| band.lower() <= raw)
            .saturating_sub(1);
        &self.bands[index]
    }
}

/// Returns whether `value` is a snake-case identifier.
///
/// A band name that is an identifier rather than prose cannot be a sentence scraped off an excerpt.
/// This is the mechanical form of "declare your categories, do not invent them per record".
fn is_snake_case_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.starts_with('_')
        && !value.ends_with('_')
}

/// The evidence a facet is being derived from.
///
/// Deliberately a borrowed view rather than an owned collection: the engine reads excerpts to decide
/// what it may not emit, and it never needs to retain them.
//
// TODO(HIGH): replace this derived `Debug` with a redacted one — it carries evidence.
//
// It holds excerpts and redaction attestations, so AGENTS.md requires a redacted `Debug`. Nothing
// leaks today only because both inner types redact themselves — an incidental property that breaks the
// moment a third field is added.
//
// The macro lives in `evidra-core/src/macros.rs`, declared crate-internal and not
// `#[macro_export]`, so this crate still cannot reach it: either export it or write the impl by hand,
// unconstrained on `'a` so the type stays `Debug` over readers that are not. See A-10 in
// `docs/AUDIT.md`, which pairs this with `ClaimedHarnessContent` in `evidra-core`.
#[derive(Debug, Clone, Copy)]
pub struct Evidence<'a> {
    excerpts: &'a [RedactedExcerpt],
    redactions: &'a [RedactionRecord],
}

impl<'a> Evidence<'a> {
    /// Pairs the excerpts and redaction attestations a derivation was computed from.
    #[must_use]
    pub fn new(excerpts: &'a [RedactedExcerpt], redactions: &'a [RedactionRecord]) -> Self {
        Self {
            excerpts,
            redactions,
        }
    }

    /// Returns the source excerpts the derivation read.
    #[must_use]
    pub fn excerpts(&self) -> &[RedactedExcerpt] {
        self.excerpts
    }

    /// Returns the strictest redaction across this evidence set (ADR-010).
    ///
    /// Inheriting the union rather than the maximum means a derived record can never weaken any
    /// redaction its evidence carries. With no attestations the derivation is unredacted, which is
    /// reported as `None` rather than as a permissive record.
    #[must_use]
    pub fn strictest_redaction(&self) -> Option<RedactionRecord> {
        let mut policy: Option<&RedactionRecord> = None;
        let mut transformations: Vec<String> = Vec::new();
        for record in self.redactions {
            if policy.is_none_or(|current| {
                record.transformations().len() > current.transformations().len()
            }) {
                policy = Some(record);
            }
            for transformation in record.transformations() {
                if !transformations.contains(transformation) {
                    transformations.push(transformation.clone());
                }
            }
        }
        policy.map(|policy| {
            let mut transformations = transformations;
            transformations.sort();
            // Rebuilding through the validating constructor keeps the inherited record subject to
            // the same rules as one a producer submitted.
            //
            // TODO(CRITICAL): move this fallibility into the signature instead of aborting the process
            // on the ADR-010 redaction-inheritance path.
            //
            // This is the only `expect` in production code in the workspace, and it sits on the decision
            // that stops a derived record from weakening any redaction its evidence carries. A failed
            // invariant aborts instead of surfacing a diagnosable error, on the one path whose failure
            // means the guarantee did not hold.
            //
            // Change the signature to `Result<Option<RedactionRecord>, EngineError>` and propagate, so
            // callers must decide what a broken redaction union means.
            //
            // It survived review because `AGENTS.md` forbids five constructs and the workspace lints
            // three — see A-11 in `docs/AUDIT.md`. Enabling the lint is the general remedy; this is the
            // specific hole it would have caught. See also A-08.
            RedactionRecord::new(policy.policy(), policy.version(), transformations)
                .expect("an attested redaction union stays valid")
        })
    }
}

/// The set of facets one engine version is permitted to emit.
///
/// A projection for anything not registered is refused rather than stored. That is what stops a
/// typo from creating a partition no query will ever find, and it is why the registry is the place
/// the redaction rule is enforced rather than a review checklist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    tables: Vec<BandTable>,
}

impl Registry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a band table, refusing anything that must not be emitted.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`BandTable::new`], plus [`EngineError::ContentNamespace`] when
    /// the table describes observed content rather than a category (ADR-010), and
    /// [`EngineError::DuplicateFacet`] when the namespace and name are already registered.
    pub fn register(&mut self, table: BandTable) -> Result<(), EngineError> {
        if table.kind() == FacetKind::Content {
            return Err(EngineError::ContentNamespace {
                namespace: table.namespace().to_owned(),
            });
        }
        if self.tables.iter().any(|existing| {
            existing.namespace() == table.namespace() && existing.name() == table.name()
        }) {
            return Err(EngineError::DuplicateFacet {
                namespace: table.namespace().to_owned(),
                name: table.name().to_owned(),
            });
        }
        self.tables.push(table);
        Ok(())
    }

    /// Returns the table registered for a namespace and name.
    #[must_use]
    pub fn table(&self, namespace: &str, name: &str) -> Option<&BandTable> {
        self.tables
            .iter()
            .find(|table| table.namespace() == namespace && table.name() == name)
    }

    /// Returns whether a namespace and name may be projected at all.
    #[must_use]
    pub fn is_registered(&self, namespace: &str, name: &str) -> bool {
        self.table(namespace, name).is_some()
    }

    /// Projects a raw integer onto the band its table declares, as a categorical facet.
    ///
    /// The emitted value is the band name from the declared table, never the raw number and never a
    /// string taken from the evidence. That is why the cardinality bound is a soundness property
    /// rather than a storage saving.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::UnregisteredFacet`] when no table is registered for the namespace and
    /// name, and [`EngineError::RedactionInheritanceViolation`] when the band name could be
    /// reconstructed from a source excerpt.
    pub fn project_banded(
        &self,
        namespace: &str,
        name: &str,
        raw: i64,
        evidence: &Evidence<'_>,
    ) -> Result<FacetProjection, EngineError> {
        let table = self
            .table(namespace, name)
            .ok_or_else(|| EngineError::UnregisteredFacet {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            })?;
        let band = table.band_of(raw);
        guard_redaction_inheritance(namespace, name, band.name(), evidence)?;
        FacetProjection::new(namespace, name, FacetValue::Text(band.name().to_owned())).map_err(
            |error| EngineError::ProjectionRefused {
                detail: error.to_string(),
            },
        )
    }

    /// Projects a caller-supplied category onto a registered facet.
    ///
    /// This is the chokepoint ADR-010 describes: a category that could have been read off an excerpt
    /// is refused, so the surviving categories are the ones that say something an excerpt did not.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::UnregisteredFacet`] when the facet is not registered,
    /// [`EngineError::BlankCategory`] when `category` is blank, and
    /// [`EngineError::RedactionInheritanceViolation`] when the category is a substring of, or
    /// derivable from, a source excerpt.
    pub fn project_category(
        &self,
        namespace: &str,
        name: &str,
        category: &str,
        evidence: &Evidence<'_>,
    ) -> Result<FacetProjection, EngineError> {
        if !self.is_registered(namespace, name) {
            return Err(EngineError::UnregisteredFacet {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            });
        }
        if category.trim().is_empty() {
            return Err(EngineError::BlankCategory);
        }
        guard_redaction_inheritance(namespace, name, category, evidence)?;
        FacetProjection::new(namespace, name, FacetValue::Text(category.to_owned())).map_err(
            |error| EngineError::ProjectionRefused {
                detail: error.to_string(),
            },
        )
    }
}

/// Refuses a value that a source excerpt could have supplied (ADR-010).
///
/// Two checks, both mechanical. The value must not appear inside an excerpt and an excerpt must not
/// appear inside it, which catches a copied secret or a copied sentence. And the value must not be
/// reconstructible from the excerpt's own words, which catches a paraphrase assembled out of terms
/// the excerpt already used.
//
// TODO(HIGH): ADR-010 names `no_facet_value_appears_in_source_excerpt` as the property that holds for
// arbitrary evidence sets, and the plan's Task 3, plus the 2026-10-03 and 2026-10-04 designs, all cite
// it as evidence the constraint is enforced. No such property exists. This function is the whole of the
// enforcement, exercised only by four fixed cases in `mod tests` — beginning with
// `verbatim_category_from_an_excerpt_is_refused`, alongside the derivable, genuine, and partially
// overlapping cases — none of which can say anything about arbitrary evidence. Write the property over
// arbitrary excerpt sets, or correct ADR-010 and the four documents citing it. See A-18 in
// `docs/AUDIT.md`.
fn guard_redaction_inheritance(
    namespace: &str,
    name: &str,
    value: &str,
    evidence: &Evidence<'_>,
) -> Result<(), EngineError> {
    let value = normalize(value);
    for excerpt in evidence.excerpts() {
        let text = normalize(excerpt.text());
        if text.contains(&value) {
            return Err(EngineError::RedactionInheritanceViolation {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                reason: RedactionBreach::Verbatim,
            });
        }
        if value.contains(&text) {
            return Err(EngineError::RedactionInheritanceViolation {
                namespace: namespace.to_owned(),
                name: value_name(name),
                reason: RedactionBreach::Superset,
            });
        }
        if is_derivable_from(&value, &text) {
            return Err(EngineError::RedactionInheritanceViolation {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                reason: RedactionBreach::Derivable,
            });
        }
    }
    Ok(())
}

/// Lowercases and collapses runs of whitespace so containment compares content, not layout.
fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Returns whether every content word of `value` appears in `excerpt`.
///
/// Words shorter than four characters are connective rather than content, so `in` or `the` cannot
/// make an unrelated value look derived. The test is deliberately strict: it is a refusal, so a
/// false positive costs a dropped record while a false negative leaks.
fn is_derivable_from(value: &str, excerpt: &str) -> bool {
    let mut content = value
        .split([' ', '-', '_', ':', '/'])
        .filter(|word| word.len() >= 4)
        .peekable();
    if content.peek().is_none() {
        return false;
    }
    content.all(|word| excerpt.contains(word))
}

/// Returns the facet name unchanged.
///
/// Present so the superset branch reads the same as its siblings rather than reaching for a
/// variable it already has.
fn value_name(name: &str) -> String {
    name.to_owned()
}

/// How a proposed facet value was reconstructed from a source excerpt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionBreach {
    /// The value appears verbatim inside an excerpt.
    Verbatim,
    /// The value contains an entire excerpt.
    Superset,
    /// Every content word of the value appears in one excerpt.
    Derivable,
}

impl fmt::Display for RedactionBreach {
    /// Names the breach for an error message.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self {
            Self::Verbatim => "the value appears verbatim in a source excerpt",
            Self::Superset => "the value contains a source excerpt",
            Self::Derivable => "the value is derivable from a source excerpt",
        };
        formatter.write_str(description)
    }
}

/// Error returned by band registration and facet projection.
#[derive(Debug, ThisError, Diagnostic)]
pub enum EngineError {
    /// A required identifier or name was blank.
    #[error("{field} must not be blank")]
    #[diagnostic(code(evidra::engine::blank_field))]
    BlankField {
        /// Name of the blank field.
        field: String,
    },

    /// A band table declared no bands.
    #[error("a band table must declare at least one band")]
    #[diagnostic(code(evidra::engine::empty_band_table))]
    EmptyBandTable,

    /// A band table declared more bands than the cardinality bound allows.
    #[error("band table declares {declared} bands; the maximum is {maximum}")]
    #[diagnostic(code(evidra::engine::too_many_bands))]
    TooManyBands {
        /// Number of bands declared.
        declared: usize,
        /// The bound that was exceeded.
        maximum: usize,
    },

    /// A band name was prose rather than a declared identifier.
    #[error("band name {name} is not a snake-case identifier")]
    #[diagnostic(code(evidra::engine::undeclared_band_name))]
    UndeclaredBandName {
        /// The rejected band name.
        name: String,
    },

    /// Two bands shared a name.
    #[error("band name {name} is declared more than once")]
    #[diagnostic(code(evidra::engine::duplicate_band_name))]
    DuplicateBandName {
        /// The repeated band name.
        name: String,
    },

    /// Bands were not supplied in ascending order.
    #[error("bands must be declared in ascending order of their lower bound")]
    #[diagnostic(code(evidra::engine::unordered_bands))]
    UnorderedBands,

    /// The declared bands do not cover every integer exactly once.
    #[error("band table does not cover every value: {reason}")]
    #[diagnostic(code(evidra::engine::incomplete_coverage))]
    IncompleteCoverage {
        /// What is wrong with the coverage.
        reason: &'static str,
    },

    /// A namespace describing observed content cannot be registered.
    #[error(
        "namespace {namespace} describes content and may not be derived from redacted evidence"
    )]
    #[diagnostic(code(evidra::engine::content_namespace))]
    ContentNamespace {
        /// The refused namespace.
        namespace: String,
    },

    /// The namespace and name are already registered.
    #[error("{namespace}.{name} is already registered")]
    #[diagnostic(code(evidra::engine::duplicate_facet))]
    DuplicateFacet {
        /// The namespace that was re-registered.
        namespace: String,
        /// The facet name that was re-registered.
        name: String,
    },

    /// A projection was requested for a facet this registry does not declare.
    #[error("{namespace}.{name} is not a registered facet")]
    #[diagnostic(code(evidra::engine::unregistered_facet))]
    UnregisteredFacet {
        /// The unregistered namespace.
        namespace: String,
        /// The unregistered facet name.
        name: String,
    },

    /// A categorical projection was requested with a blank category.
    #[error("a projected category must not be blank")]
    #[diagnostic(code(evidra::engine::blank_category))]
    BlankCategory,

    /// A proposed value could have been reconstructed from a source excerpt.
    #[error("{namespace}.{name} was refused: {reason}")]
    #[diagnostic(code(evidra::engine::redaction_inheritance_violation))]
    RedactionInheritanceViolation {
        /// The namespace whose projection was refused.
        namespace: String,
        /// The facet whose projection was refused.
        name: String,
        /// How the value was reconstructed.
        reason: RedactionBreach,
    },

    /// The domain refused a projection the engine had already validated.
    #[error("projection refused by the domain: {detail}")]
    #[diagnostic(code(evidra::engine::projection_refused))]
    ProjectionRefused {
        /// The domain's description of the refusal.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::error::Error;

    use evidra_core::AgentHarnessEventError;
    use miette::Diagnostic as _;

    use super::{
        Band, BandTable, EngineError, Evidence, FacetKind, MAX_BANDS, RedactionBreach, Registry,
    };

    /// Declares a well-formed four-band table for `duration_ms`.
    fn duration_table() -> BandTable {
        BandTable::new(
            "latency",
            "duration",
            "engine-0.1.0",
            FacetKind::Classification,
            vec![
                Band::new("instant", i64::MIN, Some(100)),
                Band::new("quick", 100, Some(1_000)),
                Band::new("slow", 1_000, Some(10_000)),
                Band::new("stalled", 10_000, None),
            ],
        )
        .expect("table should be valid")
    }

    /// A registry holding the `latency.duration` table and a categorical one.
    fn registry() -> Registry {
        let mut registry = Registry::new();
        registry
            .register(duration_table())
            .expect("duration table should register");
        registry
            .register(
                BandTable::new(
                    "friction",
                    "category",
                    "engine-0.1.0",
                    FacetKind::Classification,
                    vec![
                        Band::new("auth", i64::MIN, Some(0)),
                        Band::new("timeout", 0, Some(1)),
                        Band::new("dependency", 1, None),
                    ],
                )
                .expect("category table should be valid"),
            )
            .expect("category table should register");
        registry
    }

    fn excerpt(text: &str) -> Vec<evidra_core::RedactedExcerpt> {
        vec![
            evidra_core::RedactedExcerpt::new("tool-output", text)
                .expect("excerpt should be valid"),
        ]
    }

    /// Confirms every integer in a dense range lands in exactly one declared band.
    //
    // TODO(LOW): drop this example and let the `tests::props` proptests own the general property; keep
    // only the fixture-specific expected counts here.
    //
    // `band_is_total` in `tests::props` below is the stronger check over arbitrary tables and values.
    // This fixed-range version only pins what the `duration_table` fixture should produce, which is
    // worth keeping — but as a count assertion, not as a second statement of totality. See A-16 in
    // `docs/AUDIT.md`.
    #[test]
    fn band_is_total_over_a_dense_range() {
        let table = duration_table();
        let mut covered = [0_i64; 4];
        for raw in -500..2_000 {
            let band = table.band_of(raw);
            let index = table
                .bands()
                .iter()
                .position(|candidate| candidate.name() == band.name())
                .expect("band should be declared");
            covered[index] += 1;
        }
        assert_eq!(
            covered,
            [600, 900, 1_000, 0],
            "every value should be counted exactly once, and the top band should be unreached here"
        );
    }

    /// Confirms the band boundaries are half-open, so a boundary value joins the band above it.
    #[test]
    fn boundary_values_join_the_upper_band() {
        let table = duration_table();
        assert_eq!(table.band_of(i64::MIN).name(), "instant");
        assert_eq!(table.band_of(99).name(), "instant");
        assert_eq!(table.band_of(100).name(), "quick");
        assert_eq!(table.band_of(999).name(), "quick");
        assert_eq!(table.band_of(1_000).name(), "slow");
        assert_eq!(table.band_of(9_999).name(), "slow");
        assert_eq!(table.band_of(10_000).name(), "stalled");
        assert_eq!(table.band_of(i64::MAX).name(), "stalled");
    }

    /// Confirms a lower raw value never lands in a higher band than a greater one.
    //
    // TODO(LOW): delete this test — the identically-named proptest in `tests::props` already covers it
    // over arbitrary tables, and two tests with one name in one module invites confusion about which is
    // authoritative. See the note on `band_is_total_over_a_dense_range`; A-16 in `docs/AUDIT.md`.
    #[test]
    fn banding_is_monotonic() {
        let table = duration_table();
        let mut previous = table.band_of(i64::MIN);
        for raw in -10_000..20_000 {
            let current = table.band_of(raw);
            assert!(
                current.lower() >= previous.lower(),
                "{current:?} is below {previous:?} at {raw}"
            );
            previous = current;
        }
    }

    /// Confirms the cardinality bound is eight and that a ninth band is refused.
    #[test]
    fn band_cardinality_is_bounded_at_eight() {
        let bands = (0..=MAX_BANDS)
            .map(|index| Band::new(format!("b{index}"), index as i64, None))
            .collect::<Vec<_>>();
        assert!(
            BandTable::new("x", "y", "v", FacetKind::Classification, bands).is_err(),
            "a ninth band must exceed the cardinality bound"
        );

        let bounded = (0..MAX_BANDS)
            .map(|index| Band::new(format!("b{index}"), index as i64, None))
            .collect::<Vec<_>>();
        assert!(bounded.iter().all(|_| true));
    }

    /// Confirms a table leaving a gap, an overlap, or an unbounded edge is refused.
    #[test]
    fn incomplete_coverage_is_refused() {
        let gap = BandTable::new(
            "x",
            "y",
            "v",
            FacetKind::Classification,
            vec![Band::new("a", i64::MIN, Some(10)), Band::new("b", 20, None)],
        );
        assert!(matches!(gap, Err(EngineError::IncompleteCoverage { .. })));

        let overlap = BandTable::new(
            "x",
            "y",
            "v",
            FacetKind::Classification,
            vec![Band::new("a", i64::MIN, Some(10)), Band::new("b", 5, None)],
        );
        assert!(matches!(
            overlap,
            Err(EngineError::IncompleteCoverage { .. })
        ));

        let floating_low = BandTable::new(
            "x",
            "y",
            "v",
            FacetKind::Classification,
            vec![Band::new("a", 0, Some(10)), Band::new("b", 10, None)],
        );
        assert!(matches!(
            floating_low,
            Err(EngineError::IncompleteCoverage { .. })
        ));

        let bounded_top = BandTable::new(
            "x",
            "y",
            "v",
            FacetKind::Classification,
            vec![Band::new("a", i64::MIN, Some(10))],
        );
        assert!(matches!(
            bounded_top,
            Err(EngineError::IncompleteCoverage { .. })
        ));
    }

    /// Confirms descending band lower bounds are refused as unordered rather than as a coverage
    /// defect.
    ///
    /// Regression: `BandTable::new` documented `UnorderedBands` but never checked ordering, so a
    /// descending table surfaced as `IncompleteCoverage` and the diagnostic code existed only as a
    /// claim. Ordering is checked first so a mis-ordered table reports its actual cause.
    #[test]
    fn descending_bands_are_refused_as_unordered() {
        let descending = BandTable::new(
            "x",
            "y",
            "v",
            FacetKind::Classification,
            vec![
                Band::new("a", i64::MIN, Some(10)),
                Band::new("b", 10, Some(20)),
                Band::new("c", 5, None),
            ],
        );
        assert!(matches!(descending, Err(EngineError::UnorderedBands)));

        let repeated_lower = BandTable::new(
            "x",
            "y",
            "v",
            FacetKind::Classification,
            vec![
                Band::new("a", i64::MIN, Some(10)),
                Band::new("b", 10, None),
                Band::new("c", 10, None),
            ],
        );
        assert!(matches!(repeated_lower, Err(EngineError::UnorderedBands)));
    }

    /// Confirms a band name must be a declared identifier rather than prose.
    #[test]
    fn prose_band_names_are_refused() {
        let prose = BandTable::new(
            "x",
            "y",
            "v",
            FacetKind::Classification,
            vec![Band::new("the token was present", i64::MIN, None)],
        );
        assert!(matches!(prose, Err(EngineError::UndeclaredBandName { .. })));
    }

    /// Confirms a namespace describing observed content cannot be registered (ADR-010).
    #[test]
    fn content_namespace_is_refused_at_registration() {
        let mut registry = Registry::new();
        let table = BandTable::new(
            "friction",
            "observed_value",
            "engine-0.1.0",
            FacetKind::Content,
            vec![Band::new("seen", i64::MIN, None)],
        )
        .expect("a content table is structurally valid");

        let error = registry
            .register(table)
            .expect_err("content namespaces must be refused");
        assert!(matches!(error, EngineError::ContentNamespace { .. }));
        assert!(!registry.is_registered("friction", "observed_value"));
    }

    /// Confirms a facet registered twice is refused rather than silently shadowed.
    #[test]
    fn duplicate_registration_is_refused() {
        let mut registry = Registry::new();
        registry
            .register(duration_table())
            .expect("first registration should succeed");
        assert!(matches!(
            registry.register(duration_table()),
            Err(EngineError::DuplicateFacet { .. })
        ));
    }

    /// Confirms a raw integer becomes the band name, never the raw value.
    #[test]
    fn projection_emits_the_band_not_the_raw_value() {
        let registry = registry();
        let excerpts = excerpt("the tool finished in 1234ms");
        let evidence = Evidence::new(&excerpts, &[]);

        let projection = registry
            .project_banded("latency", "duration", 1_234, &evidence)
            .expect("a declared band should project");

        assert_eq!(projection.namespace(), "latency");
        assert_eq!(projection.name(), "duration");
        assert_eq!(
            projection.value(),
            &evidra_core::FacetValue::Text("slow".to_owned())
        );
    }

    /// Confirms projecting an unregistered facet is refused.
    #[test]
    fn unregistered_projection_is_refused() {
        let registry = registry();
        let evidence = Evidence::new(&[], &[]);
        assert!(matches!(
            registry.project_banded("latency", "nonexistent", 1, &evidence),
            Err(EngineError::UnregisteredFacet { .. })
        ));
    }

    /// Confirms a category copied verbatim out of an excerpt is refused (ADR-010).
    #[test]
    fn verbatim_category_from_an_excerpt_is_refused() {
        let registry = registry();
        let excerpts = excerpt("fatal: could not read credentials from the vault");
        let evidence = Evidence::new(&excerpts, &[]);

        let error = registry
            .project_category(
                "friction",
                "category",
                "could not read credentials",
                &evidence,
            )
            .expect_err("a category read off an excerpt must be refused");

        assert!(matches!(
            error,
            EngineError::RedactionInheritanceViolation {
                reason: RedactionBreach::Verbatim,
                ..
            }
        ));
    }

    /// Confirms a category assembled from an excerpt's own words is refused.
    ///
    /// `remote`, `socket`, and `waiting` all appear in the excerpt, so the category could have been
    /// written by reading it, which is precisely what ADR-010 forbids.
    #[test]
    fn derivable_category_from_an_excerpt_is_refused() {
        let registry = registry();
        let excerpts = excerpt("the operation stopped while waiting on the remote socket");
        let evidence = Evidence::new(&excerpts, &[]);

        let error = registry
            .project_category("friction", "category", "remote-socket-waiting", &evidence)
            .expect_err("a category built from excerpt words must be refused");

        assert!(matches!(
            error,
            EngineError::RedactionInheritanceViolation {
                reason: RedactionBreach::Derivable,
                ..
            }
        ));
    }

    /// Confirms a genuine category survives, which is the whole point of the check.
    #[test]
    fn genuine_category_survives_redaction_review() {
        let registry = registry();
        let excerpts = excerpt("the build script exceeded its time budget");
        let evidence = Evidence::new(&excerpts, &[]);

        registry
            .project_category("friction", "category", "toolchain_mismatch", &evidence)
            .expect("a category an excerpt does not contain must be admissible");
    }

    /// Confirms a category the excerpt only appears to contain is still refused.
    ///
    /// `killed` contains `kill`, so a substring test would clear this. Deriving a category from a
    /// word the excerpt merely contains is still reconstructing the excerpt.
    #[test]
    fn partially_overlapping_category_is_refused() {
        let registry = registry();
        let excerpts = excerpt("command exited with status 137 after being killed");
        let evidence = Evidence::new(&excerpts, &[]);

        assert!(
            registry
                .project_category("friction", "category", "oom_kill", &evidence)
                .is_err(),
            "a category assembled from a partially matching excerpt must be refused"
        );
    }

    /// Confirms the derived record inherits the union of its evidence's redactions.
    #[test]
    fn derived_record_inherits_the_strictest_redaction() {
        let strict = evidra_core::RedactionRecord::new(
            "obfsck",
            "2",
            vec!["secret-redaction".to_owned(), "pii-redaction".to_owned()],
        )
        .expect("record should be valid");
        let lenient =
            evidra_core::RedactionRecord::new("obfsck", "1", vec!["secret-redaction".to_owned()])
                .expect("record should be valid");
        let redactions = vec![lenient.clone(), strict.clone()];

        let inherited = Evidence::new(&[], &redactions)
            .strictest_redaction()
            .expect("evidence carries attestations");

        assert_eq!(inherited.transformations().len(), 2);
        assert!(
            inherited
                .transformations()
                .contains(&"pii-redaction".to_owned())
        );
        for transformation in lenient.transformations() {
            assert!(inherited.transformations().contains(transformation));
        }
    }

    /// Confirms the widest attestation decides which policy and version a derived record claims.
    ///
    /// Inheriting the *union* is not the same as inheriting the *record*. The union tells a reader
    /// what was cleared; the policy and version tell them which producer did it, and re-banding or
    /// re-redacting is not a free change (ADR-009, ADR-010). A test that only counts the union
    /// cannot tell a strictly-widest winner from a tie broken the wrong way.
    #[test]
    fn strictest_redaction_names_the_widest_attestation() {
        let lenient =
            evidra_core::RedactionRecord::new("obfsck", "1", vec!["secret-redaction".to_owned()])
                .expect("record should be valid");
        let strict = evidra_core::RedactionRecord::new(
            "obfsck",
            "2",
            vec!["secret-redaction".to_owned(), "pii-redaction".to_owned()],
        )
        .expect("record should be valid");

        let inherited = Evidence::new(&[], &[lenient.clone(), strict.clone()])
            .strictest_redaction()
            .expect("evidence carries attestations");
        assert_eq!(
            inherited.version(),
            "2",
            "a strictly wider attestation must supply the claimed policy version"
        );

        let reordered = Evidence::new(&[], &[strict, lenient])
            .strictest_redaction()
            .expect("evidence carries attestations");
        assert_eq!(
            reordered.version(),
            "2",
            "attestation order must not change which policy a derived record claims"
        );
    }

    /// Confirms a tie keeps the earlier attestation rather than handing the record to the later one.
    ///
    /// A tie is the only case where "widest" is ambiguous, so it is the only case that distinguishes
    /// a strictly-greater comparison from a greater-or-equal one. Last-writer-wins would make the
    /// claimed policy depend on the order a producer happened to emit its excerpts in.
    #[test]
    fn a_tied_attestation_keeps_the_earlier_policy() {
        let first =
            evidra_core::RedactionRecord::new("obfsck", "1", vec!["secret-redaction".to_owned()])
                .expect("record should be valid");
        let second = evidra_core::RedactionRecord::new(
            "other-obfuscator",
            "9",
            vec!["pii-redaction".to_owned()],
        )
        .expect("record should be valid");

        let inherited = Evidence::new(&[], &[first, second])
            .strictest_redaction()
            .expect("evidence carries attestations");

        assert_eq!(
            inherited.version(),
            "1",
            "an equally wide attestation must not displace the one already claimed"
        );
        assert_eq!(
            inherited.policy(),
            "obfsck",
            "an equally wide attestation must not relabel the producing policy"
        );
        assert_eq!(
            inherited.transformations(),
            ["pii-redaction", "secret-redaction"],
            "the union must still carry every transformation regardless of which record won"
        );
    }

    /// Confirms a table version is carried, because re-banding is not a free change (ADR-009).
    #[test]
    fn band_table_carries_its_version() {
        assert_eq!(duration_table().version(), "engine-0.1.0");
    }

    /// Confirms every engine error carries a stable, namespaced, unique diagnostic code.
    ///
    /// A diagnostic code is what a `--json` consumer matches on, so a missing, duplicated, or
    /// unnamespaced one is a silent API break rather than a cosmetic problem.
    #[test]
    fn diagnostic_codes_are_namespaced_and_unique() {
        let errors = vec![
            EngineError::BlankField {
                field: "namespace".to_owned(),
            },
            EngineError::EmptyBandTable,
            EngineError::TooManyBands {
                declared: 9,
                maximum: MAX_BANDS,
            },
            EngineError::UndeclaredBandName {
                name: "prose".to_owned(),
            },
            EngineError::DuplicateBandName {
                name: "quick".to_owned(),
            },
            EngineError::UnorderedBands,
            EngineError::IncompleteCoverage { reason: "gap" },
            EngineError::ContentNamespace {
                namespace: "friction".to_owned(),
            },
            EngineError::DuplicateFacet {
                namespace: "latency".to_owned(),
                name: "duration".to_owned(),
            },
            EngineError::UnregisteredFacet {
                namespace: "latency".to_owned(),
                name: "duration".to_owned(),
            },
            EngineError::BlankCategory,
            EngineError::RedactionInheritanceViolation {
                namespace: "friction".to_owned(),
                name: "category".to_owned(),
                reason: RedactionBreach::Verbatim,
            },
            EngineError::ProjectionRefused {
                detail: "refused".to_owned(),
            },
        ];

        let mut codes = BTreeSet::new();
        for error in &errors {
            let code = error
                .code()
                .expect("every engine error should carry a diagnostic code")
                .to_string();
            assert!(
                code.starts_with("evidra::engine::"),
                "unexpected diagnostic code {code}"
            );
            assert!(
                codes.insert(code.clone()),
                "duplicate diagnostic code {code}"
            );
        }
        assert_eq!(
            codes.len(),
            errors.len(),
            "every variant must be represented"
        );
    }

    /// Confirms the engine's error type stays convertible into a caller's error.
    #[test]
    fn engine_error_is_a_standard_error() {
        fn assert_error<E: Error + Send + Sync + 'static>() {}
        assert_error::<EngineError>();
        assert_error::<AgentHarnessEventError>();
    }

    #[cfg(test)]
    mod props {
        use proptest::prelude::*;

        use super::{Band, BandTable, Evidence, FacetKind, Registry};

        /// Arbitrary ascending band boundaries.
        fn boundaries() -> impl Strategy<Value = Vec<i64>> {
            prop::collection::vec(0i64..4_000, 0..7)
        }

        /// Builds a contiguous band table from arbitrary boundaries.
        ///
        /// Boundaries are sorted and de-duplicated before the table is built, because the bands of a
        /// valid table must tile the integer range without overlap or gap. Duplicates would
        /// otherwise produce a zero-width band, which is a different property being tested.
        fn table_from(boundaries: Vec<i64>) -> BandTable {
            let mut edges = boundaries;
            edges.sort_unstable();
            edges.dedup();
            let mut bands = edges
                .iter()
                .enumerate()
                .map(|(index, &lower)| {
                    Band::new(
                        format!("b{index}"),
                        if index == 0 { i64::MIN } else { lower },
                        edges.get(index + 1).copied(),
                    )
                })
                .collect::<Vec<_>>();
            if bands.is_empty() {
                bands.push(Band::new("only", i64::MIN, None));
            }
            BandTable::new("latency", "duration", "v", FacetKind::Classification, bands)
                .expect("sorted boundaries produce a total table")
        }

        proptest! {
            /// Every integer lands in exactly one band, and that band contains it.
            #[test]
            fn band_is_total(table in boundaries().prop_map(table_from), raw in any::<i64>()) {
                let band = table.band_of(raw);
                prop_assert!(raw >= band.lower());
                prop_assert!(band.upper().is_none_or(|upper| raw < upper));
            }

            /// A greater raw value never lands in a strictly lower band.
            #[test]
            fn banding_is_monotonic(
                table in boundaries().prop_map(table_from),
                left in any::<i64>(),
                right in any::<i64>(),
            ) {
                prop_assume!(left <= right);
                prop_assert!(table.band_of(left).lower() <= table.band_of(right).lower());
            }

            /// The declared cardinality is never exceeded, for any table this engine accepts.
            #[test]
            fn banding_never_exceeds_the_cardinality_bound(table in boundaries().prop_map(table_from))
            {
                prop_assert!(table.bands().len() <= super::MAX_BANDS);
            }

            /// A value a declared band table cannot contain is never projected.
            #[test]
            fn registration_is_the_choke_point(value in "[a-z_ ]{1,32}") {
                let mut registry = Registry::new();
                let table = BandTable::new(
                    "friction",
                    "observed_value",
                    "v",
                    FacetKind::Content,
                    vec![super::Band::new("seen", i64::MIN, None)],
                )
                .expect("structurally valid");
                prop_assert!(registry.register(table).is_err());
                prop_assert!(!registry.is_registered("friction", "observed_value"));
                let evidence = Evidence::new(&[], &[]);
                prop_assert!(
                    registry
                        .project_category("friction", "observed_value", &value, &evidence)
                        .is_err()
                );
            }
        }
    }
}

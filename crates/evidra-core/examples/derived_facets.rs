//! Derived records over the observation ledger.
//!
//! Shows the four ideas the derived layer rests on: a banded facet projection, a stored uncertainty
//! profile, a revision expressed as a supersedes chain rather than a mutation, and a model-assisted
//! method that cannot hold a confidence it has not earned.
//!
//! ```bash
//! cargo run -p evidra-core --example derived_facets
//! ```

use chrono::{Duration, TimeZone as _, Utc};
use evidra_core::{
    ConfidenceBand, Derivation, DerivationDraft, DerivationError, DerivationId, DerivationKind,
    DerivationMethod, DerivationScope, EvidenceRole, EvidenceTarget, FacetFilter, FacetProjection,
    FacetValue, FacetValueSlot, Freshness, RelationKind, Relationship, RelationshipId, SubjectRef,
    UncertaintyProfile,
};
use std::error::Error;

/// Builds one addressed facet projection.
fn facet(
    namespace: &str,
    name: &str,
    value: FacetValue,
) -> Result<FacetProjection, DerivationError> {
    FacetProjection::new(namespace, name, value)
}

fn main() -> Result<(), Box<dyn Error>> {
    let subject = SubjectRef::new("repository", "/workspace")?;
    let window_start = Utc
        .with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
        .single()
        .ok_or("ambiguous instant")?;
    let recorded_at = Utc
        .with_ymd_and_hms(2026, 10, 2, 9, 0, 0)
        .single()
        .ok_or("ambiguous instant")?;

    // A derivation records the window it ran over, not just its result. Without the scope it cannot
    // be recomputed or falsified.
    let scope = DerivationScope::new(
        subject.clone(),
        window_start,
        window_start + Duration::days(1),
        vec![FacetFilter::new(
            "outcome",
            "class",
            FacetValue::Text("verified-fail".into()),
        )?],
    )?;

    // 1. A deterministic derivation can hold the strongest band.
    let failed = Derivation::new(DerivationDraft {
        id: DerivationId::new(),
        kind: DerivationKind::Facet,
        scope: scope.clone(),
        profile: UncertaintyProfile::deterministic(),
        method: DerivationMethod::Deterministic {
            version: "engine-0.1.0".into(),
        },
        // Facets are banded, not raw: the question is "is this a fast or slow failure", and a
        // millisecond value cannot answer it (ADR-009).
        facets: vec![
            facet(
                "friction",
                "outcome",
                FacetValue::Text("verified-fail".into()),
            )?,
            facet("friction", "session", FacetValue::Text("m".into()))?,
        ],
        recorded_at,
        supersedes: None,
    })?;

    println!("first derivation   : {}", failed.id().as_str());
    println!(
        "  confidence       : {}",
        failed.profile().confidence().as_str()
    );
    println!("  content hash     : {}", hex(&failed.content_hash()?));
    println!("  supersedes       : {:?}", failed.supersedes());

    // 2. A correction appends. The original is untouched and stays readable, so the chain head and
    //    the retained predecessor are distinguishable (ADR-008).
    let corrected = Derivation::new(DerivationDraft {
        id: DerivationId::new(),
        kind: DerivationKind::Facet,
        scope: scope.clone(),
        profile: UncertaintyProfile::deterministic().with_freshness(Freshness::Current),
        method: DerivationMethod::Deterministic {
            version: "engine-0.1.0".into(),
        },
        facets: vec![
            facet(
                "friction",
                "outcome",
                FacetValue::Text("verified-fail".into()),
            )?,
            facet("friction", "session", FacetValue::Text("s".into()))?,
        ],
        recorded_at,
        supersedes: Some(failed.id()),
    })?;

    let supersedes = Relationship::new(
        RelationshipId::new(),
        EvidenceTarget::Derivation(corrected.id()),
        EvidenceTarget::Derivation(failed.id()),
        RelationKind::Supersedes,
        UncertaintyProfile::deterministic(),
        DerivationMethod::Deterministic {
            version: "engine-0.1.0".into(),
        },
        recorded_at,
    )?;

    println!("corrected derivation: {}", corrected.id().as_str());
    println!("  supersedes        : {:?}", corrected.supersedes());
    println!("  chain head        : {}", supersedes.from().key());
    println!("  retained          : {}", supersedes.to().key());

    // 3. Refuting evidence is a first-class role. It is recorded, never dropped, which is what keeps
    //    a derived record an argument rather than a claim (ADR-003).
    for role in [EvidenceRole::Supporting, EvidenceRole::Refuting] {
        println!("  evidence role     : {}", role.as_str());
    }

    // 4. An assisted method is capped. It may cluster and propose, but it cannot record a finding,
    //    and it never disposes (ADR-005, ADR-011).
    let assisted = DerivationMethod::Assisted {
        model: "example-model".into(),
        prompt_version: "cluster-v1".into(),
    };
    println!("\nassisted may dispose: {}", assisted.may_dispose());

    match Derivation::new(DerivationDraft {
        id: DerivationId::new(),
        kind: DerivationKind::Cluster,
        scope,
        profile: UncertaintyProfile::deterministic().with_confidence(ConfidenceBand::Strong),
        method: assisted.clone(),
        facets: Vec::new(),
        recorded_at,
        supersedes: None,
    }) {
        Ok(_) => println!("unexpected: assisted derivation accepted at strong confidence"),
        Err(error) => println!("assisted at strong  : refused ({error})"),
    }

    match Derivation::new(DerivationDraft {
        id: DerivationId::new(),
        kind: DerivationKind::Cluster,
        scope: DerivationScope::new(
            subject,
            window_start,
            window_start + Duration::days(1),
            Vec::new(),
        )?,
        profile: UncertaintyProfile::assisted(),
        method: assisted,
        facets: Vec::new(),
        recorded_at,
        supersedes: None,
    }) {
        Ok(record) => println!(
            "assisted at weak    : accepted ({})",
            record.profile().confidence().as_str()
        ),
        Err(error) => println!("unexpected: {error}"),
    }

    // 5. Redaction is inherited. A derived record describes category, never content, because a
    //    facet that reconstructs what redaction removed is a leak (ADR-010).
    let category = FacetFilter::new(
        "friction",
        "category",
        FacetValue::Text("auth-token-in-command".into()),
    )?;
    println!(
        "\nfacet describes category: {}.{}",
        category.namespace(),
        category.name()
    );
    println!(
        "  value              : {}",
        match category.value() {
            FacetValue::Text(value) => value.clone(),
            other => format!("{other:?}"),
        }
    );
    println!(
        "  slot               : {}",
        FacetValueSlot::of(category.value()).as_str()
    );
    println!("  the excerpt itself stays in the observation and is never promoted");

    Ok(())
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

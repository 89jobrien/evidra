//! Stage 2 content redaction, backed by `obfsck`.
//!
//! Stage 1 has already reduced a record to bounded candidates. This stage obfuscates each
//! candidate independently and records an ordered, named transformation log describing exactly what
//! the engine did — including the categories it could *not* act on.
//!
//! Four properties of the upstream engine shape this adapter, each verified against the crate
//! rather than assumed:
//!
//! 1. **Detection reporting is coarse.** `ObfuscationMapExport` exposes per-category maps plus a
//!    single `secrets_count`; the underlying secret set is private. The log can therefore say
//!    *that* secret detection fired, never *which pattern* matched.
//! 2. **Some content passes through unchanged.** `is_sensitive_path` matches credential-location
//!    paths and returns them verbatim, because preserving the name preserves the signal. That is
//!    right for log redaction and wrong for an evidence ledger, so Stage 1 drops such candidates
//!    wholesale. This stage re-checks independently and refuses rather than trusting that.
//! 3. **Two opt-in flags invert the safety property.** `with_allowlist` suppresses redaction for
//!    matching values and `with_pii(false)` skips structural PII. Neither is used, and a test
//!    pins it.
//! 4. **`Obfuscator` and `ObfuscationMap` derive `Debug` and that output contains the original
//!    values.** Neither type is ever stored in a field that derives `Debug`, and neither is
//!    formatted, anywhere in this adapter.

use std::fmt;

use evidra_core::{RedactionPolicy, Reduction};
use miette::Diagnostic;
use obfsck::{ObfuscationLevel, ObfuscationMapExport, Obfuscator};
use thiserror::Error;

use crate::claude_code::names_sensitive_location;

/// Policy name recorded in every attestation this adapter produces.
pub const POLICY_NAME: &str = "obfsck";

/// Content redaction performed by the upstream engine.
pub mod category {
    /// Internal IP addresses.
    pub const IP_INTERNAL: &str = "obfuscate:ip-internal";
    /// External IP addresses.
    pub const IP_EXTERNAL: &str = "obfuscate:ip-external";
    /// Hostnames.
    pub const HOST: &str = "obfuscate:host";
    /// Usernames.
    pub const USER: &str = "obfuscate:user";
    /// Container identifiers.
    pub const CONTAINER: &str = "obfuscate:container";
    /// Filesystem paths.
    pub const PATH: &str = "obfuscate:path";
    /// Email addresses.
    pub const EMAIL: &str = "obfuscate:email";
    /// Secret material, count only; the matching patterns are not exposed upstream.
    pub const SECRET: &str = "obfuscate:secret";
}

/// Failure returned when content redaction cannot establish a verdict.
///
/// Every variant aborts publication of the affected event. There is deliberately no way to express
/// "proceed without redacting", because a caller that cannot distinguish *no detections* from
/// *detection failed* has no way to fail closed.
#[derive(Clone, Copy, PartialEq, Eq, Error, Diagnostic)]
pub enum ObfsckPolicyError {
    /// A candidate named a credential location and would survive obfuscation unchanged.
    #[error("candidate names a credential location")]
    #[diagnostic(code(evidra::obfsck_policy::unsafe_candidate))]
    UnsafeCandidate,

    /// The engine produced a cleared value that violated the reduction contract.
    #[error("redacted candidate violated the reduction contract")]
    #[diagnostic(code(evidra::obfsck_policy::invalid_reduction))]
    InvalidReduction,
}

impl fmt::Debug for ObfsckPolicyError {
    /// Formats this error without exposing candidate content.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// `obfsck`-backed [`RedactionPolicy`] at one configured obfuscation level.
pub struct ObfsckPolicy {
    level: ObfuscationLevel,
    engine_version: String,
}

impl ObfsckPolicy {
    /// Builds a policy at `level`, recording `engine_version` in every attestation.
    ///
    /// `engine_version` is an explicit parameter rather than a compile-time constant because
    /// `CARGO_PKG_VERSION` in this crate resolves to *this* crate's version, not the engine's, and
    /// an attestation that names the wrong engine defeats its own purpose: it is what makes a
    /// redaction-policy change produce a new identity rather than an identity conflict. The caller
    /// is the component that knows which engine it linked.
    ///
    /// Neither `with_allowlist` nor `with_pii(false)` is called. An allowlist would exempt exactly
    /// the values this stage exists to catch, and disabling PII would leave emails, IPs, and
    /// usernames in the clear.
    ///
    /// # Errors
    ///
    /// Returns [`ObfsckPolicyError::InvalidReduction`] when `engine_version` is blank.
    pub fn new(
        level: ObfuscationLevel,
        engine_version: impl Into<String>,
    ) -> Result<Self, ObfsckPolicyError> {
        let engine_version = engine_version.into();
        if engine_version.trim().is_empty() {
            return Err(ObfsckPolicyError::InvalidReduction);
        }
        Ok(Self {
            level,
            engine_version: format!("{engine_version}+{}", level_name(level)),
        })
    }

    /// Borrows the attestation version, pinning both engine and level.
    #[must_use]
    pub fn attestation_version(&self) -> &str {
        &self.engine_version
    }

    /// Borrows the configured obfuscation level.
    #[must_use]
    pub fn level(&self) -> ObfuscationLevel {
        self.level
    }
}

impl fmt::Debug for ObfsckPolicy {
    /// Emits only the level, never engine state.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObfsckPolicy")
            .field("level", &level_name(self.level))
            .finish_non_exhaustive()
    }
}

impl RedactionPolicy for ObfsckPolicy {
    type Error = ObfsckPolicyError;

    fn redact(&self, kind: &str, text: &str) -> Result<Option<Reduction>, Self::Error> {
        // Independent of Stage 1, and deliberately so. Upstream returns credential-location paths
        // verbatim; if one reaches this point the reduction pipeline has a regression, and the only
        // safe response is to refuse the candidate rather than persist it.
        if names_sensitive_location(text) {
            return Err(ObfsckPolicyError::UnsafeCandidate);
        }

        // A fresh obfuscator per candidate. A shared instance would accumulate detections, and
        // `mapping()` would then credit this candidate with findings from an earlier one.
        let mut obfuscator = Obfuscator::new(self.level);
        let cleared = obfuscator.obfuscate(text);
        let transformations = categorise(&obfuscator.mapping());

        Reduction::new(kind, cleared, transformations)
            .map(Some)
            .map_err(|_| ObfsckPolicyError::InvalidReduction)
    }
}

/// Orders reported detections into the stable transformation log.
///
/// The order is fixed rather than derived from map iteration so that two runs over identical input
/// produce byte-identical attestations, which is what makes republication a duplicate instead of a
/// spurious identity conflict.
fn categorise(export: &ObfuscationMapExport) -> Vec<String> {
    let mut names = Vec::new();
    for (map, name) in [
        (&export.ips, category::IP_INTERNAL),
        (&export.ips, category::IP_EXTERNAL),
        (&export.hostnames, category::HOST),
        (&export.users, category::USER),
        (&export.containers, category::CONTAINER),
        (&export.paths, category::PATH),
        (&export.emails, category::EMAIL),
    ] {
        if !map.is_empty() && !names.contains(&name.to_owned()) {
            names.push(name.to_owned());
        }
    }
    // Secrets are reported as a count only, so the log says that detection fired without claiming
    // how many patterns matched or which.
    if export.secrets_count > 0 {
        names.push(category::SECRET.to_owned());
    }
    names
}

/// Returns the stable kebab-case name of an obfuscation level.
fn level_name(level: ObfuscationLevel) -> &'static str {
    match level {
        ObfuscationLevel::Minimal => "minimal",
        ObfuscationLevel::Standard => "standard",
        ObfuscationLevel::Paranoid => "paranoid",
    }
}

#[cfg(test)]
mod tests {
    use super::{ObfsckPolicy, ObfsckPolicyError, categorise, category};
    use evidra_core::{OBFUSCATE, RedactionPolicy, is_well_formed};
    use obfsck::ObfuscationLevel;

    /// Every transformation name this adapter can emit.
    const ALL_CATEGORIES: &[&str] = &[
        category::IP_INTERNAL,
        category::IP_EXTERNAL,
        category::HOST,
        category::USER,
        category::CONTAINER,
        category::PATH,
        category::EMAIL,
        category::SECRET,
    ];

    fn policy() -> ObfsckPolicy {
        ObfsckPolicy::new(ObfuscationLevel::Standard, "0.2.0")
            .unwrap_or_else(|error| panic!("policy should construct: {error:?}"))
    }

    #[test]
    fn clean_prose_passes_through_with_no_transformation() {
        let out = policy()
            .redact("assistant-text", "refactored the store adapter")
            .unwrap_or_else(|error| panic!("policy should succeed: {error:?}"));
        let value = out.unwrap_or_else(|| panic!("clean prose should be retained"));
        assert_eq!(value.text(), "refactored the store adapter");
        assert!(
            !value.transformed(),
            "clean prose recorded a transformation"
        );
    }

    #[test]
    fn an_email_is_obfuscated_and_named() {
        let out = policy()
            .redact("assistant-text", "contact joe@example.com for access")
            .unwrap_or_else(|error| panic!("policy should succeed: {error:?}"));
        let value = out.unwrap_or_else(|| panic!("candidate should be retained"));
        assert!(!value.text().contains("joe@example.com"), "email survived");
        assert!(
            value
                .transformations()
                .contains(&"obfuscate:email".to_owned())
        );
    }

    /// The gap the verification pass found: upstream returns credential paths verbatim, so the
    /// policy must refuse rather than inherit the default.
    #[test]
    fn a_credential_location_candidate_is_refused_rather_than_cleared() {
        for probe in [
            "the key is at /Users/joe/.aws/credentials",
            "see /etc/shadow",
            "read /Users/joe/.ssh/id_rsa",
        ] {
            let error = policy()
                .redact("assistant-text", probe)
                .expect_err("credential path must be refused");
            assert_eq!(error, ObfsckPolicyError::UnsafeCandidate);
        }
    }

    #[test]
    fn secret_detection_is_recorded_without_naming_a_pattern() {
        let out = policy()
            .redact("assistant-text", "token AKIAIOSFODNN7EXAMPLE here")
            .unwrap_or_else(|error| panic!("policy should succeed: {error:?}"));
        let value = out.unwrap_or_else(|| panic!("candidate should be retained"));
        assert!(
            value
                .transformations()
                .contains(&"obfuscate:secret".to_owned())
        );
        for name in value.transformations() {
            assert!(
                !name.contains("AKIA") && !name.contains("aws_key"),
                "transformation must not echo a matched pattern: {name}"
            );
        }
    }

    /// A fresh obfuscator per candidate is the only way `mapping()` stays attributable.
    #[test]
    fn detections_do_not_leak_across_candidates() {
        let policy = policy();
        let first = policy
            .redact("assistant-text", "mail a@example.com")
            .unwrap_or_else(|error| panic!("first should succeed: {error:?}"))
            .unwrap_or_else(|| panic!("first should be retained"));
        let second = policy
            .redact("assistant-text", "plain prose with nothing sensitive")
            .unwrap_or_else(|error| panic!("second should succeed: {error:?}"))
            .unwrap_or_else(|| panic!("second should be retained"));
        assert!(
            first
                .transformations()
                .contains(&"obfuscate:email".to_owned())
        );
        assert!(
            !second.transformed(),
            "second candidate inherited the first's detections: {:?}",
            second.transformations()
        );
    }

    #[test]
    fn the_attestation_pins_both_engine_and_level() {
        assert_eq!(policy().attestation_version(), "0.2.0+standard");
        let paranoid = ObfsckPolicy::new(ObfuscationLevel::Paranoid, "0.2.0")
            .unwrap_or_else(|error| panic!("should construct: {error:?}"));
        assert_eq!(paranoid.attestation_version(), "0.2.0+paranoid");
    }

    /// The attestation must never name this crate's own version by accident. `CARGO_PKG_VERSION`
    /// resolves to the *consuming* crate, so inlining it would silently pin the wrong engine.
    #[test]
    fn the_attestation_does_not_leak_the_consuming_crate_version() {
        assert!(
            !policy()
                .attestation_version()
                .starts_with(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn a_blank_engine_version_is_refused() {
        let error = ObfsckPolicy::new(ObfuscationLevel::Standard, "  ")
            .expect_err("blank engine version must be refused");
        assert_eq!(error, ObfsckPolicyError::InvalidReduction);
    }

    #[test]
    fn identical_input_yields_identical_ordering() {
        let input = "path /Users/joe/dev/x and mail a@example.com and ip 10.0.0.1";
        let first = policy()
            .redact("assistant-text", input)
            .unwrap_or_else(|error| panic!("should succeed: {error:?}"))
            .unwrap_or_else(|| panic!("should be retained"));
        let second = policy()
            .redact("assistant-text", input)
            .unwrap_or_else(|error| panic!("should succeed: {error:?}"))
            .unwrap_or_else(|| panic!("should be retained"));
        assert_eq!(
            first.transformations(),
            second.transformations(),
            "attestation ordering must be deterministic"
        );
    }

    #[test]
    fn every_category_is_namespaced_to_the_obfuscate_family() {
        for name in ALL_CATEGORIES {
            assert!(is_well_formed(name), "{name} must be namespaced");
            assert!(name.starts_with(OBFUSCATE), "{name} must use obfuscate:");
        }
    }

    #[test]
    fn an_empty_report_yields_no_names() {
        let export = obfsck::ObfuscationMapExport::default();
        assert!(categorise(&export).is_empty());
    }

    /// The policy must never surface upstream's leaky `Debug` implementations, which contain the
    /// original values behind every token.
    #[test]
    fn policy_debug_never_exposes_engine_state() {
        let rendered = format!("{:?}", policy());
        assert!(rendered.contains("standard"));
        assert!(!rendered.contains("ObfuscationMap"));
    }

    #[test]
    fn error_debug_is_source_free() {
        assert!(
            !format!("{:?}", ObfsckPolicyError::UnsafeCandidate).contains("aws"),
            "error must not echo candidate text"
        );
    }

    /// The safety-inverting options are deliberately *not* pinned by source scanning. An earlier
    /// attempt did exactly that and failed twice for the same reason: this file's own documentation
    /// names `with_pii` and `with_allowlist` when explaining why they are unused, so any textual
    /// check matches its own rationale. Reading one's own source is not a test.
    ///
    /// The real guards are behavioural and already present: `an_email_is_obfuscated_and_named` fails
    /// if PII is ever disabled, `ObfsckPolicy` exposes no way to configure an allowlist at all, and
    /// `policy_debug_never_exposes_engine_state` checks the diagnostic surface directly.
    #[test]
    fn obfuscated_output_is_deterministic_for_identical_input() {
        let policy = policy();
        let probe = "mail a@example.com from 10.0.0.5";
        let first = policy
            .redact("assistant-text", probe)
            .unwrap_or_else(|error| panic!("should succeed: {error:?}"))
            .unwrap_or_else(|| panic!("should be retained"));
        let second = policy
            .redact("assistant-text", probe)
            .unwrap_or_else(|error| panic!("should succeed: {error:?}"))
            .unwrap_or_else(|| panic!("should be retained"));
        assert_eq!(
            first.text(),
            second.text(),
            "token assignment must be stable, or republication is a conflict rather than a duplicate"
        );
    }
}

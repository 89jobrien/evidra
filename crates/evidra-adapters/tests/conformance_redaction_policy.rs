//! §13 — `RedactionPolicy` port substitutability.
//!
//! One shared assertion body, run against every implementation of the port. The body is written
//! against the clauses in [`docs/conformance.md`](../../../docs/conformance.md) and never against a
//! concrete policy, so a second implementation cannot quietly diverge from the first.
//!
//! `StubPolicy` is the reference implementation: it is written from the clauses alone and exists so
//! that a failing §13 clause can be attributed to a wrong clause rather than to a wrong policy.

use std::fmt;

use evidra_core::{RedactionPolicy, Reduction, is_well_formed};
use obfsck::ObfuscationLevel;

use evidra_adapters::{ObfsckPolicy, names_sensitive_location};

/// Candidate probes exercised by every clause.
///
/// The set mixes clean prose, an email, an access-key-shaped token, a credential location, and a
/// blank candidate. A clause that only ever saw clean prose would pass against a policy that
/// redacts nothing, which is the failure mode §13.3 exists to catch.
const PROBES: &[(&str, &str)] = &[
    ("assistant-text", "refactored the store adapter"),
    ("user-prompt", "run cargo nextest across the workspace"),
    (
        "assistant-text",
        "contact joe@example.com for access to the deploy key",
    ),
    ("assistant-text", "token AKIAIOSFODNN7EXAMPLE here"),
    ("tool-output", "mail a@example.com from 10.0.0.5"),
    ("assistant-text", "the key is at /Users/joe/.ssh/id_rsa"),
    ("assistant-text", "\u{2003}\u{2003}"),
];

/// Redacts `text` and names which of the three outcomes occurred.
#[derive(Debug, PartialEq)]
enum Verdict {
    /// The policy retained a cleared candidate.
    Retained(Reduction),
    /// The policy withheld the candidate.
    Withheld,
    /// The policy could not establish a verdict.
    Failed,
}

/// Applies `policy` to one probe, discarding the failure detail after §13.7 has checked it.
fn redact<P: RedactionPolicy>(policy: &P, kind: &str, text: &str) -> Verdict {
    match policy.redact(kind, text) {
        Ok(Some(reduction)) => Verdict::Retained(reduction),
        Ok(None) => Verdict::Withheld,
        Err(error) => {
            // §13.7 — a failure is the one path a caller will log, so it must carry no evidence.
            let rendered = format!("{error} {error:?}");
            assert!(
                !rendered.contains(text),
                "§13.7: a redaction failure leaked the candidate text: {rendered}"
            );
            Verdict::Failed
        }
    }
}

/// Asserts every §13 clause against one `RedactionPolicy` implementation.
fn assert_redaction_policy_contract<P: RedactionPolicy>(policy: P) {
    assert_kinds_are_preserved(&policy);
    assert_transformation_logs_are_well_formed(&policy);
    assert_attestations_do_not_lie(&policy);
    assert_identical_input_yields_identical_attestation(&policy);
    assert_candidates_do_not_share_state(&policy);
    assert_credential_locations_are_never_retained(&policy);
    assert_failures_carry_no_evidence(&policy);
}

/// §13.1 — a retained reduction keeps the category the caller asked about.
fn assert_kinds_are_preserved<P: RedactionPolicy>(policy: &P) {
    for (kind, text) in PROBES {
        if let Verdict::Retained(reduction) = redact(policy, kind, text) {
            assert_eq!(
                reduction.kind(),
                *kind,
                "§13.1: a reduction must preserve the requested candidate category"
            );
        }
    }
}

/// §13.2 — every emitted transformation is `family:action` and appears at most once.
fn assert_transformation_logs_are_well_formed<P: RedactionPolicy>(policy: &P) {
    for (kind, text) in PROBES {
        if let Verdict::Retained(reduction) = redact(policy, kind, text) {
            for name in reduction.transformations() {
                assert!(
                    is_well_formed(name),
                    "§13.2: transformation {name} is not namespaced as family:action"
                );
            }
            let mut seen = std::collections::HashSet::new();
            for name in reduction.transformations() {
                assert!(
                    seen.insert(name.as_str()),
                    "§13.2: transformation {name} appears more than once in one log"
                );
            }
        }
    }
}

/// §13.3 — a reduction that claims a transformation must not return its input verbatim.
fn assert_attestations_do_not_lie<P: RedactionPolicy>(policy: &P) {
    for (kind, text) in PROBES {
        if let Verdict::Retained(reduction) = redact(policy, kind, text)
            && reduction.transformed()
        {
            assert_ne!(
                reduction.text(),
                *text,
                "§13.3: the attestation claims transformations but the text is unchanged"
            );
        }
    }
}

/// §13.4 — the same candidate redacted twice yields the same attestation.
fn assert_identical_input_yields_identical_attestation<P: RedactionPolicy>(policy: &P) {
    for (kind, text) in PROBES {
        let first = redact(policy, kind, text);
        let second = redact(policy, kind, text);
        match (first, second) {
            (Verdict::Retained(left), Verdict::Retained(right)) => {
                assert_eq!(
                    left.text(),
                    right.text(),
                    "§13.4: identical input must produce identical retained text, or republication is a conflict rather than a duplicate"
                );
                assert_eq!(
                    left.transformations(),
                    right.transformations(),
                    "§13.4: identical input must produce an identical transformation log"
                );
            }
            (Verdict::Withheld, Verdict::Withheld) | (Verdict::Failed, Verdict::Failed) => {}
            (left, right) => {
                panic!("§13.4: identical input produced two different verdicts: {left:?} {right:?}")
            }
        }
    }
}

/// §13.5 — a candidate's verdict does not depend on what the policy saw before it.
fn assert_candidates_do_not_share_state<P: RedactionPolicy>(policy: &P) {
    let sensitive = "contact joe@example.com for access";
    let clean = "refactored the store adapter";

    let sensitive_after_clean = redact(policy, "assistant-text", sensitive);
    let clean_after_sensitive = redact(policy, "assistant-text", clean);
    let clean_alone = redact(policy, "assistant-text", clean);
    let sensitive_alone = redact(policy, "assistant-text", sensitive);

    assert_eq!(
        sensitive_alone, sensitive_after_clean,
        "§13.5: a previous clean candidate must not change a later sensitive one"
    );
    assert_eq!(
        clean_alone, clean_after_sensitive,
        "§13.5: a previous sensitive candidate must not change a later clean one"
    );
}

/// §13.6 — a candidate naming a credential location never comes back uncleared.
fn assert_credential_locations_are_never_retained<P: RedactionPolicy>(policy: &P) {
    for probe in [
        "the key is at /Users/joe/.ssh/id_rsa",
        "read /Users/joe/.aws/credentials before deploying",
        "check /etc/shadow",
        "rotate /home/build/.kube/config",
    ] {
        match redact(policy, "assistant-text", probe) {
            Verdict::Retained(reduction) => assert!(
                !reduction.text().contains(probe),
                "§13.6: a credential location was returned uncleared: {}",
                reduction.kind()
            ),
            Verdict::Withheld | Verdict::Failed => {}
        }
    }
}

/// §13.7 — a failure renders without the evidence it failed to redact.
///
/// Only the failure path is examined. A policy that never fails cannot leak through an error, and
/// demanding that it fail would assert something the port does not promise.
fn assert_failures_carry_no_evidence<P: RedactionPolicy>(policy: &P) {
    const EVIDENCE: &[(&str, &str)] = &[
        ("the key is at /Users/joe/.ssh/id_rsa", "id_rsa"),
        (
            "contact joe@example.com for access to the deploy key",
            "joe@example.com",
        ),
        ("token AKIAIOSFODNN7EXAMPLE here", "AKIAIOSFODNN7EXAMPLE"),
    ];

    for (probe, token) in EVIDENCE {
        let Err(error) = policy.redact("assistant-text", probe) else {
            continue;
        };
        let rendered = format!("{} {:?}", error, error);
        assert!(
            !rendered.contains(token),
            "§13.7: a redaction failure echoed evidence it was supposed to clear: {rendered}"
        );
    }
}

// ---------------------------------------------------------------------------
// Production implementation
// ---------------------------------------------------------------------------

#[test]
fn obfsck_policy_satisfies_contract() {
    let policy = ObfsckPolicy::new(ObfuscationLevel::Standard, "0.2.0")
        .unwrap_or_else(|error| panic!("policy should construct: {error:?}"));

    assert_redaction_policy_contract(policy);
}

// ---------------------------------------------------------------------------
// Reference implementation
// ---------------------------------------------------------------------------

/// Failure returned by the reference policy, written so it carries no candidate content.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StubError {
    /// The candidate named a credential location and would survive uncleared.
    UnsafeCandidate,
    /// The candidate carried no content to clear.
    BlankCandidate,
    /// The cleared candidate violated the reduction contract.
    InvalidReduction,
}

impl fmt::Display for StubError {
    /// Writes a fixed message that names no candidate value.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsafeCandidate => "candidate names a credential location",
            Self::BlankCandidate => "candidate carried no content",
            Self::InvalidReduction => "cleared candidate violated the reduction contract",
        })
    }
}

impl fmt::Debug for StubError {
    /// Renders the same source-free message as `Display`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for StubError {}

/// Reference `RedactionPolicy` written from the §13 clauses alone.
struct StubPolicy;

impl RedactionPolicy for StubPolicy {
    type Error = StubError;

    /// §13.6 refuses credential locations; §13.1 preserves the kind; §13.3 changes any text it
    /// claims to have transformed.
    fn redact(&self, kind: &str, text: &str) -> Result<Option<Reduction>, Self::Error> {
        if names_sensitive_location(text) {
            return Err(StubError::UnsafeCandidate);
        }
        if text.trim().is_empty() {
            return Err(StubError::BlankCandidate);
        }

        let cleared = text.replace('@', " at ");
        let transformations = if cleared == text {
            Vec::new()
        } else {
            vec!["obfuscate:email".to_owned()]
        };
        Reduction::new(kind, cleared, transformations)
            .map(Some)
            .map_err(|_| StubError::InvalidReduction)
    }
}

#[test]
fn stub_policy_satisfies_contract() {
    assert_redaction_policy_contract(StubPolicy);
}

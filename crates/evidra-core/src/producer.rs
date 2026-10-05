//! Producer-side redaction policy for harness evidence.
//!
//! A producer reduces a harness record to candidate strings before any content is retained, then
//! offers each candidate to a [`RedactionPolicy`]. The policy either withholds it or returns a
//! [`Reduction`] carrying the text that may be retained together with an ordered log of every
//! transformation applied.
//!
//! The policy receives one candidate at a time and cannot observe the rest of the record, so it
//! cannot widen its own visibility. It performs no I/O, reads no clock, and decides nothing about
//! identity, deduplication, or persistence; those belong to the producer and to the ledger.

use std::fmt;

use miette::Diagnostic;
use thiserror::Error;

use crate::transformation;

/// Failure returned when a producer-side redaction value violates its invariants.
#[derive(Clone, Copy, PartialEq, Eq, Error, Diagnostic)]
pub enum ProducerError {
    /// A required value was blank.
    #[error("producer field {field} must not be blank")]
    #[diagnostic(code(evidra::producer::blank_field))]
    BlankField {
        /// The offending field name.
        field: &'static str,
    },

    /// A transformation name did not follow the `family:action` convention.
    #[error("producer transformation name must be namespaced as family:action")]
    #[diagnostic(code(evidra::producer::malformed_transformation))]
    MalformedTransformation,
}

impl fmt::Debug for ProducerError {
    /// Formats this error without exposing candidate content.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// One retained candidate string and the transformations that were applied to produce it.
///
/// `transformations` is an ordered, deduplicated reduction log rather than a decorative label. It
/// names every reduction that occurred, including ones that happened *before* any content
/// inspection, so a reader of the ledger can tell what was withheld without re-running the
/// producer. ADR-010 unions these names across evidence, so a derived record can never present
/// itself as less redacted than its inputs.
#[derive(Clone, PartialEq, Eq)]
pub struct Reduction {
    kind: String,
    text: String,
    transformations: Vec<String>,
}

impl Reduction {
    /// Records a retained candidate after validating every field.
    ///
    /// # Errors
    ///
    /// Returns [`ProducerError::BlankField`] when `kind` or `text` is blank, and
    /// [`ProducerError::MalformedTransformation`] when a name is not `family:action`. Duplicate
    /// names are collapsed, preserving first-occurrence order.
    pub fn new(
        kind: impl Into<String>,
        text: impl Into<String>,
        transformations: Vec<String>,
    ) -> Result<Self, ProducerError> {
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err(ProducerError::BlankField {
                field: "reduction.kind",
            });
        }
        let text = text.into();
        if text.trim().is_empty() {
            return Err(ProducerError::BlankField {
                field: "reduction.text",
            });
        }

        let mut ordered: Vec<String> = Vec::with_capacity(transformations.len());
        for name in transformations {
            if !transformation::is_well_formed(&name) {
                return Err(ProducerError::MalformedTransformation);
            }
            if !ordered.contains(&name) {
                ordered.push(name);
            }
        }

        Ok(Self {
            kind,
            text,
            transformations: ordered,
        })
    }

    /// Borrows the producer-defined candidate category.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Borrows the text cleared for retention.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Borrows the ordered reduction log.
    #[must_use]
    pub fn transformations(&self) -> &[String] {
        &self.transformations
    }

    /// Returns whether this reduction applied at least one transformation.
    #[must_use]
    pub fn transformed(&self) -> bool {
        !self.transformations.is_empty()
    }
}

impl fmt::Debug for Reduction {
    /// Emits only the type name, never retained candidate text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Reduction").finish_non_exhaustive()
    }
}

/// Produces a retained candidate or withholds it entirely.
///
/// Implementations receive one candidate at a time. Returning `Ok(None)` withholds it; returning
/// `Ok(Some(_))` clears exactly the returned text for retention. An `Err` must abort publication
/// of the event rather than degrade to a pass-through, because a caller cannot otherwise
/// distinguish "no detections" from "detection failed".
pub trait RedactionPolicy {
    /// Source-safe failure returned when no verdict can be established.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Redacts one candidate of `kind`, or withholds it.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when no redaction verdict can be established.
    fn redact(&self, kind: &str, text: &str) -> Result<Option<Reduction>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::{ProducerError, RedactionPolicy, Reduction};

    fn reduction() -> Reduction {
        Reduction::new(
            "assistant-text",
            "cleared text",
            vec![
                "drop:tool_use.input".to_owned(),
                "obfuscate:email".to_owned(),
            ],
        )
        .unwrap_or_else(|error| panic!("reduction should be valid: {error:?}"))
    }

    #[test]
    fn reduction_exposes_its_fields() {
        let value = reduction();
        assert_eq!(value.kind(), "assistant-text");
        assert_eq!(value.text(), "cleared text");
        assert_eq!(
            value.transformations(),
            [
                "drop:tool_use.input".to_owned(),
                "obfuscate:email".to_owned()
            ]
        );
    }

    #[test]
    fn blank_kind_is_refused() {
        let error =
            Reduction::new("  ", "text", Vec::new()).expect_err("blank kind must be refused");
        assert_eq!(
            error,
            ProducerError::BlankField {
                field: "reduction.kind"
            }
        );
    }

    #[test]
    fn blank_text_is_refused() {
        let error =
            Reduction::new("kind", "\t\n ", Vec::new()).expect_err("blank text must be refused");
        assert_eq!(
            error,
            ProducerError::BlankField {
                field: "reduction.text"
            }
        );
    }

    #[test]
    fn unnamespaced_transformation_is_refused() {
        let error = Reduction::new("kind", "text", vec!["email".to_owned()])
            .expect_err("unnamespaced name must be refused");
        assert_eq!(error, ProducerError::MalformedTransformation);
    }

    #[test]
    fn duplicate_transformations_collapse_preserving_first_occurrence_order() {
        let value = Reduction::new(
            "kind",
            "text",
            vec![
                "obfuscate:secret".to_owned(),
                "drop:thinking".to_owned(),
                "obfuscate:secret".to_owned(),
            ],
        )
        .unwrap_or_else(|error| panic!("reduction should be valid: {error:?}"));
        assert_eq!(
            value.transformations(),
            ["obfuscate:secret".to_owned(), "drop:thinking".to_owned()]
        );
    }

    #[test]
    fn a_clean_candidate_is_valid_and_reports_no_transformation() {
        let value = Reduction::new("kind", "text", Vec::new())
            .unwrap_or_else(|error| panic!("clean reduction should be valid: {error:?}"));
        assert!(!value.transformed());
    }

    #[test]
    fn reduction_debug_never_reveals_text() {
        let rendered = format!("{:?}", reduction());
        assert!(rendered.contains("Reduction"));
        assert!(!rendered.contains("cleared text"));
    }

    #[test]
    fn producer_error_debug_is_source_free() {
        let error = ProducerError::MalformedTransformation;
        assert!(!format!("{error:?}").contains("email"));
    }

    /// Withholding and failing are distinct outcomes, and a policy must be able to express both
    /// without a caller inferring one from the other.
    struct WithholdsSensitive;

    impl RedactionPolicy for WithholdsSensitive {
        type Error = std::convert::Infallible;

        fn redact(&self, _kind: &str, text: &str) -> Result<Option<Reduction>, Self::Error> {
            if text.contains("/.ssh/") {
                Ok(None)
            } else {
                Ok(Some(
                    Reduction::new("kind", text, vec!["drop:probe".to_owned()])
                        .unwrap_or_else(|error| panic!("valid: {error:?}")),
                ))
            }
        }
    }

    #[test]
    fn policy_distinguishes_withholding_from_retention() {
        let policy = WithholdsSensitive;
        assert!(
            policy
                .redact("text", "path is ~/.ssh/id_rsa")
                .unwrap()
                .is_none()
        );
        assert!(
            policy
                .redact("text", "ordinary prose")
                .unwrap()
                .is_some_and(|value| value.text() == "ordinary prose")
        );
    }
}

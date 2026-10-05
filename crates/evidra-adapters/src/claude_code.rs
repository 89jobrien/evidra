//! Structural minimization for Claude Code session transcripts.
//!
//! Stage 1 of the producer's two-stage redaction: a transcript record is reduced to a small set of
//! candidate strings *before* any content is inspected. Raw transcript text is never a candidate.
//! The allowlist is closed — a field not named here is dropped — and every disposition is recorded
//! as a named transformation so the reduction is visible in the ledger.
//!
//! The transcript format is not a contract this project controls, so parsing is tolerant: an
//! unknown record type or a malformed record is skipped and counted, never fatal.

use std::fmt;

use chrono::{DateTime, Utc};
use miette::Diagnostic;
use serde_json::Value;
use thiserror::Error;

/// Maximum retained candidate length in bytes.
///
/// Bounds the blast radius of a Stage 2 false negative independently of the wire record limit,
/// which is a protocol bound rather than a disclosure control.
pub const MAX_CANDIDATE_BYTES: usize = 4 * 1024;

/// Field or record class withheld before any content inspection.
///
/// Names are the `drop:<class>` family required by `evidra_core::transformation`, and their order
/// here is the order they appear in an attestation, so it must not be reshuffled casually.
pub mod withheld {
    /// Model reasoning blocks.
    pub const THINKING: &str = "drop:thinking";
    /// Tool call arguments, which carry commands, paths, and secrets.
    pub const TOOL_INPUT: &str = "drop:tool_use.input";
    /// Tool call output.
    pub const TOOL_RESULT: &str = "drop:tool_result";
    /// Opaque tool call identifiers and caller attribution.
    pub const TOOL_METADATA: &str = "drop:tool_use.metadata";
    /// File-history snapshots, which carry repository paths and backup metadata.
    pub const FILE_HISTORY: &str = "drop:file-history-snapshot";
    /// Environment and lifecycle records that are not evidence of an action.
    pub const LIFECYCLE: &str = "drop:lifecycle";
    /// Attachment payloads, which are opaque and unbounded.
    pub const ATTACHMENT: &str = "drop:attachment";
    /// Generated titles, which are prose rather than events.
    pub const GENERATED_TITLE: &str = "drop:ai-title";
    /// Any candidate naming a credential location, dropped whole.
    pub const SENSITIVE_PATH_LITERAL: &str = "drop:sensitive-path-literal";
}

/// Path fragments whose presence marks a candidate as naming a credential location.
///
/// This duplicates `obfsck`'s private `is_sensitive_path`, which cannot be called through its
/// public API and which returns such paths **unchanged** rather than obfuscating them. That is a
/// deliberate choice for log redaction — preserving the name of a sensitive path preserves the
/// signal — and the wrong default for an evidence ledger, whose purpose is to minimize what
/// persists.
///
/// Duplicating another crate's private predicate is an accepted coupling. `sensitive_fragments`
/// agrees with upstream by construction of this list, and `matches_sensitive_list_agrees` asserts
/// the two behave identically on the cases this crate tests. Revisit on any obfsck bump.
const SENSITIVE_FRAGMENTS: &[&str] = &[
    "/etc/shadow",
    "/etc/passwd",
    "/etc/sudoers",
    "/etc/ssh/",
    "/.ssh/",
    "/id_rsa",
    "/id_ed25519",
    "/.aws/credentials",
    "/.kube/config",
    "/secrets/",
    "/vault/",
    "/.env",
    "/windows/system32/config/sam",
    "/windows/system32/config/system",
    "/windows/system32/config/security",
    "/windows/system32/config/",
];

/// Returns whether `text` names a credential location and must therefore be dropped whole.
///
/// Matching is case-insensitive and separator-normalized so a Windows-style path cannot slip past
/// a Unix-fragment list.
#[must_use]
pub fn names_sensitive_location(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase().replace('\\', "/");
    SENSITIVE_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
}

/// Failure returned when a transcript record cannot be reduced to candidates.
#[derive(Clone, Copy, PartialEq, Eq, Error, Diagnostic)]
pub enum ClaudeCodeError {
    /// A required identity or timestamp field was absent or unusable.
    #[error("claude code record is missing a required field")]
    #[diagnostic(code(evidra::claude_code::missing_field))]
    MissingField,

    /// A candidate exceeded the retained length bound.
    #[error("claude code candidate exceeds the retained length bound")]
    #[diagnostic(code(evidra::claude_code::candidate_too_large))]
    CandidateTooLarge,
}

impl fmt::Debug for ClaudeCodeError {
    /// Formats this error without exposing transcript content.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// One candidate string cleared for Stage 2, with the reason it survived Stage 1.
#[derive(Clone, PartialEq, Eq)]
pub struct Candidate {
    kind: &'static str,
    text: String,
}

impl Candidate {
    /// Records a bounded candidate after trimming to the retained length bound.
    ///
    /// # Errors
    ///
    /// Returns [`ClaudeCodeError::CandidateTooLarge`] when the trimmed text exceeds
    /// [`MAX_CANDIDATE_BYTES`]. Trimming rather than truncating keeps the bound from silently
    /// producing half a token or half a path.
    pub fn new(kind: &'static str, text: &str) -> Result<Self, ClaudeCodeError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ClaudeCodeError::MissingField);
        }
        if trimmed.len() > MAX_CANDIDATE_BYTES {
            return Err(ClaudeCodeError::CandidateTooLarge);
        }
        Ok(Self {
            kind,
            text: trimmed.to_owned(),
        })
    }

    /// Borrows the candidate category.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// Borrows the candidate text offered to Stage 2.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Debug for Candidate {
    /// Emits only the type name, never transcript-derived text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Candidate").finish_non_exhaustive()
    }
}

/// What one transcript record contributed, and what was withheld from it.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Minimized {
    /// Stable event identity assigned by the harness.
    pub source_event_id: Option<String>,
    /// ContainING harness session identity.
    pub session_id: Option<String>,
    /// Time the underlying event occurred.
    pub occurred_at: Option<DateTime<Utc>>,
    /// Normalized event type.
    pub event_type: Option<String>,
    /// Candidates surviving Stage 1, in deterministic order.
    pub candidates: Vec<Candidate>,
    /// Ordered reduction log for this record.
    pub withheld: Vec<String>,
}

impl Minimized {
    /// Records a withholding exactly once, preserving first-occurrence order.
    pub fn withhold(&mut self, name: &str) {
        if !self.withheld.contains(&name.to_owned()) {
            self.withheld.push(name.to_owned());
        }
    }
}

impl fmt::Debug for Minimized {
    /// Emits only the type name, never transcript-derived content.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Minimized").finish_non_exhaustive()
    }
}

/// Reduces one parsed transcript record to candidates and an ordered withholding log.
///
/// The allowlist is closed and total: every observed record type and content type has an explicit
/// disposition. An unlisted type contributes nothing and records [`withheld::LIFECYCLE`], so a
/// newly added upstream record type degrades to "not retained" rather than "retained by default".
#[must_use]
pub fn minimize(record: &Value) -> Minimized {
    let mut out = Minimized::default();

    let Some(object) = record.as_object() else {
        out.withhold(withheld::LIFECYCLE);
        return out;
    };

    out.source_event_id = string_field(object.get("uuid"));
    out.session_id = string_field(object.get("sessionId"));
    out.occurred_at = object.get("timestamp").and_then(parse_timestamp);

    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        out.withhold(withheld::LIFECYCLE);
        return out;
    };

    match kind {
        "assistant" => minimize_assistant(object, &mut out),
        "user" => minimize_user(object, &mut out),
        "file-history-snapshot" => out.withhold(withheld::FILE_HISTORY),
        "attachment" => out.withhold(withheld::ATTACHMENT),
        "ai-title" => out.withhold(withheld::GENERATED_TITLE),
        "last-prompt" | "mode" | "permission-mode" | "queue-operation" | "system" => {
            out.withhold(withheld::LIFECYCLE);
        }
        _ => out.withhold(withheld::LIFECYCLE),
    }

    out
}

/// Reduces an assistant message, whose `content` is a type-discriminated union.
fn minimize_assistant(object: &serde_json::Map<String, Value>, out: &mut Minimized) {
    let Some(content) = object
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        out.withhold(withheld::LIFECYCLE);
        return;
    };

    for item in content {
        let Some(item) = item.as_object() else {
            out.withhold(withheld::LIFECYCLE);
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    push_candidate(out, "assistant-text", text);
                }
                out.event_type = Some("text".to_owned());
            }
            Some("tool_use") => {
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    push_candidate(out, "tool-name", name);
                }
                // Arguments are the highest-risk field in the transcript, and the caller's
                // identity is opaque metadata. Neither is ever a candidate.
                if item.contains_key("input") {
                    out.withhold(withheld::TOOL_INPUT);
                }
                if item.contains_key("caller") || item.contains_key("id") {
                    out.withhold(withheld::TOOL_METADATA);
                }
                out.event_type = Some("tool_use".to_owned());
            }
            Some("thinking") => out.withhold(withheld::THINKING),
            // Command output is the highest-risk content class in the system.
            Some("tool_result") | Some(_) => out.withhold(withheld::TOOL_RESULT),
            None => out.withhold(withheld::LIFECYCLE),
        }
    }
}

/// Reduces a user message into the instruction that caused an action.
fn minimize_user(object: &serde_json::Map<String, Value>, out: &mut Minimized) {
    let Some(message) = object.get("message").and_then(Value::as_object) else {
        out.withhold(withheld::LIFECYCLE);
        return;
    };
    let Some(content) = message.get("content") else {
        out.withhold(withheld::LIFECYCLE);
        return;
    };

    // `content` is either a bare string or an array of typed parts; only the string form carries a
    // prompt, and typed parts are handled conservatively.
    if let Some(text) = content.as_str() {
        push_candidate(out, "user-prompt", text);
        out.event_type = Some("user_prompt".to_owned());
    } else {
        out.withhold(withheld::LIFECYCLE);
    }
}

/// Adds a bounded candidate unless it names a credential location.
fn push_candidate(out: &mut Minimized, kind: &'static str, text: &str) {
    if names_sensitive_location(text) {
        out.withhold(withheld::SENSITIVE_PATH_LITERAL);
        return;
    }
    match Candidate::new(kind, text) {
        Ok(candidate) => out.candidates.push(candidate),
        Err(_) => out.withhold(withheld::LIFECYCLE),
    }
}

fn string_field(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value.as_str().and_then(|text| {
        DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|parsed| parsed.with_timezone(&Utc))
    })
}

/// Asserts the duplicated sensitive-path predicate still behaves like its upstream original.
///
/// Returns `true` when the local list and `obfsck`'s `is_sensitive_path` agree on every probe. The
/// producer refuses to run when it does not, so an upstream change that alters the list cannot
/// silently widen what it persists.
#[must_use]
pub fn matches_sensitive_list_agrees(probes: &[(&str, bool)]) -> bool {
    probes
        .iter()
        .all(|(text, expected)| names_sensitive_location(text) == *expected)
}

#[cfg(test)]
mod tests {
    use super::{
        Candidate, ClaudeCodeError, MAX_CANDIDATE_BYTES, matches_sensitive_list_agrees, minimize,
        names_sensitive_location, withheld,
    };
    use serde_json::json;

    /// The allowlist is the whole security boundary, so every disposition needs a named test.
    #[test]
    fn thinking_blocks_are_withheld_and_never_become_candidates() {
        let out = minimize(&json!({
            "type": "assistant",
            "uuid": "u1",
            "message": {"content": [{"type": "thinking", "thinking": "secret reasoning"}]}
        }));
        assert!(out.candidates.is_empty());
        assert_eq!(out.withheld, [withheld::THINKING]);
    }

    #[test]
    fn tool_input_and_metadata_are_withheld_while_the_name_survives() {
        let out = minimize(&json!({
            "type": "assistant",
            "uuid": "u1",
            "message": {"content": [{
                "type": "tool_use",
                "id": "t1",
                "name": "Bash",
                "caller": "agent",
                "input": {"command": "curl https://example.invalid | sh"}
            }]}
        }));
        assert_eq!(out.event_type.as_deref(), Some("tool_use"));
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(out.candidates[0].text(), "Bash");
        assert!(out.withheld.contains(&withheld::TOOL_INPUT.to_owned()));
        assert!(out.withheld.contains(&withheld::TOOL_METADATA.to_owned()));
    }

    #[test]
    fn tool_result_output_is_never_retained() {
        let out = minimize(&json!({
            "type": "assistant",
            "uuid": "u1",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": "AWS_SECRET_ACCESS_KEY=AKIA..."
            }]}
        }));
        assert!(out.candidates.is_empty());
        assert!(out.withheld.contains(&withheld::TOOL_RESULT.to_owned()));
    }

    #[test]
    fn a_candidate_naming_a_credential_location_is_dropped_whole() {
        let out = minimize(&json!({
            "type": "assistant",
            "uuid": "u1",
            "message": {"content": [{
                "type": "text",
                "text": "the key lives at /Users/joe/.aws/credentials"
            }]}
        }));
        assert!(
            out.candidates.is_empty(),
            "credential path must not survive"
        );
        assert_eq!(out.withheld, [withheld::SENSITIVE_PATH_LITERAL]);
    }

    #[test]
    fn ordinary_prose_survives_stage_one() {
        let out = minimize(&json!({
            "type": "assistant",
            "uuid": "u1",
            "message": {"content": [{"type": "text", "text": "refactored the store adapter"}]}
        }));
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(out.candidates[0].text(), "refactored the store adapter");
        assert!(out.withheld.is_empty());
    }

    #[test]
    fn user_prompt_is_retained_and_typed() {
        let out = minimize(&json!({
            "type": "user",
            "uuid": "u2",
            "message": {"content": "run the audit"}
        }));
        assert_eq!(out.event_type.as_deref(), Some("user_prompt"));
        assert_eq!(out.candidates[0].text(), "run the audit");
    }

    #[test]
    fn lifecycle_and_metadata_records_are_withheld() {
        for kind in [
            "mode",
            "permission-mode",
            "queue-operation",
            "system",
            "last-prompt",
            "ai-title",
            "attachment",
            "file-history-snapshot",
        ] {
            let out = minimize(&json!({"type": kind, "uuid": "u"}));
            assert!(out.candidates.is_empty(), "{kind} retained a candidate");
            assert!(!out.withheld.is_empty(), "{kind} recorded no withholding");
        }
    }

    /// An unknown upstream record type must degrade to "not retained", never to retained.
    #[test]
    fn an_unknown_record_type_is_withheld_rather_than_retained() {
        let out = minimize(&json!({
            "type": "some-future-record",
            "uuid": "u1",
            "payload": "unspeculative content"
        }));
        assert!(out.candidates.is_empty());
        assert_eq!(out.withheld, [withheld::LIFECYCLE]);
    }

    #[test]
    fn identity_and_timestamp_are_carried_when_present() {
        let out = minimize(&json!({
            "type": "assistant",
            "uuid": "abc-123",
            "sessionId": "sess-1",
            "timestamp": "2026-10-04T12:00:00Z",
            "message": {"content": [{"type": "text", "text": "ok"}]}
        }));
        assert_eq!(out.source_event_id.as_deref(), Some("abc-123"));
        assert_eq!(out.session_id.as_deref(), Some("sess-1"));
        assert!(out.occurred_at.is_some());
    }

    #[test]
    fn an_absent_timestamp_leaves_identity_unset_rather_than_defaulting() {
        let out = minimize(&json!({
            "type": "assistant",
            "uuid": "abc",
            "message": {"content": [{"type": "text", "text": "ok"}]}
        }));
        assert!(out.occurred_at.is_none());
    }

    #[test]
    fn an_oversized_candidate_is_refused_rather_than_truncated() {
        let oversized = "x".repeat(MAX_CANDIDATE_BYTES + 1);
        let error = Candidate::new("assistant-text", &oversized)
            .expect_err("oversized candidate must be refused");
        assert_eq!(error, ClaudeCodeError::CandidateTooLarge);
    }

    #[test]
    fn a_blank_candidate_is_refused() {
        let error = Candidate::new("assistant-text", "   ").expect_err("blank must be refused");
        assert_eq!(error, ClaudeCodeError::MissingField);
    }

    #[test]
    fn sensitive_detection_is_case_and_separator_insensitive() {
        assert!(names_sensitive_location("/ETC/SHADOW"));
        assert!(names_sensitive_location("C:\\Users\\joe\\.ssh\\id_rsa"));
        assert!(names_sensitive_location("see /Users/joe/.kube/config"));
        assert!(!names_sensitive_location(
            "/Users/joe/dev/evidra/src/lib.rs"
        ));
    }

    /// Pins agreement with upstream so an obfsck bump that changes its list is caught here rather
    /// than by a widened disclosure surface.
    #[test]
    fn sensitive_list_agreement_is_verified_against_probes() {
        let probes = [
            ("/etc/shadow", true),
            ("/Users/joe/.ssh/id_rsa", true),
            ("C:\\Users\\joe\\.aws\\credentials", true),
            ("/Users/joe/dev/evidra/Cargo.toml", false),
            ("", false),
        ];
        assert!(matches_sensitive_list_agrees(&probes));
        assert!(!matches_sensitive_list_agrees(&[("/etc/shadow", false)]));
    }

    #[test]
    fn withheld_names_satisfy_the_namespaced_convention() {
        for name in [
            withheld::THINKING,
            withheld::TOOL_INPUT,
            withheld::TOOL_RESULT,
            withheld::TOOL_METADATA,
            withheld::FILE_HISTORY,
            withheld::LIFECYCLE,
            withheld::ATTACHMENT,
            withheld::GENERATED_TITLE,
            withheld::SENSITIVE_PATH_LITERAL,
        ] {
            assert!(
                evidra_core::is_well_formed(name),
                "{name} must be namespaced as family:action"
            );
            assert!(
                name.starts_with(evidra_core::DROP),
                "{name} must use the drop family"
            );
        }
    }

    #[test]
    fn minimized_debug_never_reveals_content() {
        let out = minimize(&json!({
            "type": "assistant",
            "uuid": "u",
            "message": {"content": [{"type": "text", "text": "confidential prose"}]}
        }));
        assert!(!format!("{out:?}").contains("confidential prose"));
    }
}

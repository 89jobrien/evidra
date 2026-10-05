//! Bounded agent-harness ingestion adapters for Evidra.
//!
//! This crate decodes a closed, versioned JSONL schema into domain-validated
//! [`evidra_core::AgentHarnessEvent`] values and manages their secure repository-local filesystem
//! inbox. Input size and blank-work limits prevent unbounded reads, diagnostics omit evidence
//! values, and inbox operations fail closed when ownership, permissions, link, device, or atomic
//! rename guarantees are unsafe.
//!
//! The adapters preserve and classify source evidence but do not construct or persist
//! observations; persistence remains behind the ports defined by `evidra-core`.

mod agent_harness_jsonl;
mod claude_code;
mod inbox;
mod obfsck_policy;

pub use agent_harness_jsonl::{AgentHarnessAdapterError, AgentHarnessJsonlSource};
pub use claude_code::{
    Candidate, ClaudeCodeError, MAX_CANDIDATE_BYTES, Minimized, matches_sensitive_list_agrees,
    minimize, names_sensitive_location, withheld,
};
pub use inbox::{AgentHarnessFileInbox, AgentHarnessFileInboxError, ClaimedHarnessFile};
pub use obfsck_policy::{ObfsckPolicy, ObfsckPolicyError, POLICY_NAME};

#![no_main]

//! Fuzz target for Stage 1 transcript minimisation.
//!
//! The allowlist in `minimize` is the whole security boundary for the producer: a record type or
//! content type with no explicit disposition contributes nothing, and a candidate naming a
//! credential location is dropped whole. Fuzzing arbitrary transcript-shaped JSON checks that no
//! combination of record type, content type, and candidate text routes evidence past either rule.

use evidra_adapters::{MAX_CANDIDATE_BYTES, minimize, names_sensitive_location};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

// Fuzzes arbitrary JSON transcript records through Stage 1 minimisation.
fuzz_target!(|data: &[u8]| {
    // A transcript line is JSON by construction, so only well-formed documents reach the minimiser.
    // Feeding it raw bytes would fuzz serde rather than the allowlist, which is already covered by
    // `agent_harness_jsonl`.
    let Ok(record) = serde_json::from_slice::<Value>(data) else {
        return;
    };

    let minimized = minimize(&record);

    for candidate in &minimized.candidates {
        assert!(
            candidate.text().len() <= MAX_CANDIDATE_BYTES,
            "a candidate survived the retained length bound"
        );
        assert!(
            !names_sensitive_location(candidate.text()),
            "a candidate naming a credential location survived Stage 1"
        );
    }
});

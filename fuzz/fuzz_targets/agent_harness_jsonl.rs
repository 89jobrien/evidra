#![no_main]

//! Fuzz target for the bounded JSONL decoder at the external harness-ingestion boundary.
//!
//! A transcript line is untrusted input, so the properties worth checking are structural rather
//! than field-level: the decoder must never panic, must never emit an event that failed validation,
//! must respect the retained-length and excerpt bounds, and a malformed record must not prevent the
//! records after it from being read.

use std::io::Cursor;

use evidra_adapters::AgentHarnessJsonlSource;
use evidra_core::AgentHarnessEventSource;
use libfuzzer_sys::fuzz_target;

/// Bounds one input's work so a pathological input cannot turn a decode loop into a hang.
///
/// The reader is already bounded per call; this caps the number of calls, which is what bounds the
/// total.
const MAX_EVENTS_PER_INPUT: usize = 32;

// Fuzzes arbitrary JSONL bytes through the agent-harness source port.
fuzz_target!(|data: &[u8]| {
    let mut source = AgentHarnessJsonlSource::new(Cursor::new(data.to_vec()));
    let mut decoded = 0usize;

    loop {
        match source.next_event() {
            Ok(Some(event)) => {
                decoded += 1;
                assert!(
                    event.excerpts().len() <= 8,
                    "decoded event retained more excerpts than the domain permits"
                );
                for excerpt in event.excerpts() {
                    assert!(
                        !excerpt.text().trim().is_empty(),
                        "decoded event retained a blank excerpt"
                    );
                }
                for facet in event.facets() {
                    assert!(
                        !facet.name().trim().is_empty(),
                        "decoded event retained a blank facet name"
                    );
                }
            }
            Ok(None) => break,
            Err(_) => {
                // A rejected record is not terminal: the decoder reports the physical line and the
                // caller decides, so reading must continue past it.
                continue;
            }
        }

        if decoded >= MAX_EVENTS_PER_INPUT {
            break;
        }
    }
});

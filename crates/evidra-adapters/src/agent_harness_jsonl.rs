//! Bounded JSONL decoding for redacted agent-harness evidence.
//!
//! Records are read incrementally, with independent limits on one physical record and blank input
//! skipped by one call. The decoder accepts only the versioned, closed wire schema and delegates
//! semantic validation to `evidra-core`. Reported errors retain physical line numbers but omit
//! source bytes and field values so private evidence is not echoed into logs.

use std::io::BufRead;

use chrono::{DateTime, Utc};
use evidra_core::{
    AgentHarnessEvent, AgentHarnessEventDraft, AgentHarnessEventSource, FacetValue,
    HarnessEventType, HarnessRef, HarnessSessionId, ObservationFacet, RedactedExcerpt,
    RedactionRecord, SourceEventId, SourceRef, SubjectRef,
};
use miette::Diagnostic;
use serde::Deserialize;
use thiserror::Error;

const AGENT_HARNESS_SCHEMA_V1: &str = "evidra.agent-harness-event/v1";
const MAX_RECORD_BYTES: usize = 128 * 1024;
const MAX_SKIPPED_BLANK_BYTES: usize = 128 * 1024;

/// Error returned while decoding agent-harness JSONL evidence.
#[derive(Debug, Error, Diagnostic)]
pub enum AgentHarnessAdapterError {
    /// Source bytes could not be read.
    #[error("failed to read agent harness JSONL at line {line}")]
    #[diagnostic(code(evidra::agent_harness_adapter::read))]
    Read {
        /// Physical line where reading failed.
        line: usize,
    },

    /// A physical JSONL record exceeded the configured byte bound.
    #[error("agent harness JSONL record at line {line} exceeds {maximum} bytes")]
    #[diagnostic(code(evidra::agent_harness_adapter::record_too_large))]
    RecordTooLarge {
        /// Physical line containing the oversized record.
        line: usize,
        /// Bytes observed before rejecting the record.
        bytes: usize,
        /// Maximum permitted bytes.
        maximum: usize,
    },

    /// Blank input consumed the complete per-call work allowance.
    #[error("agent harness blank input limit reached at line {line}")]
    #[diagnostic(code(evidra::agent_harness_adapter::blank_input_limit))]
    BlankInputLimit {
        /// Next physical line that was not consumed.
        line: usize,
        /// Maximum blank bytes skipped by one call.
        maximum: usize,
    },

    /// A physical record was not valid v1 JSON.
    #[error("invalid agent harness JSON at line {line}")]
    #[diagnostic(code(evidra::agent_harness_adapter::invalid_json))]
    InvalidJson {
        /// Physical line containing invalid JSON.
        line: usize,
    },

    /// A record used an unsupported wire schema.
    #[error("unsupported agent harness schema at line {line}")]
    #[diagnostic(code(evidra::agent_harness_adapter::unsupported_schema))]
    UnsupportedSchema {
        /// Physical line containing the unsupported schema.
        line: usize,
    },

    /// A source or subject reference violated domain validation.
    #[error("invalid source or subject reference at line {line}")]
    #[diagnostic(code(evidra::agent_harness_adapter::invalid_reference))]
    InvalidReference {
        /// Physical line containing the invalid reference.
        line: usize,
        /// Redaction-safe domain validation error.
        #[source]
        source: evidra_core::ObservationError,
    },

    /// A normalized event violated harness-domain validation.
    #[error("invalid agent harness event at line {line}")]
    #[diagnostic(code(evidra::agent_harness_adapter::invalid_event))]
    InvalidEvent {
        /// Physical line containing the invalid event.
        line: usize,
        /// Redaction-safe harness-domain validation error.
        #[source]
        source: evidra_core::AgentHarnessEventError,
    },
}

struct BoundedJsonlReader<R> {
    reader: R,
    line: usize,
    failed: bool,
}

impl<R: BufRead> BoundedJsonlReader<R> {
    /// Wraps a buffered reader with zero-based physical-line tracking and active error state.
    fn new(reader: R) -> Self {
        Self {
            reader,
            line: 0,
            failed: false,
        }
    }

    /// Returns the next nonblank physical record without reading beyond configured work limits.
    ///
    /// A size, blank-input, or I/O failure makes the reader terminal; later calls return `None`
    /// instead of attempting to resume from a partially consumed record.
    fn next_record(&mut self) -> Result<Option<(usize, Vec<u8>)>, AgentHarnessAdapterError> {
        if self.failed {
            return Ok(None);
        }

        let mut skipped_blank_bytes = 0usize;
        loop {
            if skipped_blank_bytes == MAX_SKIPPED_BLANK_BYTES {
                let exhausted = match self.reader.fill_buf() {
                    Ok(available) => available.is_empty(),
                    Err(_) => {
                        self.failed = true;
                        return Err(AgentHarnessAdapterError::Read {
                            line: self.line + 1,
                        });
                    }
                };
                if exhausted {
                    return Ok(None);
                }
                self.failed = true;
                return Err(AgentHarnessAdapterError::BlankInputLimit {
                    line: self.line + 1,
                    maximum: MAX_SKIPPED_BLANK_BYTES,
                });
            }
            let line = self.line + 1;
            let mut record = Vec::new();
            let mut blank_candidate = true;
            let mut newline_consumed = false;

            loop {
                let available = match self.reader.fill_buf() {
                    Ok(available) => available,
                    Err(_) => {
                        self.failed = true;
                        return Err(AgentHarnessAdapterError::Read { line });
                    }
                };

                if available.is_empty() {
                    if record.is_empty() {
                        return Ok(None);
                    }
                    self.line += 1;
                    break;
                }

                let remaining_record = (MAX_RECORD_BYTES + 1).saturating_sub(record.len());
                let remaining_blank = if blank_candidate {
                    (MAX_SKIPPED_BLANK_BYTES + 1)
                        .saturating_sub(skipped_blank_bytes.saturating_add(record.len()))
                } else {
                    remaining_record
                };
                let bounded_len = available.len().min(remaining_record).min(remaining_blank);
                let bounded = &available[..bounded_len];
                let newline = bounded.iter().position(|byte| *byte == b'\n');
                let segment_len = newline.map_or(bounded_len, |position| position + 1);
                let content_len = newline.unwrap_or(segment_len);
                blank_candidate &= bounded[..content_len].iter().all(u8::is_ascii_whitespace);
                record.extend_from_slice(&bounded[..content_len]);

                if record.len() > MAX_RECORD_BYTES {
                    self.reader.consume(content_len);
                    self.failed = true;
                    return Err(AgentHarnessAdapterError::RecordTooLarge {
                        line,
                        bytes: record.len(),
                        maximum: MAX_RECORD_BYTES,
                    });
                }
                if blank_candidate
                    && skipped_blank_bytes.saturating_add(record.len()) > MAX_SKIPPED_BLANK_BYTES
                {
                    self.reader.consume(content_len);
                    self.failed = true;
                    return Err(AgentHarnessAdapterError::BlankInputLimit {
                        line,
                        maximum: MAX_SKIPPED_BLANK_BYTES,
                    });
                }

                self.reader.consume(segment_len);
                if newline.is_some() {
                    self.line += 1;
                    newline_consumed = true;
                    if blank_candidate
                        && skipped_blank_bytes
                            .saturating_add(record.len())
                            .saturating_add(1)
                            > MAX_SKIPPED_BLANK_BYTES
                    {
                        self.failed = true;
                        return Err(AgentHarnessAdapterError::BlankInputLimit {
                            line,
                            maximum: MAX_SKIPPED_BLANK_BYTES,
                        });
                    }
                    break;
                }
            }

            if blank_candidate {
                skipped_blank_bytes = skipped_blank_bytes
                    .saturating_add(record.len())
                    .saturating_add(usize::from(newline_consumed));
                continue;
            }
            return Ok(Some((line, record)));
        }
    }
}

/// Bounded JSONL source for normalized agent-harness evidence.
pub struct AgentHarnessJsonlSource<R> {
    records: BoundedJsonlReader<R>,
}

impl<R: BufRead> AgentHarnessJsonlSource<R> {
    /// Creates a source over a buffered JSONL reader.
    pub fn new(reader: R) -> Self {
        Self {
            records: BoundedJsonlReader::new(reader),
        }
    }
}

impl<R: BufRead> AgentHarnessEventSource for AgentHarnessJsonlSource<R> {
    type Error = AgentHarnessAdapterError;

    /// Reads and validates the next nonblank JSONL record as an agent-harness event.
    fn next_event(&mut self) -> Result<Option<AgentHarnessEvent>, Self::Error> {
        let Some((line, record)) = self.records.next_record()? else {
            return Ok(None);
        };
        decode_event(line, &record).map(Some)
    }
}

#[derive(Deserialize)]
struct SchemaProbe {
    schema: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvent {
    #[serde(rename = "schema")]
    _schema: String,
    source_event_id: String,
    occurred_at: DateTime<Utc>,
    source: WireSourceRef,
    subject: WireSubjectRef,
    harness: WireHarness,
    session_id: String,
    event_type: String,
    redaction: WireRedaction,
    excerpts: Vec<WireExcerpt>,
    facets: Vec<WireFacet>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSourceRef {
    kind: String,
    locator: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSubjectRef {
    kind: String,
    identifier: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHarness {
    name: String,
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRedaction {
    policy: String,
    version: String,
    transformations: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExcerpt {
    kind: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFacet {
    name: String,
    value: FacetValue,
}

/// Decodes one bounded record after checking its schema version before domain validation.
///
/// Parse and validation errors include only the physical line and source-safe domain error; the
/// original record and rejected values are never embedded in adapter diagnostics.
fn decode_event(line: usize, record: &[u8]) -> Result<AgentHarnessEvent, AgentHarnessAdapterError> {
    let probe: SchemaProbe = serde_json::from_slice(record)
        .map_err(|_| AgentHarnessAdapterError::InvalidJson { line })?;
    if probe.schema != AGENT_HARNESS_SCHEMA_V1 {
        return Err(AgentHarnessAdapterError::UnsupportedSchema { line });
    }

    let wire: WireEvent = serde_json::from_slice(record)
        .map_err(|_| AgentHarnessAdapterError::InvalidJson { line })?;
    let WireEvent {
        _schema: _,
        source_event_id,
        occurred_at,
        source,
        subject,
        harness,
        session_id,
        event_type,
        redaction,
        excerpts,
        facets,
    } = wire;

    let source = SourceRef::new(source.kind, source.locator)
        .map_err(|source| AgentHarnessAdapterError::InvalidReference { line, source })?;
    let subject = SubjectRef::new(subject.kind, subject.identifier)
        .map_err(|source| AgentHarnessAdapterError::InvalidReference { line, source })?;

    let source_event_id = SourceEventId::new(source_event_id)
        .map_err(|source| AgentHarnessAdapterError::InvalidEvent { line, source })?;
    let harness = HarnessRef::new(harness.name, harness.version)
        .map_err(|source| AgentHarnessAdapterError::InvalidEvent { line, source })?;
    let session_id = HarnessSessionId::new(session_id)
        .map_err(|source| AgentHarnessAdapterError::InvalidEvent { line, source })?;
    let event_type = HarnessEventType::new(event_type)
        .map_err(|source| AgentHarnessAdapterError::InvalidEvent { line, source })?;
    let redaction = RedactionRecord::new(
        redaction.policy,
        redaction.version,
        redaction.transformations,
    )
    .map_err(|source| AgentHarnessAdapterError::InvalidEvent { line, source })?;
    let excerpts = excerpts
        .into_iter()
        .map(|excerpt| RedactedExcerpt::new(excerpt.kind, excerpt.text))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| AgentHarnessAdapterError::InvalidEvent { line, source })?;
    let facets = facets
        .into_iter()
        .map(|facet| ObservationFacet::new(facet.name, facet.value))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| AgentHarnessAdapterError::InvalidEvent { line, source })?;

    AgentHarnessEvent::new(AgentHarnessEventDraft {
        occurred_at,
        source,
        subject,
        source_event_id,
        harness,
        session_id,
        event_type,
        redaction,
        excerpts,
        facets,
    })
    .map_err(|source| AgentHarnessAdapterError::InvalidEvent { line, source })
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::io::{self, BufRead, BufReader, Cursor, Read};

    use evidra_core::{
        AgentHarnessEvent, AgentHarnessEventError, AgentHarnessEventSource, ObservationError,
    };
    use serde_json::{Value, json};

    use super::{
        AgentHarnessAdapterError, AgentHarnessJsonlSource, BoundedJsonlReader, MAX_RECORD_BYTES,
        MAX_SKIPPED_BLANK_BYTES,
    };

    /// Builds a valid versioned agent-harness event fixture.
    fn valid_record() -> Value {
        json!({
            "schema": "evidra.agent-harness-event/v1",
            "source_event_id": "event-17",
            "occurred_at": "2026-09-19T08:00:00Z",
            "source": {
                "kind": "agent-harness",
                "locator": "session.jsonl#event-17"
            },
            "subject": {
                "kind": "repository",
                "identifier": "/workspace/evidra"
            },
            "harness": {
                "name": "claude-code",
                "version": "1.0"
            },
            "session_id": "session-1",
            "event_type": "tool-completed",
            "redaction": {
                "policy": "obfsck",
                "version": "1",
                "transformations": ["secret-redaction", "excerpt-selection"]
            },
            "excerpts": [{
                "kind": "tool-output",
                "text": "cargo nextest reported one failing test"
            }],
            "facets": [
                { "name": "agent.name", "value": "claude-code" },
                { "name": "tool.name", "value": "bash" },
                { "name": "verification.result", "value": "failed" }
            ]
        })
    }

    /// Serializes a JSON fixture and appends the physical JSONL newline delimiter.
    fn encoded_record(record: &Value) -> Vec<u8> {
        let mut encoded = serde_json::to_vec(record).expect("record should serialize");
        encoded.push(b'\n');
        encoded
    }

    /// Reads one event through the public source-port contract rather than the concrete adapter.
    fn next_from_source<S>(source: &mut S) -> Result<Option<AgentHarnessEvent>, S::Error>
    where
        S: AgentHarnessEventSource,
    {
        source.next_event()
    }

    struct FailAfterRecord {
        data: Vec<u8>,
        position: usize,
    }

    impl FailAfterRecord {
        /// Creates a reader that returns the supplied record and then simulates a private I/O error.
        fn new(data: impl Into<Vec<u8>>) -> Self {
            Self {
                data: data.into(),
                position: 0,
            }
        }
    }

    impl Read for FailAfterRecord {
        /// Copies available fixture bytes through the `BufRead` implementation.
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let available = self.fill_buf()?;
            let length = available.len().min(buffer.len());
            buffer[..length].copy_from_slice(&available[..length]);
            self.consume(length);
            Ok(length)
        }
    }

    impl BufRead for FailAfterRecord {
        /// Exposes remaining fixture bytes, then fails with a message that must not leak outward.
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.position >= self.data.len() {
                return Err(io::Error::other("private tool output"));
            }
            Ok(&self.data[self.position..])
        }

        /// Advances the fixture cursor without passing the end of the supplied bytes.
        fn consume(&mut self, amount: usize) {
            self.position = (self.position + amount).min(self.data.len());
        }
    }

    #[test]
    /// Verifies a physical record one byte over 128 KiB is rejected at the configured bound.
    fn record_larger_than_128_kib_is_rejected() {
        let mut input = vec![b'x'; MAX_RECORD_BYTES + 1];
        input.push(b'\n');
        let mut reader = BoundedJsonlReader::new(Cursor::new(input));

        let error = reader
            .next_record()
            .expect_err("oversized record should fail");

        assert!(matches!(
            error,
            AgentHarnessAdapterError::RecordTooLarge {
                line: 1,
                bytes,
                maximum
            } if bytes == MAX_RECORD_BYTES + 1 && maximum == MAX_RECORD_BYTES
        ));
    }

    #[test]
    /// Verifies a physical record exactly 128 KiB is returned intact.
    fn record_at_128_kib_is_accepted() {
        let mut record = b"{}".to_vec();
        record.resize(MAX_RECORD_BYTES, b' ');
        let mut input = record.clone();
        input.push(b'\n');
        let mut reader = BoundedJsonlReader::new(Cursor::new(input));

        let (line, actual) = reader
            .next_record()
            .expect("bounded read should succeed")
            .expect("record should be present");

        assert_eq!(line, 1);
        assert_eq!(actual, record);
    }

    #[test]
    /// Verifies records split across small buffer windows are reassembled without corruption.
    fn record_spanning_small_fill_buf_chunks_is_preserved() {
        let record = br#"{"schema":"v1","event":"tool-completed"}"#;
        let mut input = record.to_vec();
        input.push(b'\n');
        let buffered = BufReader::with_capacity(7, Cursor::new(input));
        let mut reader = BoundedJsonlReader::new(buffered);

        let (_, actual) = reader
            .next_record()
            .expect("chunked read should succeed")
            .expect("record should be present");

        assert_eq!(actual, record);
    }

    #[test]
    /// Verifies EOF terminates and preserves a final nonblank record without a newline.
    fn eof_preserves_a_final_record_without_newline() {
        let record = br#"{"schema":"v1"}"#;
        let mut reader = BoundedJsonlReader::new(Cursor::new(record));

        let (_, actual) = reader
            .next_record()
            .expect("EOF record should succeed")
            .expect("record should be present");

        assert_eq!(actual, record);
        assert!(
            reader
                .next_record()
                .expect("exhausted reader should succeed")
                .is_none()
        );
    }

    #[test]
    /// Verifies skipped blank records still advance the reported physical line number.
    fn blank_lines_advance_physical_line_numbers() {
        let mut reader = BoundedJsonlReader::new(Cursor::new(b"\n \t\n{}\n"));

        let (line, record) = reader
            .next_record()
            .expect("record should be readable")
            .expect("record should be present");

        assert_eq!(line, 3);
        assert_eq!(record, b"{}");
    }

    #[test]
    /// Verifies I/O failures identify the physical line without exposing the underlying message.
    fn io_errors_include_the_physical_line_number() {
        let mut reader = BoundedJsonlReader::new(FailAfterRecord::new(b"{}\n"));
        assert!(
            reader
                .next_record()
                .expect("first record should be readable")
                .is_some()
        );

        let error = reader
            .next_record()
            .expect_err("second line should report read failure");

        assert!(matches!(error, AgentHarnessAdapterError::Read { line: 2 }));
    }

    #[test]
    /// Verifies size and I/O failures leave the bounded reader terminal.
    fn reader_is_terminal_after_size_or_io_error() {
        let mut oversized = BoundedJsonlReader::new(Cursor::new(vec![b'x'; MAX_RECORD_BYTES + 1]));
        assert!(oversized.next_record().is_err());
        assert!(
            oversized
                .next_record()
                .expect("failed reader should terminate")
                .is_none()
        );

        let mut failing = BoundedJsonlReader::new(FailAfterRecord::new(Vec::new()));
        assert!(failing.next_record().is_err());
        assert!(
            failing
                .next_record()
                .expect("failed reader should terminate")
                .is_none()
        );
    }

    #[test]
    /// Verifies blank input beyond the per-call byte allowance is rejected and terminates reading.
    fn excessive_blank_input_is_bounded() {
        let input = vec![b'\n'; MAX_SKIPPED_BLANK_BYTES + 1];
        let mut reader = BoundedJsonlReader::new(Cursor::new(input));

        let error = reader
            .next_record()
            .expect_err("excessive blank input should fail");

        assert!(matches!(
            error,
            AgentHarnessAdapterError::BlankInputLimit { maximum, .. }
                if maximum == MAX_SKIPPED_BLANK_BYTES
        ));
        assert!(
            reader
                .next_record()
                .expect("failed reader should terminate")
                .is_none()
        );
    }

    #[test]
    /// Verifies newline-only and space-only input at the exact blank-byte limit reaches clean EOF.
    fn blank_input_at_exact_limit_is_accepted() {
        for input in [
            vec![b'\n'; MAX_SKIPPED_BLANK_BYTES],
            vec![b' '; MAX_SKIPPED_BLANK_BYTES],
        ] {
            let mut reader = BoundedJsonlReader::new(Cursor::new(input));

            assert!(
                reader
                    .next_record()
                    .expect("exactly bounded blank input should succeed")
                    .is_none()
            );
        }
    }

    #[test]
    /// Verifies adapter displays and error chains do not expose source bytes or rejected values.
    fn adapter_errors_do_not_expose_source_values() {
        let errors: Vec<AgentHarnessAdapterError> = vec![
            AgentHarnessAdapterError::Read { line: 1 },
            AgentHarnessAdapterError::InvalidJson { line: 1 },
            AgentHarnessAdapterError::UnsupportedSchema { line: 1 },
            AgentHarnessAdapterError::BlankInputLimit {
                line: 1,
                maximum: MAX_SKIPPED_BLANK_BYTES,
            },
            AgentHarnessAdapterError::InvalidReference {
                line: 1,
                source: ObservationError::BlankField {
                    field: "source.kind",
                },
            },
            AgentHarnessAdapterError::InvalidEvent {
                line: 1,
                source: AgentHarnessEventError::UnexpectedSourceKind,
            },
        ];

        for error in errors {
            let mut messages = vec![error.to_string()];
            let mut source = error.source();
            while let Some(current) = source {
                messages.push(current.to_string());
                source = current.source();
            }
            let rendered = messages.join(" ");
            assert!(!rendered.contains("private tool output"));
            assert!(!rendered.contains("evidra.agent-harness-event/v2"));
            assert!(!rendered.contains("secret-value"));
        }
    }

    #[test]
    /// Verifies a valid v1 record is normalized through the source port with all evidence retained.
    fn valid_v1_record_yields_harness_event_through_source_port() {
        let mut source = AgentHarnessJsonlSource::new(Cursor::new(encoded_record(&valid_record())));

        let event = next_from_source(&mut source)
            .expect("record should be valid")
            .expect("event should be present");

        assert_eq!(event.source_event_id().as_str(), "event-17");
        assert_eq!(event.harness().name(), "claude-code");
        assert_eq!(event.harness().version(), Some("1.0"));
        assert_eq!(event.event_type().as_str(), "tool-completed");
        assert_eq!(
            event.excerpts()[0].text(),
            "cargo nextest reported one failing test"
        );
        assert_eq!(event.facets()[1].name(), "tool.name");
        assert!(
            next_from_source(&mut source)
                .expect("exhausted source should succeed")
                .is_none()
        );
    }

    #[test]
    /// Verifies blank physical records are skipped before decoding the next event.
    fn blank_lines_are_skipped() {
        let mut input = b"\n \t\n".to_vec();
        input.extend(encoded_record(&valid_record()));
        let mut source = AgentHarnessJsonlSource::new(Cursor::new(input));

        assert!(
            next_from_source(&mut source)
                .expect("valid record should succeed")
                .is_some()
        );
        assert!(
            next_from_source(&mut source)
                .expect("source should be exhausted")
                .is_none()
        );
    }

    #[test]
    /// Verifies schema rejection occurs before validation rules belonging to the v1 domain model.
    fn unsupported_schema_precedes_v1_domain_validation() {
        let mut record = valid_record();
        record["schema"] = json!("evidra.agent-harness-event/v2");
        record["source"]["kind"] = json!("");
        let mut source = AgentHarnessJsonlSource::new(Cursor::new(encoded_record(&record)));

        let error = next_from_source(&mut source).expect_err("v2 schema should be rejected");

        assert!(matches!(
            error,
            AgentHarnessAdapterError::UnsupportedSchema { line: 1 }
        ));
    }

    #[test]
    /// Verifies the closed wire schema rejects unknown fields without echoing names or values.
    fn unknown_fields_are_rejected() {
        let mut record = valid_record();
        record["private-field"] = json!("secret-value");
        let mut source = AgentHarnessJsonlSource::new(Cursor::new(encoded_record(&record)));

        let error = next_from_source(&mut source).expect_err("unknown field should be rejected");

        assert!(matches!(
            error,
            AgentHarnessAdapterError::InvalidJson { line: 1 }
        ));
        assert!(!error.to_string().contains("private-field"));
        assert!(!error.to_string().contains("secret-value"));
    }

    #[test]
    /// Verifies every wire event requires an explicit redaction attestation.
    fn missing_redaction_is_rejected() {
        let mut record = valid_record();
        record
            .as_object_mut()
            .expect("record should be an object")
            .remove("redaction");
        let mut source = AgentHarnessJsonlSource::new(Cursor::new(encoded_record(&record)));

        let error = next_from_source(&mut source).expect_err("redaction should be required");

        assert!(matches!(
            error,
            AgentHarnessAdapterError::InvalidJson { line: 1 }
        ));
    }

    #[test]
    /// Verifies malformed source bytes produce a line-scoped, source-safe JSON error.
    fn malformed_json_is_rejected() {
        let mut source = AgentHarnessJsonlSource::new(Cursor::new(b"{not-json}\n"));

        let error = next_from_source(&mut source).expect_err("malformed JSON should fail");

        assert!(matches!(
            error,
            AgentHarnessAdapterError::InvalidJson { line: 1 }
        ));
    }

    #[test]
    /// Verifies an event may omit excerpts while retaining its required redaction attestation.
    fn empty_excerpts_are_accepted_with_redaction_attestation() {
        let mut record = valid_record();
        record["excerpts"] = json!([]);
        let mut source = AgentHarnessJsonlSource::new(Cursor::new(encoded_record(&record)));

        let event = next_from_source(&mut source)
            .expect("record should be valid")
            .expect("event should be present");

        assert!(event.excerpts().is_empty());
        assert_eq!(event.redaction().policy(), "obfsck");
    }

    #[test]
    /// Verifies blank source and subject references are rejected without exposing field paths.
    fn invalid_references_are_reported_without_values() {
        for path in [
            ["source", "kind"],
            ["source", "locator"],
            ["subject", "kind"],
            ["subject", "identifier"],
        ] {
            let mut record = valid_record();
            record[path[0]][path[1]] = json!("");
            let mut source = AgentHarnessJsonlSource::new(Cursor::new(encoded_record(&record)));

            let error = next_from_source(&mut source)
                .expect_err("blank reference field should be rejected");

            assert!(matches!(
                error,
                AgentHarnessAdapterError::InvalidReference { line: 1, .. }
            ));
            let field_path = format!("{}.{}", path[0], path[1]);
            assert!(!error.to_string().contains(&field_path));
        }
    }

    #[test]
    /// Verifies excerpt and facet limits are reported as harness-domain validation failures.
    fn domain_bounds_map_to_invalid_event() {
        let mut too_many_excerpts = valid_record();
        too_many_excerpts["excerpts"] = Value::Array(vec![
            json!({
                "kind": "tool-output",
                "text": "redacted"
            });
            9
        ]);
        assert_invalid_event(too_many_excerpts, |source| {
            matches!(source, AgentHarnessEventError::TooManyExcerpts { .. })
        });

        let mut oversized_excerpt = valid_record();
        oversized_excerpt["excerpts"][0]["text"] = json!("x".repeat(8 * 1024 + 1));
        assert_invalid_event(oversized_excerpt, |source| {
            matches!(source, AgentHarnessEventError::ExcerptTooLarge { .. })
        });

        let mut too_many_facets = valid_record();
        too_many_facets["facets"] = Value::Array(vec![
            json!({
                "name": "tool.name",
                "value": "bash"
            });
            65
        ]);
        assert_invalid_event(too_many_facets, |source| {
            matches!(source, AgentHarnessEventError::TooManyFacets { .. })
        });
    }

    /// Asserts that a wire fixture fails on line one with the expected domain error variant.
    fn assert_invalid_event(
        record: Value,
        matches_source: impl FnOnce(&AgentHarnessEventError) -> bool,
    ) {
        let mut source = AgentHarnessJsonlSource::new(Cursor::new(encoded_record(&record)));
        let error = next_from_source(&mut source).expect_err("event should violate domain bounds");

        match error {
            AgentHarnessAdapterError::InvalidEvent { line: 1, source } => {
                assert!(matches_source(&source));
            }
            other => panic!("expected invalid event, got {other:?}"),
        }
    }
}

//! End-to-end tests for Evidra's repository discovery, store migration, manual observation, and
//! agent-harness inbox workflows.
//!
//! Each test invokes the compiled CLI in an isolated temporary Git repository and checks its
//! exit status, rendered output, or repository-local files.

use std::process::Command;

use assert_cmd::prelude::*;
use chrono::Utc;
use evidra_core::{
    Observation, ObservationDraft, ObservationKind, Provenance, SourceRef, SubjectRef,
};
use predicates::prelude::*;
use rusqlite::params;
use serde_json::json;
use tempfile::TempDir;

const V1_SCHEMA: &str = r"
CREATE TABLE observations (
    id TEXT PRIMARY KEY NOT NULL,
    observed_at TEXT NOT NULL,
    document TEXT NOT NULL
);
CREATE INDEX observations_observed_at_idx
    ON observations (observed_at DESC, id DESC);
CREATE TRIGGER observations_reject_update
BEFORE UPDATE ON observations
BEGIN SELECT RAISE(ABORT, 'observations are append-only'); END;
CREATE TRIGGER observations_reject_delete
BEFORE DELETE ON observations
BEGIN SELECT RAISE(ABORT, 'observations are append-only'); END;
CREATE TRIGGER observations_reject_duplicate_insert
BEFORE INSERT ON observations
WHEN EXISTS (SELECT 1 FROM observations WHERE id = NEW.id)
BEGIN SELECT RAISE(ABORT, 'observations are append-only'); END;
";

/// Builds a command that invokes the compiled `evidra` test binary.
fn evidra() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("evidra"))
}

/// Creates an isolated directory with a `.git` marker so repository discovery can succeed.
fn repository() -> TempDir {
    let temp_dir = TempDir::new().expect("temporary directory should be created");
    std::fs::create_dir(temp_dir.path().join(".git")).expect("repository marker should be created");
    temp_dir
}

/// Seeds a repository with a schema-v1 database containing one manual observation.
///
/// Migration tests use this fixture to verify both upgrade requirements and document
/// preservation.
fn repository_with_v1_observation() -> TempDir {
    let temp_dir = repository();
    let store_dir = temp_dir.path().join(".evidra");
    std::fs::create_dir(&store_dir).expect("store directory should be created");
    let database = store_dir.join("evidra.db");
    // TODO(MEDIUM): opens rusqlite directly instead of going through `evidra-store`, which is why
    // `rusqlite` is a dev-dependency of the CLI at all. Reaching around the port contradicts
    // AGENTS.md ("keep external systems behind ports defined in `evidra-core`") and means these
    // fixtures do not exercise the real v1 migration path — they assert against a schema literal
    // this test file itself defines, so a drift in `evidra-store` would not fail here.
    let connection = rusqlite::Connection::open(database).expect("v1 database should open");
    connection
        .execute_batch(V1_SCHEMA)
        .expect("v1 schema should apply");
    connection
        .pragma_update(None, "application_id", 0x4556_4452_i32)
        .expect("application ID should be set");
    connection
        .pragma_update(None, "user_version", 1_i32)
        .expect("v1 version should be set");
    let observation = Observation::record(ObservationDraft {
        occurred_at: Utc::now(),
        source: SourceRef::new("manual", "evidra note").expect("source should be valid"),
        kind: ObservationKind::ManualIntervention,
        subject: SubjectRef::new("repository", temp_dir.path().to_string_lossy())
            .expect("subject should be valid"),
        payload: json!({"summary": "Preserved v1 observation"}),
        provenance: Provenance::direct("evidra-cli").expect("provenance should be valid"),
    })
    .expect("observation should record");
    connection
        .execute(
            "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
            params![
                observation.id().to_string(),
                observation.observed_at().to_rfc3339(),
                serde_json::to_string(&observation).expect("observation should serialize")
            ],
        )
        .expect("v1 observation should insert");
    temp_dir
}

/// Serializes a valid redacted harness event with caller-controlled identity and result facet.
fn harness_record(source_event_id: &str, result: &str) -> String {
    serde_json::json!({
        "schema": "evidra.agent-harness-event/v1",
        "source_event_id": source_event_id,
        "occurred_at": "2026-09-19T08:00:00Z",
        "source": { "kind": "agent-harness", "locator": "session#event" },
        "subject": { "kind": "repository", "identifier": "/tmp/example" },
        "harness": { "name": "claude-code", "version": null },
        "session_id": "session-1",
        "event_type": "tool-completed",
        "redaction": { "policy": "test", "version": "1", "transformations": [] },
        "excerpts": [],
        "facets": [{ "name": "verification.result", "value": result }]
    })
    .to_string()
}

/// Initializes the store and performs an empty ingest to create the secure inbox directories.
fn initialize_inbox(temp_dir: &TempDir) {
    evidra()
        .current_dir(temp_dir.path())
        .arg("init")
        .assert()
        .success();
    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .success();
}

/// Writes a producer fixture into the ready inbox with the permissions required by ingestion.
fn write_inbox_file(temp_dir: &TempDir, name: &str, content: &str) {
    let path = temp_dir.path().join(".evidra/inbox").join(name);
    std::fs::write(&path, content).expect("inbox file should write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("inbox permissions should set");
    }
}

/// Confirms a valid ready event is recorded once and removed from the inbox after ingestion.
#[test]
fn ingest_records_then_deletes_ready_file() {
    let temp_dir = repository();
    initialize_inbox(&temp_dir);
    write_inbox_file(
        &temp_dir,
        "event-1.json",
        &harness_record("event-1", "passed"),
    );

    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .success()
        .stdout(predicate::eq(
            "Ingested 1 recorded, 0 duplicate, 0 quarantined\n",
        ));

    assert_eq!(
        std::fs::read_dir(temp_dir.path().join(".evidra/inbox"))
            .expect("inbox should list")
            .count(),
        0
    );
}

/// Confirms malformed JSON is quarantined without preventing a later valid event from recording.
#[test]
fn ingest_quarantines_invalid_and_continues() {
    let temp_dir = repository();
    initialize_inbox(&temp_dir);
    write_inbox_file(&temp_dir, "a-invalid.json", "{not-json}");
    write_inbox_file(
        &temp_dir,
        "b-valid.json",
        &harness_record("event-2", "passed"),
    );

    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ingested 1 recorded, 0 duplicate, 1 quarantined",
        ))
        .stdout(predicate::str::contains("Quarantine invalid-json: 1"));

    assert_eq!(
        std::fs::read_dir(temp_dir.path().join(".evidra/quarantine"))
            .expect("quarantine should list")
            .count(),
        2
    );
}

/// Pins the pretty-printed JSON ingest summary for a batch containing one recorded event.
#[test]
fn ingest_json_summary_is_stable() {
    let temp_dir = repository();
    initialize_inbox(&temp_dir);
    write_inbox_file(
        &temp_dir,
        "event-3.json",
        &harness_record("event-3", "passed"),
    );

    let output = evidra()
        .current_dir(temp_dir.path())
        .args(["ingest", "--json"])
        .output()
        .expect("ingest should run");

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"{\n  \"recorded\": 1,\n  \"duplicate\": 0,\n  \"quarantined\": 0,\n  \"reasons\": {}\n}\n"
    );
}

/// Simulates a recovered processing claim and confirms its previously recorded event is reported
/// as a duplicate before the claim file is deleted.
#[test]
fn ingest_duplicate_after_processing_recovery() {
    let temp_dir = repository();
    initialize_inbox(&temp_dir);
    let record = harness_record("event-4", "passed");
    write_inbox_file(&temp_dir, "event-4.json", &record);
    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .success();
    let processing = temp_dir
        .path()
        .join(format!(".evidra/processing/{}.json", ulid::Ulid::new()));
    std::fs::write(&processing, record).expect("recovered processing file should write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&processing, std::fs::Permissions::from_mode(0o600))
            .expect("processing permissions should set");
    }

    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ingested 0 recorded, 1 duplicate, 0 quarantined",
        ));
    assert!(!processing.exists());
}

/// Records two semantically different events with one source identity and expects the second to
/// be quarantined as an identity conflict.
#[test]
fn ingest_quarantines_identity_conflict() {
    let temp_dir = repository();
    initialize_inbox(&temp_dir);
    write_inbox_file(
        &temp_dir,
        "first.json",
        &harness_record("event-5", "passed"),
    );
    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .success();
    write_inbox_file(
        &temp_dir,
        "changed.json",
        &harness_record("event-5", "failed"),
    );

    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .success()
        .stdout(predicate::str::contains("Quarantine identity-conflict: 1"));
}

/// Runs ingestion without a store and pins the failure to an empty stdout and stable store error.
#[test]
fn ingest_rejects_uninitialized_repository_with_fixed_error() {
    let temp_dir = repository();

    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::eq("ingest failed: store\n"));
}

/// Attempts ingestion against a schema-v1 store and confirms migration is required before inbox
/// claims can begin.
#[test]
fn ingest_rejects_v1_before_claiming() {
    let temp_dir = repository_with_v1_observation();

    evidra()
        .current_dir(temp_dir.path())
        .arg("ingest")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::eq("ingest failed: store\n"));
}

/// Runs a non-migration command against schema v1 and checks that the error directs the operator
/// to back up the store and run `evidra init`.
#[test]
fn ordinary_command_rejects_v1_with_migration_guidance() {
    let temp_dir = repository_with_v1_observation();

    evidra()
        .current_dir(temp_dir.path())
        .args(["observation", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("back it up and run `evidra init`"));
}

/// Migrates a schema-v1 store through `init` and confirms its existing observation remains
/// readable afterward.
#[test]
fn init_migrates_v1_then_preserves_manual_observation() {
    let temp_dir = repository_with_v1_observation();

    evidra()
        .current_dir(temp_dir.path())
        .arg("init")
        .assert()
        .success();
    evidra()
        .current_dir(temp_dir.path())
        .args(["observation", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Preserved v1 observation"));
}

/// Exercises initialization, local ignore-file creation, note recording, idempotent
/// reinitialization, and human-readable listing in one repository.
#[test]
fn init_note_and_list_complete_local_loop() {
    let temp_dir = repository();

    evidra()
        .current_dir(temp_dir.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized Evidra"));
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join(".evidra/.gitignore"))
            .expect("local ignore file should be readable"),
        "*\n!.gitignore\n"
    );

    evidra()
        .current_dir(temp_dir.path())
        .args(["note", "--summary", "Removed stale generated artifacts"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded observation"));

    evidra()
        .current_dir(temp_dir.path())
        .arg("init")
        .assert()
        .success();

    evidra()
        .current_dir(temp_dir.path())
        .args(["observation", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("manual-intervention"))
        .stdout(predicate::str::contains(
            "Removed stale generated artifacts",
        ));
}

/// Records a manual note and parses JSON list output to verify its kind and summary fields.
#[test]
fn json_list_is_machine_readable() {
    let temp_dir = repository();

    evidra()
        .current_dir(temp_dir.path())
        .arg("init")
        .assert()
        .success();
    evidra()
        .current_dir(temp_dir.path())
        .args(["note", "--summary", "Updated the recovery runbook"])
        .assert()
        .success();

    let output = evidra()
        .current_dir(temp_dir.path())
        .args(["observation", "list", "--json"])
        .output()
        .expect("list command should run");

    assert!(output.status.success());
    let observations: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output should be valid JSON");
    assert_eq!(observations[0]["kind"], "manual-intervention");
    assert_eq!(
        observations[0]["payload"]["summary"],
        "Updated the recovery runbook"
    );
}

/// Initializes a store and confirms whitespace-only note input fails with the validation reason.
#[test]
fn note_rejects_blank_summary() {
    let temp_dir = repository();
    evidra()
        .current_dir(temp_dir.path())
        .arg("init")
        .assert()
        .success();

    evidra()
        .current_dir(temp_dir.path())
        .args(["note", "--summary", "   "])
        .assert()
        .failure()
        .stderr(predicate::str::contains("summary must not be blank"));
}

/// Attempts to record a note without a store and checks that the error names the required init
/// command.
#[test]
fn note_before_init_reports_actionable_error() {
    let temp_dir = repository();

    evidra()
        .current_dir(temp_dir.path())
        .args(["note", "--summary", "Recorded before initialization"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("run `evidra init` first"));
}

/// Records a note from a nested directory and confirms repository discovery stores it at the root.
#[test]
fn nested_commands_use_repository_root() {
    let temp_dir = repository();
    let nested = temp_dir.path().join("crates/example/src");
    std::fs::create_dir_all(&nested).expect("nested directory should be created");

    evidra()
        .current_dir(temp_dir.path())
        .arg("init")
        .assert()
        .success();
    evidra()
        .current_dir(&nested)
        .args(["note", "--summary", "Recorded from a nested directory"])
        .assert()
        .success();

    evidra()
        .current_dir(temp_dir.path())
        .args(["observation", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded from a nested directory"));
}

/// Requests a list limit above the supported maximum and checks the bounded-range error.
#[test]
fn list_rejects_unbounded_limit() {
    let temp_dir = repository();

    evidra()
        .current_dir(temp_dir.path())
        .args(["observation", "list", "--limit", "1001"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "observation limit must be between 1 and 1000",
        ));
}

/// Records a summary containing newline and tab characters and confirms table output renders
/// their escaped forms instead of raw control characters.
#[test]
fn table_output_escapes_control_characters() {
    let temp_dir = repository();
    evidra()
        .current_dir(temp_dir.path())
        .arg("init")
        .assert()
        .success();
    evidra()
        .current_dir(temp_dir.path())
        .args(["note", "--summary", "first line\nsecond line\tvalue"])
        .assert()
        .success();

    evidra()
        .current_dir(temp_dir.path())
        .args(["observation", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("first line\\nsecond line\\tvalue"));
}

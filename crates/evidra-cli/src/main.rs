//! Evidra's command-line entry point and composition root.
//!
//! This module parses commands, connects repository-local inbox and storage adapters to the
//! domain APIs, and renders results for terminal or JSON output.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use clap::{Parser, Subcommand};
use evidra_adapters::AgentHarnessFileInbox;
use evidra_core::{
    AgentHarnessIngestError, AgentHarnessIngestSummary, Observation, ObservationDraft,
    ObservationKind, ObservationStore, Provenance, SourceRef, SubjectRef, ingest_agent_harness,
};
use evidra_store::SqliteObservationStore;
use serde_json::json;

const STORE_DIRECTORY: &str = ".evidra";
const STORE_FILENAME: &str = "evidra.db";

#[derive(Debug, Parser)]
#[command(name = "evidra", version, about = "Evidence into controls.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize repository-local Evidra storage.
    Init,
    /// Record a manual intervention observation.
    Note {
        /// Concise description of the intervention.
        #[arg(long)]
        summary: String,
    },
    /// Inspect recorded observations.
    Observation {
        #[command(subcommand)]
        command: ObservationCommand,
    },
    /// Ingest atomically published agent-harness event files.
    Ingest {
        /// Render the batch summary as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ObservationCommand {
    /// List observations from newest to oldest.
    List {
        /// Maximum number of observations to return.
        #[arg(
            long,
            default_value_t = 50,
            value_parser = parse_limit
        )]
        limit: usize,
        /// Render the complete observation envelopes as JSON.
        #[arg(long)]
        json: bool,
    },
}

enum CliError {
    General(anyhow::Error),
    Ingest(AgentHarnessIngestError),
}

/// Classifies a general application error for CLI-level reporting.
impl From<anyhow::Error> for CliError {
    /// Wraps an application error as a general CLI failure.
    fn from(error: anyhow::Error) -> Self {
        Self::General(error)
    }
}

/// Runs the CLI, reports any failure to standard error, and returns its process exit status.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Ingest(error)) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
        Err(CliError::General(error)) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Parses one command, locates the enclosing Git repository, and dispatches the requested action.
fn run() -> std::result::Result<(), CliError> {
    let cli = Cli::parse();
    let current_dir = env::current_dir().context("failed to determine the current directory")?;
    let root = repository_root(&current_dir)?;
    let stdout = io::stdout();
    let mut output = stdout.lock();

    match cli.command {
        Command::Init => initialize(&root, &mut output).map_err(CliError::General),
        Command::Note { summary } => {
            record_note(&root, &summary, &mut output).map_err(CliError::General)
        }
        Command::Observation {
            command: ObservationCommand::List { limit, json },
        } => list_observations(&root, limit, json, &mut output).map_err(CliError::General),
        Command::Ingest { json } => ingest(&root, json, &mut output),
    }
}

/// Ingests pending agent-harness files into the repository store and writes a batch summary.
fn ingest(
    root: &Path,
    as_json: bool,
    output: &mut impl Write,
) -> std::result::Result<(), CliError> {
    let mut store = SqliteObservationStore::open(&database_path(root))
        .map_err(|_| CliError::Ingest(AgentHarnessIngestError::Store))?;
    let mut inbox = AgentHarnessFileInbox::initialize(&root.join(STORE_DIRECTORY))
        .map_err(|_| CliError::Ingest(AgentHarnessIngestError::Inbox))?;
    let summary =
        ingest_agent_harness(&mut inbox, &mut store, "evidra-cli").map_err(CliError::Ingest)?;
    let rendered = render_ingest_summary(&summary, as_json).map_err(CliError::General)?;
    output
        .write_all(&rendered)
        .map_err(|_| CliError::Ingest(AgentHarnessIngestError::Inbox))
}

/// Serializes an ingest summary as pretty JSON or a human-readable quarantine breakdown.
fn render_ingest_summary(summary: &AgentHarnessIngestSummary, as_json: bool) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    if as_json {
        serde_json::to_writer_pretty(&mut output, summary)?;
        output.push(b'\n');
        return Ok(output);
    }
    writeln!(
        output,
        "Ingested {} recorded, {} duplicate, {} quarantined",
        summary.recorded(),
        summary.duplicate(),
        summary.quarantined()
    )?;
    for (reason, count) in summary.reasons() {
        writeln!(output, "Quarantine {reason}: {count}")?;
    }
    Ok(output)
}

/// Initializes repository-local storage, protects it from Git tracking, and reports its location.
fn initialize(root: &Path, output: &mut impl Write) -> Result<()> {
    let database = database_path(root);
    SqliteObservationStore::initialize(&database)
        .with_context(|| format!("failed to initialize {}", database.display()))?;
    ensure_store_ignored(root)?;
    writeln!(
        output,
        "Initialized Evidra at {}",
        database.display().to_string().escape_debug()
    )?;
    Ok(())
}

/// Validates and appends a manual-intervention observation for the repository.
fn record_note(root: &Path, summary: &str, output: &mut impl Write) -> Result<()> {
    let summary = summary.trim();
    ensure!(!summary.is_empty(), "summary must not be blank");

    let database = database_path(root);
    let mut store = SqliteObservationStore::open(&database)?;
    let repository = root
        .to_str()
        .context("repository path is not valid UTF-8")?;
    let observation = Observation::record(ObservationDraft {
        occurred_at: Utc::now(),
        source: SourceRef::new("manual", "evidra note")?,
        kind: ObservationKind::ManualIntervention,
        subject: SubjectRef::new("repository", repository)?,
        payload: json!({ "summary": summary }),
        provenance: Provenance::direct("evidra-cli")?,
    })?;

    store.append(&observation)?;
    writeln!(output, "Recorded observation {}", observation.id())?;
    Ok(())
}

/// Writes up to `limit` stored observations as pretty JSON or a tab-separated table.
fn list_observations(
    root: &Path,
    limit: usize,
    as_json: bool,
    output: &mut impl Write,
) -> Result<()> {
    let store = SqliteObservationStore::open(&database_path(root))?;
    let observations = store.list(limit)?;

    if as_json {
        serde_json::to_writer_pretty(&mut *output, &observations)?;
        writeln!(output)?;
        return Ok(());
    }

    writeln!(output, "ID\tOCCURRED AT\tKIND\tSUMMARY")?;
    for observation in observations {
        let summary = observation
            .payload()
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-")
            .escape_debug();
        writeln!(
            output,
            "{}\t{}\t{}\t{}",
            observation.id(),
            observation.occurred_at().to_rfc3339(),
            observation.kind(),
            summary
        )?;
    }
    Ok(())
}

/// Builds the path to the repository-local SQLite database.
fn database_path(root: &Path) -> PathBuf {
    root.join(STORE_DIRECTORY).join(STORE_FILENAME)
}

/// Writes the store's ignore rules after rejecting an existing symbolic-link ignore file.
fn ensure_store_ignored(root: &Path) -> Result<()> {
    let ignore_file = root.join(STORE_DIRECTORY).join(".gitignore");
    if let Ok(metadata) = fs::symlink_metadata(&ignore_file) {
        ensure!(
            !metadata.file_type().is_symlink(),
            "refusing symbolic-link ignore file {}",
            ignore_file.display()
        );
    }
    fs::write(&ignore_file, "*\n!.gitignore\n")
        .with_context(|| format!("failed to write {}", ignore_file.display()))?;
    Ok(())
}

/// Finds the nearest ancestor of `start` that contains a `.git` entry.
fn repository_root(start: &Path) -> Result<PathBuf> {
    start
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .map(Path::to_path_buf)
        .context("current directory is not inside a Git repository")
}

/// Parses an observation limit and accepts only values from 1 through 1,000.
fn parse_limit(value: &str) -> std::result::Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|error| format!("invalid observation limit: {error}"))?;
    if (1..=1000).contains(&limit) {
        Ok(limit)
    } else {
        Err("observation limit must be between 1 and 1000".to_owned())
    }
}

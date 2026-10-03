//! SQLite persistence for Evidra's append-only observation ledger.
//!
//! This module owns database initialization, v1-to-v2 migration, schema and integrity
//! validation, filesystem safeguards, and atomic harness-observation receipts.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use evidra_core::{
    AgentHarnessAppendOutcome, AgentHarnessEvent, AgentHarnessEventDraft,
    AgentHarnessEventIdentity, AgentHarnessObservation, Derivation, FacetValue, HarnessEventType,
    HarnessRef, HarnessSessionId, Observation, ObservationError, ObservationStore, RedactionRecord,
    Relationship, SourceEventId, SourceRef, SubjectRef,
};
use miette::Diagnostic;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

const APPLICATION_ID: i32 = 0x4556_4452;
const LEGACY_SCHEMA_VERSION: i32 = 1;
const RECEIPT_SCHEMA_VERSION: i32 = 2;
const SCHEMA_VERSION: i32 = 3;
const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS observations (
    id TEXT PRIMARY KEY NOT NULL,
    observed_at TEXT NOT NULL,
    document TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS observations_observed_at_idx
    ON observations (observed_at DESC, id DESC);

CREATE TRIGGER IF NOT EXISTS observations_reject_update
BEFORE UPDATE ON observations
BEGIN
    SELECT RAISE(ABORT, 'observations are append-only');
END;

CREATE TRIGGER IF NOT EXISTS observations_reject_delete
BEFORE DELETE ON observations
BEGIN
    SELECT RAISE(ABORT, 'observations are append-only');
END;

CREATE TRIGGER IF NOT EXISTS observations_reject_duplicate_insert
BEFORE INSERT ON observations
WHEN EXISTS (SELECT 1 FROM observations WHERE id = NEW.id)
BEGIN
    SELECT RAISE(ABORT, 'observations are append-only');
END;
";

const RECEIPT_SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS agent_harness_receipts (
    harness TEXT NOT NULL,
    session_id TEXT NOT NULL,
    source_event_id TEXT NOT NULL,
    event_digest TEXT NOT NULL,
    observation_id TEXT NOT NULL UNIQUE,
    PRIMARY KEY (harness, session_id, source_event_id),
    FOREIGN KEY (observation_id) REFERENCES observations(id)
);

CREATE TRIGGER IF NOT EXISTS agent_harness_receipts_reject_update
BEFORE UPDATE ON agent_harness_receipts
BEGIN
    SELECT RAISE(ABORT, 'agent harness receipts are append-only');
END;

CREATE TRIGGER IF NOT EXISTS agent_harness_receipts_reject_delete
BEFORE DELETE ON agent_harness_receipts
BEGIN
    SELECT RAISE(ABORT, 'agent harness receipts are append-only');
END;

CREATE TRIGGER IF NOT EXISTS agent_harness_receipts_reject_duplicate_insert
BEFORE INSERT ON agent_harness_receipts
WHEN EXISTS (
    SELECT 1 FROM agent_harness_receipts
    WHERE harness = NEW.harness
      AND session_id = NEW.session_id
      AND source_event_id = NEW.source_event_id
)
OR EXISTS (
    SELECT 1 FROM agent_harness_receipts
    WHERE observation_id = NEW.observation_id
)
BEGIN
    SELECT RAISE(ABORT, 'agent harness receipts are append-only');
END;
";

/// The derived layer: derivations, their projected facets, their evidence, and the typed edges
/// between records.
///
/// Every table carries the same three immutability triggers as the observation ledger, because the
/// derived layer is revisable only by appending. A correction is a new derivation plus a
/// `supersedes` relationship; it never rewrites the record it replaces.
///
/// Two constraints here are deliberately stronger than the application layer, so that a bug in a
/// writer cannot produce a record the domain would have refused. The facet slot must agree with
/// which value column is populated, and a relationship may not point at itself.
const DERIVED_SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS derivations (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('facet', 'aggregate', 'cluster')),
    recorded_at TEXT NOT NULL,
    supersedes TEXT,
    document TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS derivations_recorded_at_idx
    ON derivations (recorded_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS derivations_supersedes_idx
    ON derivations (supersedes, id);

CREATE TRIGGER IF NOT EXISTS derivations_reject_update
BEFORE UPDATE ON derivations
BEGIN
    SELECT RAISE(ABORT, 'derivations are append-only');
END;

CREATE TRIGGER IF NOT EXISTS derivations_reject_delete
BEFORE DELETE ON derivations
BEGIN
    SELECT RAISE(ABORT, 'derivations are append-only');
END;

CREATE TRIGGER IF NOT EXISTS derivations_reject_duplicate_insert
BEFORE INSERT ON derivations
WHEN EXISTS (SELECT 1 FROM derivations WHERE id = NEW.id)
BEGIN
    SELECT RAISE(ABORT, 'derivations are append-only');
END;

CREATE TABLE IF NOT EXISTS derivation_facets (
    derivation_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    slot TEXT NOT NULL CHECK (slot IN ('text', 'integer', 'boolean')),
    text_value TEXT,
    integer_value INTEGER,
    boolean_value INTEGER CHECK (boolean_value IS NULL OR boolean_value IN (0, 1)),
    confidence TEXT NOT NULL
        CHECK (confidence IN ('speculative', 'weak', 'moderate', 'strong')),
    freshness TEXT NOT NULL CHECK (freshness IN ('current', 'aging', 'stale')),
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (derivation_id, namespace, name),
    FOREIGN KEY (derivation_id) REFERENCES derivations(id),
    CHECK (
        (slot = 'text'
            AND text_value IS NOT NULL
            AND integer_value IS NULL
            AND boolean_value IS NULL)
        OR (slot = 'integer'
            AND text_value IS NULL
            AND integer_value IS NOT NULL
            AND boolean_value IS NULL)
        OR (slot = 'boolean'
            AND text_value IS NULL
            AND integer_value IS NULL
            AND boolean_value IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS derivation_facets_lookup_idx
    ON derivation_facets (namespace, name, recorded_at DESC);

CREATE TRIGGER IF NOT EXISTS derivation_facets_reject_update
BEFORE UPDATE ON derivation_facets
BEGIN
    SELECT RAISE(ABORT, 'derivation facets are append-only');
END;

CREATE TRIGGER IF NOT EXISTS derivation_facets_reject_delete
BEFORE DELETE ON derivation_facets
BEGIN
    SELECT RAISE(ABORT, 'derivation facets are append-only');
END;

CREATE TRIGGER IF NOT EXISTS derivation_facets_reject_duplicate_insert
BEFORE INSERT ON derivation_facets
WHEN EXISTS (
    SELECT 1 FROM derivation_facets
    WHERE derivation_id = NEW.derivation_id
      AND namespace = NEW.namespace
      AND name = NEW.name
)
BEGIN
    SELECT RAISE(ABORT, 'derivation facets are append-only');
END;

CREATE TABLE IF NOT EXISTS derivation_evidence (
    derivation_id TEXT NOT NULL,
    target_key TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('supporting', 'refuting')),
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (derivation_id, target_key),
    FOREIGN KEY (derivation_id) REFERENCES derivations(id)
);

CREATE INDEX IF NOT EXISTS derivation_evidence_target_idx
    ON derivation_evidence (target_key, role);

CREATE TRIGGER IF NOT EXISTS derivation_evidence_reject_update
BEFORE UPDATE ON derivation_evidence
BEGIN
    SELECT RAISE(ABORT, 'derivation evidence is append-only');
END;

CREATE TRIGGER IF NOT EXISTS derivation_evidence_reject_delete
BEFORE DELETE ON derivation_evidence
BEGIN
    SELECT RAISE(ABORT, 'derivation evidence is append-only');
END;

CREATE TRIGGER IF NOT EXISTS derivation_evidence_reject_duplicate_insert
BEFORE INSERT ON derivation_evidence
WHEN EXISTS (
    SELECT 1 FROM derivation_evidence
    WHERE derivation_id = NEW.derivation_id AND target_key = NEW.target_key
)
BEGIN
    SELECT RAISE(ABORT, 'derivation evidence is append-only');
END;

CREATE TABLE IF NOT EXISTS relationships (
    id TEXT PRIMARY KEY NOT NULL,
    from_key TEXT NOT NULL,
    to_key TEXT NOT NULL,
    relation TEXT NOT NULL CHECK (relation IN (
        'supports', 'refutes', 'caused-by', 'enabled', 'prevented', 'supersedes',
        'derives-from', 'co-occurs-with', 'no-effect'
    )),
    confidence TEXT NOT NULL
        CHECK (confidence IN ('speculative', 'weak', 'moderate', 'strong')),
    recorded_at TEXT NOT NULL,
    document TEXT NOT NULL,
    CHECK (from_key <> to_key)
);

CREATE INDEX IF NOT EXISTS relationships_from_key_idx ON relationships (from_key, relation);

CREATE INDEX IF NOT EXISTS relationships_to_key_idx ON relationships (to_key, relation);

CREATE INDEX IF NOT EXISTS relationships_relation_idx ON relationships (relation);

CREATE TRIGGER IF NOT EXISTS relationships_reject_update
BEFORE UPDATE ON relationships
BEGIN
    SELECT RAISE(ABORT, 'relationships are append-only');
END;

CREATE TRIGGER IF NOT EXISTS relationships_reject_delete
BEFORE DELETE ON relationships
BEGIN
    SELECT RAISE(ABORT, 'relationships are append-only');
END;

CREATE TRIGGER IF NOT EXISTS relationships_reject_duplicate_insert
BEFORE INSERT ON relationships
WHEN EXISTS (SELECT 1 FROM relationships WHERE id = NEW.id)
BEGIN
    SELECT RAISE(ABORT, 'relationships are append-only');
END;
";

/// SQLite-backed append-only observation store.
pub struct SqliteObservationStore {
    connection: Connection,
    #[cfg(test)]
    append_pause: Option<AppendPause>,
}

#[cfg(test)]
struct AppendPause {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl SqliteObservationStore {
    /// Creates parent directories, opens the database, and applies the current schema.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the directory, database, or schema cannot be created.
    pub fn initialize(path: &Path) -> Result<Self, StoreError> {
        reject_store_symlinks(path)?;
        let database_existed = path.exists();
        if let Some(parent) = path.parent() {
            reject_symlink_ancestors(parent)?;
            let parent_existed = parent.exists();
            fs::create_dir_all(parent).map_err(|source| StoreError::CreateParent {
                path: parent.to_path_buf(),
                source,
            })?;
            restrict_new_directory(parent, parent_existed)?;
        }

        let mut connection = open_connection(path)?;
        configure_connection(&connection)?;
        if database_existed {
            let (application_id, user_version) = database_identity(&connection)?;
            if application_id != APPLICATION_ID {
                return Err(StoreError::UnexpectedDatabase {
                    path: path.to_path_buf(),
                    application_id,
                    user_version,
                });
            }
            match user_version {
                LEGACY_SCHEMA_VERSION => {
                    validate_v1(&connection, path)?;
                    enable_wal(&connection)?;
                    restrict_store_permissions(path)?;
                    migrate_v1_to_v2(&mut connection, path)?;
                    migrate_v2_to_v3(&mut connection, path)?;
                }
                RECEIPT_SCHEMA_VERSION => {
                    validate_v2(&connection, path)?;
                    enable_wal(&connection)?;
                    restrict_store_permissions(path)?;
                    migrate_v2_to_v3(&mut connection, path)?;
                }
                SCHEMA_VERSION => {
                    validate_v3(&connection, path)?;
                    enable_wal(&connection)?;
                }
                _ => {
                    return Err(StoreError::UnexpectedDatabase {
                        path: path.to_path_buf(),
                        application_id,
                        user_version,
                    });
                }
            }
        } else {
            enable_wal(&connection)?;
            restrict_store_permissions(path)?;
            initialize_v3(&mut connection, path)?;
        }
        validate_v3(&connection, path)?;
        restrict_store_permissions(path)?;
        Ok(Self {
            connection,
            #[cfg(test)]
            append_pause: None,
        })
    }

    /// Opens an existing initialized database without creating a missing store.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when `path` is not a file, or another
    /// [`StoreError`] when opening or validating the database fails.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        reject_store_symlinks(path)?;
        if let Some(parent) = path.parent() {
            reject_symlink_ancestors(parent)?;
        }
        if !path.is_file() {
            return Err(StoreError::NotInitialized {
                path: path.to_path_buf(),
            });
        }

        let connection = open_connection(path)?;
        configure_connection(&connection)?;
        let (application_id, user_version) = database_identity(&connection)?;
        if application_id == APPLICATION_ID
            && (user_version == LEGACY_SCHEMA_VERSION || user_version == RECEIPT_SCHEMA_VERSION)
        {
            if user_version == LEGACY_SCHEMA_VERSION {
                validate_v1(&connection, path)?;
            } else {
                validate_v2(&connection, path)?;
            }
            return Err(StoreError::MigrationRequired {
                path: path.to_path_buf(),
                current_version: user_version,
                target_version: SCHEMA_VERSION,
            });
        }
        if application_id != APPLICATION_ID || user_version != SCHEMA_VERSION {
            return Err(StoreError::UnexpectedDatabase {
                path: path.to_path_buf(),
                application_id,
                user_version,
            });
        }
        validate_v3(&connection, path)?;
        enable_wal(&connection)?;
        Ok(Self {
            connection,
            #[cfg(test)]
            append_pause: None,
        })
    }
}

impl fmt::Debug for SqliteObservationStore {
    /// Formats this value without exposing sensitive evidence.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SqliteObservationStore")
            .finish_non_exhaustive()
    }
}

impl ObservationStore for SqliteObservationStore {
    type Error = StoreError;

    /// Appends a non-harness observation in its own transaction.
    fn append(&mut self, observation: &Observation) -> Result<(), Self::Error> {
        if observation.kind() == &evidra_core::ObservationKind::AgentHarnessEvent {
            return Err(StoreError::HarnessObservationRequiresReceipt);
        }
        let transaction = self.connection.transaction()?;
        insert_observation(&transaction, observation)?;
        transaction.commit()?;
        Ok(())
    }

    /// Returns up to `limit` observations in reverse chronological order.
    fn list(&self, limit: usize) -> Result<Vec<Observation>, Self::Error> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let bounded_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(
            "SELECT id, observed_at, document
             FROM observations
             ORDER BY observed_at DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([bounded_limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut observations = Vec::new();

        for row in rows {
            let (stored_id, stored_observed_at, document) = row?;
            let observation: Observation =
                serde_json::from_str(&document).map_err(|_| StoreError::Serialization)?;
            if !observation.verify_integrity()? {
                return Err(StoreError::IntegrityMismatch {
                    id: observation.id().to_string(),
                });
            }
            if stored_id != observation.id().to_string()
                || stored_observed_at != observation.observed_at().to_rfc3339()
            {
                return Err(StoreError::MetadataMismatch {
                    id: observation.id().to_string(),
                });
            }
            observations.push(observation);
        }

        Ok(observations)
    }

    /// Atomically appends a harness observation and its idempotency receipt.
    fn append_harness_observation(
        &mut self,
        value: &AgentHarnessObservation,
    ) -> Result<AgentHarnessAppendOutcome, Self::Error> {
        let incoming_valid = AgentHarnessObservation::verify_receipt(
            value.identity(),
            value.event_digest(),
            value.observation(),
        )
        .map_err(|_| StoreError::InvalidHarnessReceipt)?;
        if !incoming_valid {
            return Err(StoreError::InvalidHarnessReceipt);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        #[cfg(test)]
        if let Some(pause) = self.append_pause.take() {
            let _ = pause.entered.send(());
            let _ = pause.release.recv();
        }
        let identity = value.identity();
        let stored_digest = transaction
            .query_row(
                "SELECT event_digest FROM agent_harness_receipts
                 WHERE harness = ?1 AND session_id = ?2 AND source_event_id = ?3",
                params![
                    identity.harness(),
                    identity.session_id(),
                    identity.source_event_id()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(stored_digest) = stored_digest {
            validate_receipt_rows(&transaction)?;
            return if stored_digest == value.event_digest() {
                Ok(AgentHarnessAppendOutcome::Duplicate)
            } else {
                Ok(AgentHarnessAppendOutcome::IdentityConflict)
            };
        }

        insert_observation(&transaction, value.observation())?;
        transaction.execute(
            "INSERT INTO agent_harness_receipts
             (harness, session_id, source_event_id, event_digest, observation_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                identity.harness(),
                identity.session_id(),
                identity.source_event_id(),
                value.event_digest(),
                value.observation().id().to_string()
            ],
        )?;
        validate_receipt_rows(&transaction)?;
        transaction.commit()?;
        Ok(AgentHarnessAppendOutcome::Recorded)
    }
}

/// Verifies and inserts one observation through the supplied connection or transaction.
fn insert_observation(
    connection: &Connection,
    observation: &Observation,
) -> Result<(), StoreError> {
    if !observation.verify_integrity()? {
        return Err(StoreError::IntegrityMismatch {
            id: observation.id().to_string(),
        });
    }
    let document = serde_json::to_string(observation).map_err(|_| StoreError::Serialization)?;
    let result = connection.execute(
        "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
        params![
            observation.id().to_string(),
            observation.observed_at().to_rfc3339(),
            document
        ],
    );
    match result {
        Ok(_) => Ok(()),
        Err(error) if error.sqlite_error_code() == Some(ErrorCode::ConstraintViolation) => {
            Err(StoreError::DuplicateObservation {
                id: observation.id().to_string(),
            })
        }
        Err(error) => Err(StoreError::Database(error)),
    }
}

/// Error returned by the SQLite observation store.
#[derive(Debug, Error, Diagnostic)]
pub enum StoreError {
    /// The repository-local database has not been initialized.
    #[error("Evidra is not initialized at {path}; run `evidra init` first")]
    #[diagnostic(code(evidra::store::not_initialized))]
    NotInitialized {
        /// Expected database path.
        path: PathBuf,
    },

    /// A database parent directory could not be created.
    #[error("failed to create database directory {path}: {source}")]
    #[diagnostic(code(evidra::store::create_parent))]
    CreateParent {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// SQLite could not open the configured database path.
    #[error("failed to open SQLite database {path}: {source}")]
    #[diagnostic(code(evidra::store::open))]
    Open {
        /// Database path that could not be opened.
        path: PathBuf,
        /// Underlying SQLite error.
        #[source]
        source: rusqlite::Error,
    },

    /// An observation with the same identity already exists.
    #[error("observation {id} already exists")]
    #[diagnostic(code(evidra::store::duplicate_observation))]
    DuplicateObservation {
        /// Duplicate observation identity.
        id: String,
    },

    /// Harness observations require an atomic idempotency receipt.
    #[error("agent harness observations must be appended with an idempotency receipt")]
    #[diagnostic(code(evidra::store::harness_observation_requires_receipt))]
    HarnessObservationRequiresReceipt,

    /// A persisted observation no longer matches its integrity digest.
    #[error("observation {id} failed integrity verification")]
    #[diagnostic(code(evidra::store::integrity_mismatch))]
    IntegrityMismatch {
        /// Identity of the altered or corrupted observation.
        id: String,
    },

    /// Indexed SQLite columns disagree with the signed observation document.
    #[error("observation {id} has inconsistent indexed metadata")]
    #[diagnostic(code(evidra::store::metadata_mismatch))]
    MetadataMismatch {
        /// Identity from the signed observation document.
        id: String,
    },

    /// A store path is a symbolic link and could redirect writes.
    #[error("refusing symbolic-link store path {path}")]
    #[diagnostic(code(evidra::store::symlink_path))]
    SymlinkPath {
        /// Symbolic link that was rejected.
        path: PathBuf,
    },

    /// A store path could not be inspected safely.
    #[error("failed to inspect store path {path}: {source}")]
    #[diagnostic(code(evidra::store::inspect_path))]
    InspectPath {
        /// Path that could not be inspected.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// An initialized legacy database requires explicit migration.
    #[error(
        "database {path} uses schema v{current_version}; back it up and run `evidra init` to migrate to v{target_version}"
    )]
    #[diagnostic(code(evidra::store::migration_required))]
    MigrationRequired {
        /// Legacy database path.
        path: PathBuf,
        /// Current schema version.
        current_version: i32,
        /// Required schema version.
        target_version: i32,
    },

    /// An existing database does not belong to this Evidra schema version.
    #[error(
        "database {path} is not an Evidra schema (application_id={application_id}, user_version={user_version})"
    )]
    #[diagnostic(code(evidra::store::unexpected_database))]
    UnexpectedDatabase {
        /// Rejected database path.
        path: PathBuf,
        /// SQLite application identifier found in the file.
        application_id: i32,
        /// SQLite schema version found in the file.
        user_version: i32,
    },

    /// An expected table, index, or append-only trigger is missing.
    #[error("database {path} is missing required schema object {object}")]
    #[diagnostic(code(evidra::store::incomplete_schema))]
    IncompleteSchema {
        /// Database with an incomplete schema.
        path: PathBuf,
        /// Missing SQLite schema object.
        object: &'static str,
    },

    /// The database schema permits a mutation forbidden by append-only semantics.
    #[error("database {path} failed append-only control probe for {operation}")]
    #[diagnostic(code(evidra::store::incomplete_append_only_control))]
    IncompleteAppendOnlyControl {
        /// Database with ineffective controls.
        path: PathBuf,
        /// Mutation that was not blocked.
        operation: &'static str,
    },

    /// A persisted harness receipt disagrees with its observation.
    #[error("invalid persisted agent harness receipt")]
    #[diagnostic(code(evidra::store::invalid_harness_receipt))]
    InvalidHarnessReceipt,

    /// The database schema permits a derived mutation forbidden by append-only semantics.
    #[error("database {path} does not enforce the {constraint} constraint on derived records")]
    #[diagnostic(code(evidra::store::incomplete_derived_constraint))]
    IncompleteDerivedConstraint {
        /// Database with an ineffective derived constraint.
        path: PathBuf,
        /// Constraint that was not enforced.
        constraint: &'static str,
    },

    /// A persisted derived document could not be read back as a domain record.
    #[error("derived record {id} could not be deserialized")]
    #[diagnostic(code(evidra::store::invalid_derived_record))]
    InvalidDerivedRecord {
        /// Identity of the unreadable record.
        id: String,
    },

    /// Indexed derived columns disagree with the stored derived document.
    #[error("derived record {id} has inconsistent indexed metadata")]
    #[diagnostic(code(evidra::store::derived_metadata_mismatch))]
    DerivedMetadataMismatch {
        /// Identity from the stored derived document.
        id: String,
    },

    /// Indexed facet rows disagree with the facets recorded on the derivation.
    #[error("derived record {id} has facet rows that disagree with its document")]
    #[diagnostic(code(evidra::store::derived_facet_mismatch))]
    DerivedFacetMismatch {
        /// Identity of the record whose projection drifted.
        id: String,
    },

    /// Migration changed a stored observation document it must not have touched.
    #[error("schema migration altered stored observation documents")]
    #[diagnostic(code(evidra::store::invalid_derived_schema))]
    InvalidDerivedSchema,

    /// Store permissions could not be restricted.
    #[error("failed to set private permissions on {path}: {source}")]
    #[diagnostic(code(evidra::store::set_permissions))]
    SetPermissions {
        /// Path whose permissions could not be changed.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// A SQLite operation failed.
    #[error("SQLite operation failed: {0}")]
    Database(#[from] rusqlite::Error),

    /// Observation JSON could not be encoded or decoded.
    #[error("observation serialization failed")]
    #[diagnostic(code(evidra::store::serialization))]
    Serialization,

    /// Observation integrity verification could not be completed.
    #[error("observation integrity verification failed: {0}")]
    IntegrityVerification(#[from] ObservationError),
}

/// Opens SQLite through a canonical parent path without following the database symlink.
fn open_connection(path: &Path) -> Result<Connection, StoreError> {
    let parent = path.parent().ok_or_else(|| StoreError::Open {
        path: path.to_path_buf(),
        source: rusqlite::Error::InvalidPath(path.to_path_buf()),
    })?;
    let canonical_parent = fs::canonicalize(parent).map_err(|source| StoreError::InspectPath {
        path: parent.to_path_buf(),
        source,
    })?;
    let filename = path.file_name().ok_or_else(|| StoreError::Open {
        path: path.to_path_buf(),
        source: rusqlite::Error::InvalidPath(path.to_path_buf()),
    })?;
    let canonical_path = canonical_parent.join(filename);
    Connection::open_with_flags(
        canonical_path,
        OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| StoreError::Open {
        path: path.to_path_buf(),
        source,
    })
}

/// Enables foreign-key enforcement and sets the connection's lock wait timeout.
fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

/// Switches the connection to write-ahead logging mode.
fn enable_wal(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    Ok(())
}

/// Reads the SQLite application identifier and user schema version.
fn database_identity(connection: &Connection) -> Result<(i32, i32), StoreError> {
    let application_id = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    Ok((application_id, user_version))
}

/// Creates and validates a new v3 schema, then records its database identity.
fn initialize_v3(connection: &mut Connection, path: &Path) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(SCHEMA)?;
    transaction.execute_batch(RECEIPT_SCHEMA)?;
    transaction.execute_batch(DERIVED_SCHEMA)?;
    validate_v3_schema(&transaction, path)?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

/// Migrates a validated v1 database to v2 without rewriting observation documents.
fn migrate_v1_to_v2(connection: &mut Connection, path: &Path) -> Result<(), StoreError> {
    let documents_before = observation_documents(connection)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(RECEIPT_SCHEMA)?;
    validate_v2_schema(&transaction, path)?;
    if observation_documents(&transaction)? != documents_before {
        return Err(StoreError::InvalidHarnessReceipt);
    }
    transaction.pragma_update(None, "user_version", RECEIPT_SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

/// Migrates a validated v2 database to v3 by adding the derived layer.
///
/// Additive only. No observation, receipt, or derived document is rewritten, because the schema
/// version is the one thing that may legitimately change while the stored bytes may not.
fn migrate_v2_to_v3(connection: &mut Connection, path: &Path) -> Result<(), StoreError> {
    let documents_before = observation_documents(connection)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(DERIVED_SCHEMA)?;
    validate_v3_schema(&transaction, path)?;
    if observation_documents(&transaction)? != documents_before {
        return Err(StoreError::InvalidDerivedSchema);
    }
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

/// Validates the complete v1 schema, append-only controls, and observation rows.
fn validate_v1(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    validate_schema(connection, path)?;
    validate_append_only_controls(connection, path)?;
    validate_observation_rows(connection)
}

/// Validates the v2 database identity and all v2 schema invariants.
fn validate_v2(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    let (application_id, user_version) = database_identity(connection)?;
    if application_id != APPLICATION_ID || user_version != RECEIPT_SCHEMA_VERSION {
        return Err(StoreError::UnexpectedDatabase {
            path: path.to_path_buf(),
            application_id,
            user_version,
        });
    }
    validate_v2_schema(connection, path)
}

/// Validates v2 observation and receipt structures, controls, and persisted rows.
fn validate_v2_schema(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    validate_schema(connection, path)?;
    validate_append_only_controls(connection, path)?;
    validate_observation_rows(connection)?;
    validate_receipt_schema(connection, path)?;
    validate_receipt_controls(connection, path)?;
    validate_receipt_identity_probe(connection)?;
    validate_receipt_rows(connection)
}

/// Validates the v3 database identity and every v3 schema invariant.
fn validate_v3(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    let (application_id, user_version) = database_identity(connection)?;
    if application_id != APPLICATION_ID || user_version != SCHEMA_VERSION {
        return Err(StoreError::UnexpectedDatabase {
            path: path.to_path_buf(),
            application_id,
            user_version,
        });
    }
    validate_v3_schema(connection, path)
}

/// Validates v3 structures, controls, and persisted rows across both layers.
///
/// The v2 invariants are re-asserted rather than assumed. The derived tables reference
/// `observations`, so a database that satisfies only the derived checks is not a v3 database.
fn validate_v3_schema(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    validate_v2_schema(connection, path)?;
    validate_derived_schema(connection, path)?;
    validate_derived_controls(connection, path)?;
    validate_derived_constraints(connection, path)?;
    validate_derived_rows(connection)
}

/// Validates the observation table, index, and exact append-only trigger definitions.
fn validate_schema(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    const REQUIRED_OBJECTS: [(&str, &str); 5] = [
        ("table", "observations"),
        ("index", "observations_observed_at_idx"),
        ("trigger", "observations_reject_update"),
        ("trigger", "observations_reject_delete"),
        ("trigger", "observations_reject_duplicate_insert"),
    ];

    for (object_type, object_name) in REQUIRED_OBJECTS {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2
             )",
            params![object_type, object_name],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::IncompleteSchema {
                path: path.to_path_buf(),
                object: object_name,
            });
        }
    }
    for (name, expected) in [
        (
            "observations_reject_update",
            "CREATE TRIGGER observations_reject_update
             BEFORE UPDATE ON observations
             BEGIN SELECT RAISE(ABORT, 'observations are append-only'); END",
        ),
        (
            "observations_reject_delete",
            "CREATE TRIGGER observations_reject_delete
             BEFORE DELETE ON observations
             BEGIN SELECT RAISE(ABORT, 'observations are append-only'); END",
        ),
        (
            "observations_reject_duplicate_insert",
            "CREATE TRIGGER observations_reject_duplicate_insert
             BEFORE INSERT ON observations
             WHEN EXISTS (SELECT 1 FROM observations WHERE id = NEW.id)
             BEGIN SELECT RAISE(ABORT, 'observations are append-only'); END",
        ),
    ] {
        validate_trigger_definition(connection, path, name, expected)?;
    }
    validate_observation_columns(connection, path)?;
    validate_observation_index(connection, path)?;
    Ok(())
}

/// Normalizes SQLite DDL for semantic comparison independent of formatting.
fn canonical_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
        .replace("if not exists ", "")
}

/// Verifies that a named trigger has the expected normalized definition.
fn validate_trigger_definition(
    connection: &Connection,
    path: &Path,
    name: &'static str,
    expected: &str,
) -> Result<(), StoreError> {
    let actual = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            [name],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if actual.as_deref().map(canonical_sql).as_deref() != Some(&canonical_sql(expected)) {
        return Err(StoreError::IncompleteSchema {
            path: path.to_path_buf(),
            object: name,
        });
    }
    Ok(())
}

/// Verifies the observation table's column order, types, nullability, and primary key.
fn validate_observation_columns(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    let mut statement = connection.prepare("PRAGMA table_info(observations)")?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, i32>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected = vec![
        ("id".to_owned(), "TEXT".to_owned(), 1, 1),
        ("observed_at".to_owned(), "TEXT".to_owned(), 1, 0),
        ("document".to_owned(), "TEXT".to_owned(), 1, 0),
    ];
    if columns != expected {
        return Err(StoreError::IncompleteSchema {
            path: path.to_path_buf(),
            object: "observations columns",
        });
    }
    Ok(())
}

/// Verifies the observation listing index and its descending column order.
fn validate_observation_index(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    let index_properties = connection
        .prepare("PRAGMA index_list(observations)")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, i32>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if !index_properties.iter().any(|(name, unique, partial)| {
        name == "observations_observed_at_idx" && *unique == 0 && *partial == 0
    }) {
        return Err(StoreError::IncompleteSchema {
            path: path.to_path_buf(),
            object: "observations_observed_at_idx properties",
        });
    }
    let mut statement = connection.prepare("PRAGMA index_xinfo(observations_observed_at_idx)")?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, i32>(5)?,
            ))
        })?
        .filter_map(|row| match row {
            Ok((name, descending, key)) if key != 0 => Some(Ok((name, descending))),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if columns
        != [
            (Some("observed_at".to_owned()), 1),
            (Some("id".to_owned()), 1),
        ]
    {
        return Err(StoreError::IncompleteSchema {
            path: path.to_path_buf(),
            object: "observations_observed_at_idx columns",
        });
    }
    Ok(())
}

/// Probes that observation updates, deletes, and replacement inserts are rejected.
fn validate_append_only_controls(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    const PROBE_ID: &str = "__evidra_append_only_probe__";

    connection.execute_batch("SAVEPOINT evidra_observation_control_probe")?;
    let probe_result = connection.execute(
        "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
        params![PROBE_ID, "1970-01-01T00:00:00+00:00", "{}"],
    );
    if probe_result.is_err() {
        connection.execute_batch(
            "ROLLBACK TO evidra_observation_control_probe;
             RELEASE evidra_observation_control_probe;",
        )?;
        probe_result?;
    }
    let update_blocked = connection
        .execute(
            "UPDATE observations SET observed_at = observed_at WHERE id = ?1",
            [PROBE_ID],
        )
        .is_err();
    let delete_blocked = connection
        .execute("DELETE FROM observations WHERE id = ?1", [PROBE_ID])
        .is_err();
    let replace_blocked = connection
        .execute(
            "INSERT OR REPLACE INTO observations (id, observed_at, document)
             VALUES (?1, ?2, ?3)",
            params![PROBE_ID, "1970-01-01T00:00:00+00:00", "{}"],
        )
        .is_err();
    connection.execute_batch(
        "ROLLBACK TO evidra_observation_control_probe;
         RELEASE evidra_observation_control_probe;",
    )?;

    for (operation, blocked) in [
        ("update", update_blocked),
        ("delete", delete_blocked),
        ("insert-or-replace", replace_blocked),
    ] {
        if !blocked {
            return Err(StoreError::IncompleteAppendOnlyControl {
                path: path.to_path_buf(),
                operation,
            });
        }
    }
    Ok(())
}

/// Verifies every stored observation's JSON, integrity digest, and indexed metadata.
fn validate_observation_rows(connection: &Connection) -> Result<(), StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, observed_at, document
         FROM observations
         ORDER BY observed_at DESC, id DESC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (stored_id, stored_observed_at, document) = row?;
        let observation: Observation =
            serde_json::from_str(&document).map_err(|_| StoreError::Serialization)?;
        if !observation.verify_integrity()? {
            return Err(StoreError::IntegrityMismatch {
                id: observation.id().to_string(),
            });
        }
        if stored_id != observation.id().to_string()
            || stored_observed_at != observation.observed_at().to_rfc3339()
        {
            return Err(StoreError::MetadataMismatch {
                id: observation.id().to_string(),
            });
        }
    }
    Ok(())
}

/// Loads observation identifiers and serialized documents in stable identifier order.
fn observation_documents(connection: &Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut statement = connection.prepare("SELECT id, document FROM observations ORDER BY id")?;
    let documents = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(documents)
}

/// Verifies the derived tables, indexes, and exact trigger definitions.
///
/// Trigger text is compared after normalization rather than trusted, because a trigger that exists
/// but has been redefined to do nothing is indistinguishable from a missing one by name alone.
fn validate_derived_schema(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    const REQUIRED_OBJECTS: [(&str, &str); 20] = [
        ("table", "derivations"),
        ("table", "derivation_facets"),
        ("table", "derivation_evidence"),
        ("table", "relationships"),
        ("index", "derivations_recorded_at_idx"),
        ("index", "derivations_supersedes_idx"),
        ("index", "derivation_facets_lookup_idx"),
        ("index", "derivation_evidence_target_idx"),
        ("index", "relationships_from_key_idx"),
        ("index", "relationships_to_key_idx"),
        ("index", "relationships_relation_idx"),
        ("trigger", "derivations_reject_update"),
        ("trigger", "derivations_reject_delete"),
        ("trigger", "derivations_reject_duplicate_insert"),
        ("trigger", "derivation_facets_reject_update"),
        ("trigger", "derivation_facets_reject_delete"),
        ("trigger", "derivation_facets_reject_duplicate_insert"),
        ("trigger", "derivation_evidence_reject_update"),
        ("trigger", "derivation_evidence_reject_delete"),
        ("trigger", "derivation_evidence_reject_duplicate_insert"),
    ];

    for (object_type, object_name) in REQUIRED_OBJECTS {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2
             )",
            params![object_type, object_name],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::IncompleteSchema {
                path: path.to_path_buf(),
                object: object_name,
            });
        }
    }

    for (name, expected) in [
        (
            "derivations_reject_update",
            "CREATE TRIGGER derivations_reject_update
             BEFORE UPDATE ON derivations
             BEGIN SELECT RAISE(ABORT, 'derivations are append-only'); END",
        ),
        (
            "derivations_reject_delete",
            "CREATE TRIGGER derivations_reject_delete
             BEFORE DELETE ON derivations
             BEGIN SELECT RAISE(ABORT, 'derivations are append-only'); END",
        ),
        (
            "derivations_reject_duplicate_insert",
            "CREATE TRIGGER derivations_reject_duplicate_insert
             BEFORE INSERT ON derivations
             WHEN EXISTS (SELECT 1 FROM derivations WHERE id = NEW.id)
             BEGIN SELECT RAISE(ABORT, 'derivations are append-only'); END",
        ),
        (
            "derivation_facets_reject_update",
            "CREATE TRIGGER derivation_facets_reject_update
             BEFORE UPDATE ON derivation_facets
             BEGIN SELECT RAISE(ABORT, 'derivation facets are append-only'); END",
        ),
        (
            "derivation_facets_reject_delete",
            "CREATE TRIGGER derivation_facets_reject_delete
             BEFORE DELETE ON derivation_facets
             BEGIN SELECT RAISE(ABORT, 'derivation facets are append-only'); END",
        ),
        (
            "derivation_facets_reject_duplicate_insert",
            "CREATE TRIGGER derivation_facets_reject_duplicate_insert
             BEFORE INSERT ON derivation_facets
             WHEN EXISTS (
                 SELECT 1 FROM derivation_facets
                 WHERE derivation_id = NEW.derivation_id
                   AND namespace = NEW.namespace
                   AND name = NEW.name
             )
             BEGIN SELECT RAISE(ABORT, 'derivation facets are append-only'); END",
        ),
        (
            "derivation_evidence_reject_update",
            "CREATE TRIGGER derivation_evidence_reject_update
             BEFORE UPDATE ON derivation_evidence
             BEGIN SELECT RAISE(ABORT, 'derivation evidence is append-only'); END",
        ),
        (
            "derivation_evidence_reject_delete",
            "CREATE TRIGGER derivation_evidence_reject_delete
             BEFORE DELETE ON derivation_evidence
             BEGIN SELECT RAISE(ABORT, 'derivation evidence is append-only'); END",
        ),
        (
            "derivation_evidence_reject_duplicate_insert",
            "CREATE TRIGGER derivation_evidence_reject_duplicate_insert
             BEFORE INSERT ON derivation_evidence
             WHEN EXISTS (
                 SELECT 1 FROM derivation_evidence
                 WHERE derivation_id = NEW.derivation_id AND target_key = NEW.target_key
             )
             BEGIN SELECT RAISE(ABORT, 'derivation evidence is append-only'); END",
        ),
        (
            "relationships_reject_update",
            "CREATE TRIGGER relationships_reject_update
             BEFORE UPDATE ON relationships
             BEGIN SELECT RAISE(ABORT, 'relationships are append-only'); END",
        ),
        (
            "relationships_reject_delete",
            "CREATE TRIGGER relationships_reject_delete
             BEFORE DELETE ON relationships
             BEGIN SELECT RAISE(ABORT, 'relationships are append-only'); END",
        ),
        (
            "relationships_reject_duplicate_insert",
            "CREATE TRIGGER relationships_reject_duplicate_insert
             BEFORE INSERT ON relationships
             WHEN EXISTS (SELECT 1 FROM relationships WHERE id = NEW.id)
             BEGIN SELECT RAISE(ABORT, 'relationships are append-only'); END",
        ),
    ] {
        validate_trigger_definition(connection, path, name, expected)?;
    }

    validate_derived_columns(connection, path)?;
    Ok(())
}

/// Verifies the column order, types, nullability, and keys of the derived tables.
fn validate_derived_columns(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    const DERIVATIONS: [(&str, i32, i32); 5] = [
        ("id", 1, 1),
        ("kind", 1, 0),
        ("recorded_at", 1, 0),
        ("supersedes", 0, 0),
        ("document", 1, 0),
    ];
    const DERIVATION_FACETS: [(&str, i32, i32); 10] = [
        ("derivation_id", 1, 1),
        ("namespace", 1, 2),
        ("name", 1, 3),
        ("slot", 1, 0),
        ("text_value", 0, 0),
        ("integer_value", 0, 0),
        ("boolean_value", 0, 0),
        ("confidence", 1, 0),
        ("freshness", 1, 0),
        ("recorded_at", 1, 0),
    ];
    const DERIVATION_EVIDENCE: [(&str, i32, i32); 4] = [
        ("derivation_id", 1, 1),
        ("target_key", 1, 2),
        ("role", 1, 0),
        ("recorded_at", 1, 0),
    ];
    const RELATIONSHIPS: [(&str, i32, i32); 7] = [
        ("id", 1, 1),
        ("from_key", 1, 0),
        ("to_key", 1, 0),
        ("relation", 1, 0),
        ("confidence", 1, 0),
        ("recorded_at", 1, 0),
        ("document", 1, 0),
    ];

    for (table, columns_label, expected) in [
        ("derivations", "derivations columns", &DERIVATIONS[..]),
        (
            "derivation_facets",
            "derivation facets columns",
            &DERIVATION_FACETS[..],
        ),
        (
            "derivation_evidence",
            "derivation evidence columns",
            &DERIVATION_EVIDENCE[..],
        ),
        ("relationships", "relationships columns", &RELATIONSHIPS[..]),
    ] {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(3)?,
                    row.get::<_, i32>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let expected = expected
            .iter()
            .map(|(name, not_null, primary_key)| (String::from(*name), *not_null, *primary_key))
            .collect::<Vec<_>>();
        if columns != expected {
            return Err(StoreError::IncompleteSchema {
                path: path.to_path_buf(),
                object: columns_label,
            });
        }
    }
    Ok(())
}

/// Probes that every derived table rejects update, delete, replacement, and duplicate insert.
///
/// Also probes the foreign key from the two child tables to `derivations`, because a facet or
/// evidence row pointing at a derivation that does not exist would silently corrupt every
/// current-view resolution that walks those rows.
///
/// `relationships` gets no such foreign key, and that is deliberate rather than an omission: its
/// `from_key` and `to_key` are strings that may name either an observation or a derivation, so no
/// single SQL foreign key can express the union. Referential integrity for edges is checked by
/// [`validate_derived_rows`] walking both ends instead.
fn validate_derived_controls(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    const PROBE_DERIVATION: &str = "__evidra_derived_probe__";
    const PROBE_RELATIONSHIP: &str = "__evidra_relationship_probe__";
    const PROBE_TARGET: &str = "observation:__evidra_derived_probe__";

    connection.execute_batch("SAVEPOINT evidra_derived_control_probe")?;
    let result = (|| -> Result<Vec<(&'static str, bool)>, StoreError> {
        connection.execute(
            "INSERT INTO derivations (id, kind, recorded_at, supersedes, document)
             VALUES (?1, 'facet', '1970-01-01T00:00:00+00:00', NULL, '{}')",
            [PROBE_DERIVATION],
        )?;
        connection.execute(
            "INSERT INTO derivation_facets
             (derivation_id, namespace, name, slot, text_value, integer_value, boolean_value,
              confidence, freshness, recorded_at)
             VALUES (?1, 'probe', 'probe', 'text', 'probe', NULL, NULL,
                     'weak', 'current', '1970-01-01T00:00:00+00:00')",
            [PROBE_DERIVATION],
        )?;
        connection.execute(
            "INSERT INTO derivation_evidence (derivation_id, target_key, role, recorded_at)
             VALUES (?1, ?2, 'supporting', '1970-01-01T00:00:00+00:00')",
            params![PROBE_DERIVATION, PROBE_TARGET],
        )?;
        connection.execute(
            "INSERT INTO relationships
             (id, from_key, to_key, relation, confidence, recorded_at, document)
             VALUES (?1, ?2, 'derivation:other', 'supports', 'weak',
                     '1970-01-01T00:00:00+00:00', '{}')",
            params![PROBE_RELATIONSHIP, PROBE_TARGET],
        )?;

        let mut checks = Vec::new();
        for (operation, table, key) in [
            ("derivations update", "derivations", "id = ?1"),
            ("derivations delete", "derivations", "id = ?1"),
            (
                "derivation facets update",
                "derivation_facets",
                "derivation_id = ?1",
            ),
            (
                "derivation facets delete",
                "derivation_facets",
                "derivation_id = ?1",
            ),
            (
                "derivation evidence update",
                "derivation_evidence",
                "derivation_id = ?1",
            ),
            (
                "derivation evidence delete",
                "derivation_evidence",
                "derivation_id = ?1",
            ),
            ("relationships update", "relationships", "id = ?1"),
            ("relationships delete", "relationships", "id = ?1"),
        ] {
            let probe_id = if table == "relationships" {
                PROBE_RELATIONSHIP
            } else {
                PROBE_DERIVATION
            };
            let statement = if operation.ends_with("delete") {
                format!("DELETE FROM {table} WHERE {key}")
            } else {
                format!("UPDATE {table} SET recorded_at = recorded_at WHERE {key}")
            };
            checks.push((
                operation,
                connection.execute(&statement, [probe_id]).is_err(),
            ));
        }

        checks.push((
            "derivations insert-or-replace",
            connection
                .execute(
                    "INSERT OR REPLACE INTO derivations
                     (id, kind, recorded_at, supersedes, document)
                     VALUES (?1, 'facet', '1970-01-01T00:00:00+00:00', NULL, '{}')",
                    [PROBE_DERIVATION],
                )
                .is_err(),
        ));
        checks.push((
            "derivations duplicate insert",
            connection
                .execute(
                    "INSERT INTO derivations
                     (id, kind, recorded_at, supersedes, document)
                     VALUES (?1, 'facet', '1970-01-01T00:00:00+00:00', NULL, '{}')",
                    [PROBE_DERIVATION],
                )
                .is_err(),
        ));
        checks.push((
            "derivation facets duplicate insert",
            connection
                .execute(
                    "INSERT INTO derivation_facets
                     (derivation_id, namespace, name, slot, text_value, integer_value,
                      boolean_value, confidence, freshness, recorded_at)
                     VALUES (?1, 'probe', 'probe', 'text', 'other', NULL, NULL,
                             'weak', 'current', '1970-01-01T00:00:00+00:00')",
                    [PROBE_DERIVATION],
                )
                .is_err(),
        ));
        checks.push((
            "derivation evidence duplicate insert",
            connection
                .execute(
                    "INSERT INTO derivation_evidence (derivation_id, target_key, role, recorded_at)
                     VALUES (?1, ?2, 'refuting', '1970-01-01T00:00:00+00:00')",
                    params![PROBE_DERIVATION, PROBE_TARGET],
                )
                .is_err(),
        ));
        checks.push((
            "derivation evidence orphan insert",
            connection
                .execute(
                    "INSERT INTO derivation_evidence (derivation_id, target_key, role, recorded_at)
                     VALUES ('__evidra_missing_derivation__', ?1, 'refuting',
                             '1970-01-01T00:00:00+00:00')",
                    [PROBE_TARGET],
                )
                .is_err(),
        ));
        checks.push((
            "derivation facets orphan insert",
            connection
                .execute(
                    "INSERT INTO derivation_facets
                     (derivation_id, namespace, name, slot, text_value, integer_value,
                      boolean_value, confidence, freshness, recorded_at)
                     VALUES ('__evidra_missing_derivation__', 'probe', 'orphan', 'text',
                             'probe', NULL, NULL, 'weak', 'current',
                             '1970-01-01T00:00:00+00:00')",
                    [],
                )
                .is_err(),
        ));
        checks.push((
            "relationships duplicate insert",
            connection
                .execute(
                    "INSERT INTO relationships
                     (id, from_key, to_key, relation, confidence, recorded_at, document)
                     VALUES (?1, ?2, 'derivation:other', 'supports', 'weak',
                             '1970-01-01T00:00:00+00:00', '{}')",
                    params![PROBE_RELATIONSHIP, PROBE_TARGET],
                )
                .is_err(),
        ));
        Ok(checks)
    })();
    connection.execute_batch(
        "ROLLBACK TO evidra_derived_control_probe;
         RELEASE evidra_derived_control_probe;",
    )?;
    let checks = result?;

    for (operation, blocked) in checks {
        if !blocked {
            return Err(StoreError::IncompleteAppendOnlyControl {
                path: path.to_path_buf(),
                operation,
            });
        }
    }
    Ok(())
}

/// Probes the two constraints that are stronger than the application layer.
///
/// The facet slot must agree with which value column is populated, and a relationship may not point
/// at itself. Both are refused by the domain too; asserting them here proves the database enforces
/// them even when a writer bypasses the constructor.
fn validate_derived_constraints(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    const PROBE_DERIVATION: &str = "__evidra_slot_probe__";
    const PROBE_RELATIONSHIP: &str = "__evidra_edge_probe__";

    connection.execute_batch("SAVEPOINT evidra_derived_constraint_probe")?;
    let result = (|| -> Result<[bool; 4], StoreError> {
        connection.execute(
            "INSERT INTO derivations (id, kind, recorded_at, supersedes, document)
             VALUES (?1, 'facet', '1970-01-01T00:00:00+00:00', NULL, '{}')",
            [PROBE_DERIVATION],
        )?;
        let slot_mismatch = connection
            .execute(
                "INSERT INTO derivation_facets
                 (derivation_id, namespace, name, slot, text_value, integer_value,
                  boolean_value, confidence, freshness, recorded_at)
                 VALUES (?1, 'probe', 'mismatch', 'text', NULL, 7, NULL,
                         'weak', 'current', '1970-01-01T00:00:00+00:00')",
                [PROBE_DERIVATION],
            )
            .is_err();
        let unknown_slot = connection
            .execute(
                "INSERT INTO derivation_facets
                 (derivation_id, namespace, name, slot, text_value, integer_value,
                  boolean_value, confidence, freshness, recorded_at)
                 VALUES (?1, 'probe', 'unknown', 'path', 'probe', NULL, NULL,
                         'weak', 'current', '1970-01-01T00:00:00+00:00')",
                [PROBE_DERIVATION],
            )
            .is_err();
        let self_edge = connection
            .execute(
                "INSERT INTO relationships
                 (id, from_key, to_key, relation, confidence, recorded_at, document)
                 VALUES (?1, 'derivation:self', 'derivation:self', 'supports', 'weak',
                         '1970-01-01T00:00:00+00:00', '{}')",
                [PROBE_RELATIONSHIP],
            )
            .is_err();
        let unknown_relation = connection
            .execute(
                "INSERT INTO relationships
                 (id, from_key, to_key, relation, confidence, recorded_at, document)
                 VALUES (?1, 'derivation:a', 'derivation:b', 'implies', 'weak',
                         '1970-01-01T00:00:00+00:00', '{}')",
                [PROBE_RELATIONSHIP],
            )
            .is_err();
        Ok([slot_mismatch, unknown_slot, self_edge, unknown_relation])
    })();
    connection.execute_batch(
        "ROLLBACK TO evidra_derived_constraint_probe;
         RELEASE evidra_derived_constraint_probe;",
    )?;

    let checks = result?;

    for (operation, blocked) in [
        ("facet slot disagreement", checks[0]),
        ("unknown facet slot", checks[1]),
        ("self-referential relationship", checks[2]),
        ("unknown relation kind", checks[3]),
    ] {
        if !blocked {
            return Err(StoreError::IncompleteDerivedConstraint {
                path: path.to_path_buf(),
                constraint: operation,
            });
        }
    }
    Ok(())
}

/// Verifies every persisted derived record against its own serialized document.
///
/// Deserializing through the domain type re-runs the constructor invariants, so a stored record that
/// could never have been constructed is rejected here too. The indexed facets are then compared
/// against the document's facets, which is the check that matters most: a stale projection would
/// otherwise answer current-view queries with a value the record no longer claims.
fn validate_derived_rows(connection: &Connection) -> Result<(), StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, kind, recorded_at, supersedes, document
         FROM derivations
         ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    for (id, kind, recorded_at, supersedes, document) in rows {
        let derivation: Derivation = serde_json::from_str(&document)
            .map_err(|_| StoreError::InvalidDerivedRecord { id: id.clone() })?;
        if derivation.kind().as_str() != kind
            || derivation.recorded_at().to_rfc3339() != recorded_at
            || derivation.supersedes().map(|id| id.as_str()) != supersedes
        {
            return Err(StoreError::DerivedMetadataMismatch { id: id.clone() });
        }
        validate_projected_facets(connection, &derivation)?;
    }

    let mut statement = connection.prepare(
        "SELECT id, from_key, to_key, relation, confidence, recorded_at, document
         FROM relationships
         ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    for (id, from_key, to_key, relation, confidence, recorded_at, document) in rows {
        let relationship: Relationship = serde_json::from_str(&document)
            .map_err(|_| StoreError::InvalidDerivedRecord { id: id.clone() })?;
        if relationship.from().key() != from_key
            || relationship.to().key() != to_key
            || relationship.relation().as_str() != relation
            || relationship.profile().confidence().as_str() != confidence
            || relationship.recorded_at().to_rfc3339() != recorded_at
        {
            return Err(StoreError::DerivedMetadataMismatch { id });
        }
    }

    Ok(())
}

/// Confirms the indexed facet rows agree with the facets recorded on the derivation.
fn validate_projected_facets(
    connection: &Connection,
    derivation: &Derivation,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(
        "SELECT namespace, name, slot, text_value, integer_value, boolean_value,
                confidence, freshness, recorded_at
         FROM derivation_facets
         WHERE derivation_id = ?1
         ORDER BY namespace, name",
    )?;
    let projected = statement
        .query_map([derivation.id().as_str()], |row| {
            Ok(FacetRow {
                namespace: row.get(0)?,
                name: row.get(1)?,
                slot: row.get(2)?,
                text_value: row.get(3)?,
                integer_value: row.get(4)?,
                boolean_value: row.get(5)?,
                confidence: row.get(6)?,
                freshness: row.get(7)?,
                recorded_at: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let profile = derivation.profile();
    let mut recorded = derivation
        .facets()
        .iter()
        .map(|facet| {
            let (text_value, integer_value, boolean_value) = match facet.value() {
                FacetValue::Text(text) => (Some(text.clone()), None, None),
                FacetValue::Integer(number) => (None, Some(*number), None),
                FacetValue::Boolean(flag) => (None, None, Some(i64::from(*flag))),
            };
            FacetRow {
                namespace: facet.namespace().to_owned(),
                name: facet.name().to_owned(),
                slot: facet.slot().as_str().to_owned(),
                text_value,
                integer_value,
                boolean_value,
                confidence: profile.confidence().as_str().to_owned(),
                freshness: profile.freshness().as_str().to_owned(),
                recorded_at: derivation.recorded_at().to_rfc3339(),
            }
        })
        .collect::<Vec<_>>();
    recorded
        .sort_by(|left, right| (&left.namespace, &left.name).cmp(&(&right.namespace, &right.name)));

    if projected != recorded {
        return Err(StoreError::DerivedFacetMismatch {
            id: derivation.id().as_str(),
        });
    }
    Ok(())
}

/// One projected facet row, as read from storage or rebuilt from the record, for exact comparison.
///
/// Comparing whole rows rather than spot-checking columns is what makes a stale projection
/// detectable: a writer that updates one column of one row is caught by the same equality check
/// that catches a missing row.
#[derive(Debug, PartialEq, Eq)]
struct FacetRow {
    namespace: String,
    name: String,
    slot: String,
    text_value: Option<String>,
    integer_value: Option<i64>,
    boolean_value: Option<i64>,
    confidence: String,
    freshness: String,
    recorded_at: String,
}

/// Verifies the receipt table, indexes, foreign key, and exact trigger definitions.
fn validate_receipt_schema(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    for trigger in [
        "agent_harness_receipts_reject_update",
        "agent_harness_receipts_reject_delete",
        "agent_harness_receipts_reject_duplicate_insert",
    ] {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1)",
            [trigger],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::IncompleteSchema {
                path: path.to_path_buf(),
                object: "agent harness receipt trigger",
            });
        }
    }
    for (name, expected) in [
        (
            "agent_harness_receipts_reject_update",
            "CREATE TRIGGER agent_harness_receipts_reject_update
             BEFORE UPDATE ON agent_harness_receipts
             BEGIN SELECT RAISE(ABORT, 'agent harness receipts are append-only'); END",
        ),
        (
            "agent_harness_receipts_reject_delete",
            "CREATE TRIGGER agent_harness_receipts_reject_delete
             BEFORE DELETE ON agent_harness_receipts
             BEGIN SELECT RAISE(ABORT, 'agent harness receipts are append-only'); END",
        ),
        (
            "agent_harness_receipts_reject_duplicate_insert",
            "CREATE TRIGGER agent_harness_receipts_reject_duplicate_insert
             BEFORE INSERT ON agent_harness_receipts
             WHEN EXISTS (
                 SELECT 1 FROM agent_harness_receipts
                 WHERE harness = NEW.harness
                   AND session_id = NEW.session_id
                   AND source_event_id = NEW.source_event_id
             )
             OR EXISTS (
                 SELECT 1 FROM agent_harness_receipts
                 WHERE observation_id = NEW.observation_id
             )
             BEGIN SELECT RAISE(ABORT, 'agent harness receipts are append-only'); END",
        ),
    ] {
        validate_trigger_definition(connection, path, name, expected)?;
    }

    let mut statement = connection.prepare("PRAGMA table_info(agent_harness_receipts)")?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, i32>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected = vec![
        ("harness".to_owned(), "TEXT".to_owned(), 1, 1),
        ("session_id".to_owned(), "TEXT".to_owned(), 1, 2),
        ("source_event_id".to_owned(), "TEXT".to_owned(), 1, 3),
        ("event_digest".to_owned(), "TEXT".to_owned(), 1, 0),
        ("observation_id".to_owned(), "TEXT".to_owned(), 1, 0),
    ];
    if columns != expected {
        return Err(StoreError::IncompleteSchema {
            path: path.to_path_buf(),
            object: "agent_harness_receipts columns",
        });
    }

    let mut indexes = connection.prepare("PRAGMA index_list(agent_harness_receipts)")?;
    let unique_indexes = indexes
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)? != 0,
                row.get::<_, i32>(4)? != 0,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut observation_unique = false;
    for (name, unique, partial) in unique_indexes {
        if !unique || partial {
            continue;
        }
        let sql = format!("PRAGMA index_info('{name}')");
        let mut info = connection.prepare(&sql)?;
        let indexed = info
            .query_map([], |row| row.get::<_, String>(2))?
            .collect::<Result<Vec<_>, _>>()?;
        observation_unique |= indexed == ["observation_id"];
    }
    if !observation_unique {
        return Err(StoreError::IncompleteSchema {
            path: path.to_path_buf(),
            object: "agent_harness_receipts observation_id uniqueness",
        });
    }

    let foreign_keys = connection
        .prepare("PRAGMA foreign_key_list(agent_harness_receipts)")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if foreign_keys
        != [(
            "observations".to_owned(),
            "observation_id".to_owned(),
            "id".to_owned(),
        )]
    {
        return Err(StoreError::IncompleteSchema {
            path: path.to_path_buf(),
            object: "agent_harness_receipts foreign key",
        });
    }
    let foreign_keys_enabled: bool =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    let foreign_key_violation = connection
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    if !foreign_keys_enabled || foreign_key_violation {
        return Err(StoreError::IncompleteSchema {
            path: path.to_path_buf(),
            object: "foreign key enforcement",
        });
    }
    Ok(())
}

/// Probes receipt immutability, uniqueness, and foreign-key enforcement.
fn validate_receipt_controls(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    const PROBE_ID: &str = "__evidra_receipt_probe__";
    connection.execute_batch("SAVEPOINT evidra_receipt_control_probe")?;
    let result = (|| -> Result<[bool; 6], StoreError> {
        connection.execute(
            "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
            params![PROBE_ID, "1970-01-01T00:00:00+00:00", "{}"],
        )?;
        connection.execute(
            "INSERT INTO agent_harness_receipts
             (harness, session_id, source_event_id, event_digest, observation_id)
             VALUES ('probe', 'probe', 'probe', ?1, ?2)",
            params!["0".repeat(64), PROBE_ID],
        )?;
        let update = connection
            .execute(
                "UPDATE agent_harness_receipts SET harness = harness WHERE observation_id = ?1",
                [PROBE_ID],
            )
            .is_err();
        let delete = connection
            .execute(
                "DELETE FROM agent_harness_receipts WHERE observation_id = ?1",
                [PROBE_ID],
            )
            .is_err();
        let replace = connection
            .execute(
                "INSERT OR REPLACE INTO agent_harness_receipts
                 (harness, session_id, source_event_id, event_digest, observation_id)
                 VALUES ('probe', 'probe', 'probe', ?1, ?2)",
                params!["0".repeat(64), PROBE_ID],
            )
            .is_err();
        let duplicate = connection
            .execute(
                "INSERT INTO agent_harness_receipts
                 (harness, session_id, source_event_id, event_digest, observation_id)
                 VALUES ('probe', 'probe', 'probe', ?1, ?2)",
                params!["0".repeat(64), PROBE_ID],
            )
            .is_err();
        let replace_observation = connection
            .execute(
                "INSERT OR REPLACE INTO agent_harness_receipts
                 (harness, session_id, source_event_id, event_digest, observation_id)
                 VALUES ('different', 'different', 'different', ?1, ?2)",
                params!["0".repeat(64), PROBE_ID],
            )
            .is_err();
        let orphan = connection
            .execute(
                "INSERT INTO agent_harness_receipts
                 (harness, session_id, source_event_id, event_digest, observation_id)
                 VALUES ('orphan', 'orphan', 'orphan', ?1, 'missing')",
                ["0".repeat(64)],
            )
            .is_err();
        Ok([
            update,
            delete,
            replace,
            duplicate,
            replace_observation,
            orphan,
        ])
    })();
    connection.execute_batch(
        "ROLLBACK TO evidra_receipt_control_probe;
         RELEASE evidra_receipt_control_probe;",
    )?;
    let checks = result?;
    for (operation, blocked) in [
        "update",
        "delete",
        "insert-or-replace",
        "duplicate-insert",
        "observation-id-replace",
        "orphan-insert",
    ]
    .into_iter()
    .zip(checks)
    {
        if !blocked {
            return Err(StoreError::IncompleteAppendOnlyControl {
                path: path.to_path_buf(),
                operation,
            });
        }
    }
    Ok(())
}

/// Confirms that receipt validation detects identity fields that disagree with the payload.
fn validate_receipt_identity_probe(connection: &Connection) -> Result<(), StoreError> {
    let value = probe_harness_observation()?;
    connection.execute_batch("SAVEPOINT evidra_receipt_identity_probe")?;
    let result = (|| -> Result<bool, StoreError> {
        insert_observation(connection, value.observation())?;
        connection.execute(
            "INSERT INTO agent_harness_receipts
             (harness, session_id, source_event_id, event_digest, observation_id)
             VALUES ('mismatched-harness', ?1, ?2, ?3, ?4)",
            params![
                value.identity().session_id(),
                value.identity().source_event_id(),
                value.event_digest(),
                value.observation().id().to_string()
            ],
        )?;
        Ok(validate_receipt_rows(connection).is_err())
    })();
    connection.execute_batch(
        "ROLLBACK TO evidra_receipt_identity_probe;
         RELEASE evidra_receipt_identity_probe;",
    )?;
    if !result? {
        return Err(StoreError::InvalidHarnessReceipt);
    }
    Ok(())
}

/// Builds a valid synthetic harness observation for schema-control probes.
fn probe_harness_observation() -> Result<AgentHarnessObservation, StoreError> {
    let event = AgentHarnessEvent::new(AgentHarnessEventDraft {
        occurred_at: "1970-01-01T00:00:00Z".parse().map_err(invalid_receipt)?,
        source: SourceRef::new("agent-harness", "evidra-schema-probe").map_err(invalid_receipt)?,
        subject: SubjectRef::new("database", "evidra-schema-probe").map_err(invalid_receipt)?,
        source_event_id: SourceEventId::new("evidra-schema-probe").map_err(invalid_receipt)?,
        harness: HarnessRef::new("evidra-schema-probe", None).map_err(invalid_receipt)?,
        session_id: HarnessSessionId::new("evidra-schema-probe").map_err(invalid_receipt)?,
        event_type: HarnessEventType::new("schema-probe").map_err(invalid_receipt)?,
        redaction: RedactionRecord::new("schema-probe", "1", Vec::new())
            .map_err(invalid_receipt)?,
        excerpts: Vec::new(),
        facets: Vec::new(),
    })
    .map_err(invalid_receipt)?;
    AgentHarnessObservation::record(event, "evidra-store").map_err(invalid_receipt)
}

/// Maps any probe-construction failure to the store's receipt-validation error.
fn invalid_receipt<E>(_error: E) -> StoreError {
    StoreError::InvalidHarnessReceipt
}

/// Verifies all receipt identities and digests and rejects unreceipted harness observations.
fn validate_receipt_rows(connection: &Connection) -> Result<(), StoreError> {
    let mut statement = connection.prepare(
        "SELECT r.harness, r.session_id, r.source_event_id, r.event_digest, o.document
         FROM agent_harness_receipts r
         JOIN observations o ON o.id = r.observation_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (harness, session_id, source_event_id, event_digest, document) = row?;
        let observation: Observation =
            serde_json::from_str(&document).map_err(|_| StoreError::InvalidHarnessReceipt)?;
        let identity = AgentHarnessEventIdentity::new(harness, session_id, source_event_id)
            .map_err(|_| StoreError::InvalidHarnessReceipt)?;
        let valid = AgentHarnessObservation::verify_receipt(&identity, &event_digest, &observation)
            .map_err(|_| StoreError::InvalidHarnessReceipt)?;
        if !valid {
            return Err(StoreError::InvalidHarnessReceipt);
        }
    }
    let unreceipted_harness_observation: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1
            FROM observations o
            LEFT JOIN agent_harness_receipts r ON r.observation_id = o.id
            WHERE json_extract(o.document, '$.kind') = 'agent-harness-event'
              AND r.observation_id IS NULL
         )",
        [],
        |row| row.get(0),
    )?;
    if unreceipted_harness_observation {
        return Err(StoreError::InvalidHarnessReceipt);
    }
    Ok(())
}

/// Rejects symbolic links at the database path or any SQLite sidecar path.
fn reject_store_symlinks(path: &Path) -> Result<(), StoreError> {
    reject_symlink(path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        reject_symlink(&sidecar_path(path, suffix))?;
    }
    Ok(())
}

/// Appends a SQLite sidecar suffix to a database path without assuming UTF-8.
fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

/// Rejects symbolic links in the repository-local state directory and repository root.
fn reject_symlink_ancestors(path: &Path) -> Result<(), StoreError> {
    // For <repository>/.evidra/evidra.db, `path` is the .evidra directory. Inspect only that
    // directory and the repository root so platform aliases above the repository (for example,
    // macOS /var) do not cause a false rejection.
    for ancestor in path.ancestors().take(2) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(StoreError::SymlinkPath {
                    path: ancestor.to_path_buf(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(StoreError::InspectPath {
                    path: ancestor.to_path_buf(),
                    source,
                });
            }
        }
    }
    Ok(())
}

/// Rejects `path` when it exists as a symbolic link and permits a missing path.
fn reject_symlink(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StoreError::SymlinkPath {
            path: path.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StoreError::InspectPath {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
/// Restricts the store directory to mode 0700 and database files to mode 0600 on Unix.
fn restrict_store_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
            StoreError::SetPermissions {
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }
    for candidate in [
        path.to_path_buf(),
        sidecar_path(path, "-wal"),
        sidecar_path(path, "-shm"),
    ] {
        if candidate.exists() {
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).map_err(
                |source| StoreError::SetPermissions {
                    path: candidate.clone(),
                    source,
                },
            )?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
/// Leaves store permissions unchanged on platforms without Unix permission modes.
fn restrict_store_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(unix)]
/// Restricts a newly created directory to mode 0700 on Unix.
fn restrict_new_directory(path: &Path, existed: bool) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    if !existed {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            StoreError::SetPermissions {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
/// Leaves a newly created directory unchanged on platforms without Unix permission modes.
fn restrict_new_directory(_path: &Path, _existed: bool) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use evidra_core::{
        AgentHarnessEvent, AgentHarnessEventDraft, AgentHarnessObservation, ConfidenceBand,
        Derivation, DerivationDraft, DerivationId, DerivationKind, DerivationMethod,
        DerivationScope, EvidenceTarget, FacetProjection, FacetValue, HarnessEventType, HarnessRef,
        HarnessSessionId, Observation, ObservationDraft, ObservationFacet, ObservationKind,
        ObservationStore, Provenance, RedactedExcerpt, RedactionRecord, RelationKind, Relationship,
        RelationshipId, SourceEventId, SourceRef, SubjectRef, UncertaintyProfile,
    };
    use serde_json::json;
    use tempfile::TempDir;

    use super::{APPLICATION_ID, RECEIPT_SCHEMA, SCHEMA, SqliteObservationStore, StoreError};

    /// A fixed instant so derived fixtures are reproducible across runs.
    const DERIVED_AT: &str = "2026-10-02T09:00:00Z";

    /// Builds a valid facet derivation carrying one projection of each slot type.
    fn facet_derivation() -> Derivation {
        let recorded_at = DERIVED_AT.parse().expect("fixture instant should parse");
        let subject = SubjectRef::new("repository", "/workspace").expect("subject should be valid");
        let scope = DerivationScope::new(
            subject,
            recorded_at,
            recorded_at + chrono::Duration::days(1),
            Vec::new(),
        )
        .expect("window should be ordered");

        Derivation::new(DerivationDraft {
            id: DerivationId::new(),
            kind: DerivationKind::Facet,
            scope,
            profile: UncertaintyProfile::deterministic().with_confidence(ConfidenceBand::Moderate),
            method: DerivationMethod::Deterministic {
                version: "engine-0.1.0".into(),
            },
            facets: vec![
                FacetProjection::new("friction", "category", FacetValue::Text("auth".into()))
                    .expect("projection should be valid"),
                FacetProjection::new("latency", "count", FacetValue::Integer(7))
                    .expect("projection should be valid"),
                FacetProjection::new("retry", "exhausted", FacetValue::Boolean(true))
                    .expect("projection should be valid"),
            ],
            recorded_at,
            supersedes: None,
        })
        .expect("derivation should be valid")
    }

    /// Builds a valid relationship between two derivations.
    fn edge_relationship(from: DerivationId, to: DerivationId) -> Relationship {
        let recorded_at = DERIVED_AT.parse().expect("fixture instant should parse");
        Relationship::new(
            RelationshipId::new(),
            EvidenceTarget::Derivation(from),
            EvidenceTarget::Derivation(to),
            RelationKind::Supports,
            UncertaintyProfile::deterministic().with_confidence(ConfidenceBand::Weak),
            DerivationMethod::Deterministic {
                version: "engine-0.1.0".into(),
            },
            recorded_at,
        )
        .expect("relationship should be valid")
    }

    /// Writes a derivation and its indexed projections exactly as the store will.
    ///
    /// Nothing writes derived rows yet, so without this the row validator would pass vacuously and
    /// prove nothing. `corrupt_facet_value` exists so a test can introduce projection drift and
    /// confirm validation catches it rather than rubber-stamping whatever is on disk.
    fn insert_derivation(
        connection: &rusqlite::Connection,
        derivation: &Derivation,
        corrupt_facet_value: bool,
    ) {
        let recorded_at = derivation.recorded_at().to_rfc3339();
        let document = serde_json::to_string(derivation).expect("derivation should serialize");
        connection
            .execute(
                "INSERT INTO derivations (id, kind, recorded_at, supersedes, document)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    derivation.id().as_str(),
                    derivation.kind().as_str(),
                    recorded_at,
                    derivation.supersedes().map(|id| id.as_str()),
                    document
                ],
            )
            .expect("derivation should insert");

        let profile = derivation.profile();
        for facet in derivation.facets() {
            let (text_value, integer_value, boolean_value) = if corrupt_facet_value {
                (Some("drifted".to_owned()), None, None)
            } else {
                match facet.value() {
                    FacetValue::Text(text) => (Some(text.clone()), None, None),
                    FacetValue::Integer(number) => (None, Some(*number), None),
                    FacetValue::Boolean(flag) => (None, None, Some(i64::from(*flag))),
                }
            };
            connection
                .execute(
                    "INSERT INTO derivation_facets
                     (derivation_id, namespace, name, slot, text_value, integer_value,
                      boolean_value, confidence, freshness, recorded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        derivation.id().as_str(),
                        facet.namespace(),
                        facet.name(),
                        if corrupt_facet_value {
                            "text".to_owned()
                        } else {
                            facet.slot().as_str().to_owned()
                        },
                        text_value,
                        integer_value,
                        boolean_value,
                        profile.confidence().as_str(),
                        profile.freshness().as_str(),
                        recorded_at
                    ],
                )
                .expect("facet row should insert");
        }
    }

    fn insert_relationship(connection: &rusqlite::Connection, relationship: &Relationship) {
        connection
            .execute(
                "INSERT INTO relationships
                 (id, from_key, to_key, relation, confidence, recorded_at, document)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    relationship.id().as_str(),
                    relationship.from().key(),
                    relationship.to().key(),
                    relationship.relation().as_str(),
                    relationship.profile().confidence().as_str(),
                    relationship.recorded_at().to_rfc3339(),
                    serde_json::to_string(relationship).expect("relationship should serialize")
                ],
            )
            .expect("relationship should insert");
    }

    /// Reopens a store after mutating the file underneath it, so validation runs over the result.
    fn reopen_after_mutation(
        database: &std::path::Path,
        mutate: impl FnOnce(&rusqlite::Connection),
    ) -> Result<SqliteObservationStore, StoreError> {
        drop(SqliteObservationStore::initialize(database).expect("store should initialize"));
        let connection = rusqlite::Connection::open(database).expect("database should open");
        mutate(&connection);
        drop(connection);
        SqliteObservationStore::open(database)
    }

    /// Builds a valid immutable observation fixture.
    fn observation() -> Observation {
        Observation::record(ObservationDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("manual", "cli").expect("source should be valid"),
            kind: ObservationKind::ManualIntervention,
            subject: SubjectRef::new("repository", "/tmp/example")
                .expect("subject should be valid"),
            payload: json!({"summary": "Removed stale generated artifacts"}),
            provenance: Provenance::direct("evidra-cli").expect("provenance should be valid"),
        })
        .expect("observation should be valid")
    }

    /// Builds a valid harness-observation fixture.
    fn harness_observation() -> AgentHarnessObservation {
        harness_observation_with_result("passed")
    }

    /// Builds a harness-observation fixture with the requested verification result.
    fn harness_observation_with_result(result: &str) -> AgentHarnessObservation {
        let event = AgentHarnessEvent::new(AgentHarnessEventDraft {
            occurred_at: "2026-09-19T08:00:00Z"
                .parse()
                .expect("timestamp should be valid"),
            source: SourceRef::new("agent-harness", "session.jsonl#event-17")
                .expect("source should be valid"),
            subject: SubjectRef::new("repository", "/tmp/example")
                .expect("subject should be valid"),
            source_event_id: SourceEventId::new("event-17")
                .expect("source event ID should be valid"),
            harness: HarnessRef::new("claude-code", Some("1.0".to_owned()))
                .expect("harness should be valid"),
            session_id: HarnessSessionId::new("session-1").expect("session should be valid"),
            event_type: HarnessEventType::new("tool-completed")
                .expect("event type should be valid"),
            redaction: RedactionRecord::new("obfsck", "1", vec!["secret-redaction".to_owned()])
                .expect("redaction should be valid"),
            excerpts: vec![
                RedactedExcerpt::new("tool-output", "tests passed")
                    .expect("excerpt should be valid"),
            ],
            facets: vec![
                ObservationFacet::new("verification.result", FacetValue::Text(result.to_owned()))
                    .expect("facet should be valid"),
            ],
        })
        .expect("event should be valid");
        AgentHarnessObservation::record(event, "evidra-cli")
            .expect("harness observation should record")
    }

    /// Creates a valid v1 database, optionally containing one observation.
    fn create_v1_database(path: &std::path::Path, existing: Option<&Observation>) {
        let connection = rusqlite::Connection::open(path).expect("v1 database should open");
        connection
            .execute_batch(SCHEMA)
            .expect("v1 schema should apply");
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .expect("application ID should be set");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("v1 version should be set");
        if let Some(observation) = existing {
            connection
                .execute(
                    "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        observation.id().to_string(),
                        observation.observed_at().to_rfc3339(),
                        serde_json::to_string(observation).expect("observation should serialize")
                    ],
                )
                .expect("v1 observation should insert");
        }
    }

    /// Creates a valid v2 database, optionally containing one observation.
    ///
    /// v2 is the observation ledger plus harness receipts and no derived layer, which is exactly
    /// the state this migration has to accept as input.
    fn create_v2_database(path: &std::path::Path, existing: Option<&Observation>) {
        create_v1_database(path, existing);
        let connection = rusqlite::Connection::open(path).expect("v2 database should open");
        connection
            .execute_batch(RECEIPT_SCHEMA)
            .expect("receipt schema should apply");
        connection
            .pragma_update(None, "user_version", 2)
            .expect("v2 version should be set");
    }

    /// Captures non-internal schema objects for before-and-after comparisons.
    fn schema_snapshot(path: &std::path::Path) -> Vec<(String, String, Option<String>)> {
        let connection = rusqlite::Connection::open(path).expect("database should open");
        let mut statement = connection
            .prepare(
                "SELECT type, name, sql FROM sqlite_master
                 WHERE name NOT LIKE 'sqlite_%'
                 ORDER BY type, name",
            )
            .expect("schema query should prepare");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("schema query should run")
            .collect::<Result<Vec<_>, _>>()
            .expect("schema rows should decode")
    }

    #[test]
    /// Round-trips an appended observation through reverse-chronological listing.
    fn append_then_list_round_trips_observation() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("nested/evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        let expected = observation();

        store.append(&expected).expect("append should succeed");
        let actual = store.list(10).expect("list should succeed");

        assert_eq!(actual, vec![expected]);
    }

    #[test]
    /// Rejects a second append of the same observation identity.
    fn duplicate_observation_is_rejected() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        let observation = observation();

        store
            .append(&observation)
            .expect("first append should succeed");
        let error = store
            .append(&observation)
            .expect_err("duplicate append should fail");

        assert!(matches!(error, StoreError::DuplicateObservation { .. }));
    }

    #[test]
    /// Rejects harness events sent through the generic append path without a receipt.
    fn generic_append_rejects_unreceipted_harness_kind() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        let observation = Observation::record(ObservationDraft {
            occurred_at: Utc::now(),
            source: SourceRef::new("agent-harness", "session.jsonl#event-17")
                .expect("source should be valid"),
            kind: ObservationKind::AgentHarnessEvent,
            subject: SubjectRef::new("repository", "/tmp/example")
                .expect("subject should be valid"),
            payload: json!({"schema": "evidra.agent-harness-observation/v1"}),
            provenance: Provenance::transformed(
                "evidra-cli",
                vec!["agent-harness-observation/v1".to_owned()],
            )
            .expect("provenance should be valid"),
        })
        .expect("observation should be valid");

        let error = store
            .append(&observation)
            .expect_err("generic append should reject harness observations");

        assert!(matches!(
            error,
            StoreError::HarnessObservationRequiresReceipt
        ));
    }

    #[test]
    /// Records exactly one observation and one receipt for a harness append.
    fn harness_append_records_observation_and_receipt_atomically() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        let value = harness_observation();

        let outcome = store
            .append_harness_observation(&value)
            .expect("harness append should succeed");
        let observation_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
            .expect("observation count should load");
        let receipt_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM agent_harness_receipts", [], |row| {
                row.get(0)
            })
            .expect("receipt count should load");

        assert_eq!(outcome, evidra_core::AgentHarnessAppendOutcome::Recorded);
        assert_eq!(observation_count, 1);
        assert_eq!(receipt_count, 1);
    }

    #[test]
    /// Classifies a repeated harness identity with the same digest as a duplicate.
    fn equal_identity_and_digest_returns_duplicate() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        let value = harness_observation();
        store
            .append_harness_observation(&value)
            .expect("first append should succeed");

        let outcome = store
            .append_harness_observation(&value)
            .expect("duplicate should resolve");

        assert_eq!(outcome, evidra_core::AgentHarnessAppendOutcome::Duplicate);
        assert_eq!(store.list(10).expect("list should succeed").len(), 1);
    }

    #[test]
    /// Classifies changed content under an existing harness identity as a conflict.
    fn equal_identity_with_changed_content_returns_identity_conflict() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        let first = harness_observation_with_result("passed");
        let changed = harness_observation_with_result("failed");
        store
            .append_harness_observation(&first)
            .expect("first append should succeed");

        let outcome = store
            .append_harness_observation(&changed)
            .expect("conflict should resolve");

        assert_eq!(
            outcome,
            evidra_core::AgentHarnessAppendOutcome::IdentityConflict
        );
        assert_eq!(store.list(10).expect("list should succeed").len(), 1);
    }

    #[test]
    /// Serializes concurrent equal harness events into recorded and duplicate outcomes.
    fn concurrent_equal_events_resolve_recorded_then_duplicate() {
        use std::{sync::mpsc, time::Duration};

        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let value = harness_observation();
        let mut first_store =
            SqliteObservationStore::open(&database).expect("first store should open");
        let mut second_store =
            SqliteObservationStore::open(&database).expect("second store should open");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        first_store.append_pause = Some(super::AppendPause {
            entered: entered_tx,
            release: release_rx,
        });
        let first_value = value.clone();
        let first = std::thread::spawn(move || {
            first_store
                .append_harness_observation(&first_value)
                .expect("first append should resolve")
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first writer should hold immediate transaction");
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let outcome = second_store
                .append_harness_observation(&value)
                .expect("second append should resolve");
            second_done_tx
                .send(())
                .expect("second completion should send");
            outcome
        });
        assert!(matches!(
            second_done_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release_tx.send(()).expect("first writer should release");
        let outcomes = [
            first.join().expect("first writer should finish"),
            second.join().expect("second writer should finish"),
        ];

        assert!(outcomes.contains(&evidra_core::AgentHarnessAppendOutcome::Recorded));
        assert!(outcomes.contains(&evidra_core::AgentHarnessAppendOutcome::Duplicate));
        assert_eq!(
            SqliteObservationStore::open(&database)
                .expect("store should reopen")
                .list(10)
                .expect("list should succeed")
                .len(),
            1
        );
    }

    #[test]
    /// Serializes concurrent conflicting harness events into recorded and conflict outcomes.
    fn concurrent_conflicting_events_resolve_recorded_then_identity_conflict() {
        use std::{sync::mpsc, time::Duration};

        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let mut first_store =
            SqliteObservationStore::open(&database).expect("first store should open");
        let mut second_store =
            SqliteObservationStore::open(&database).expect("second store should open");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        first_store.append_pause = Some(super::AppendPause {
            entered: entered_tx,
            release: release_rx,
        });
        let first_value = harness_observation_with_result("passed");
        let second_value = harness_observation_with_result("failed");
        let first = std::thread::spawn(move || {
            first_store
                .append_harness_observation(&first_value)
                .expect("first append should resolve")
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first writer should hold immediate transaction");
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let outcome = second_store
                .append_harness_observation(&second_value)
                .expect("second append should resolve");
            second_done_tx
                .send(())
                .expect("second completion should send");
            outcome
        });
        assert!(matches!(
            second_done_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release_tx.send(()).expect("first writer should release");
        let outcomes = [
            first.join().expect("first writer should finish"),
            second.join().expect("second writer should finish"),
        ];

        assert!(outcomes.contains(&evidra_core::AgentHarnessAppendOutcome::Recorded));
        assert!(outcomes.contains(&evidra_core::AgentHarnessAppendOutcome::IdentityConflict));
        assert_eq!(
            SqliteObservationStore::open(&database)
                .expect("store should reopen")
                .list(10)
                .expect("list should succeed")
                .len(),
            1
        );
    }

    #[test]
    /// Blocks direct updates, deletes, and replacement inserts against observations.
    fn direct_mutation_is_rejected() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        let observation = observation();
        store.append(&observation).expect("append should succeed");
        drop(store);

        let connection = rusqlite::Connection::open(&database).expect("database should open");
        let update = connection.execute(
            "UPDATE observations SET observed_at = '2000-01-01T00:00:00Z'",
            [],
        );
        let delete = connection.execute("DELETE FROM observations", []);
        let replace = connection.execute(
            "INSERT OR REPLACE INTO observations (id, observed_at, document)
             SELECT id, observed_at, document FROM observations LIMIT 1",
            [],
        );

        assert!(update.is_err());
        assert!(delete.is_err());
        assert!(replace.is_err());
    }

    #[test]
    /// Rejects listed observations whose indexed timestamp differs from the document.
    fn list_rejects_inconsistent_indexed_metadata() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        store.append(&observation()).expect("append should succeed");
        drop(store);

        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch("DROP TRIGGER observations_reject_update;")
            .expect("test should remove update trigger");
        connection
            .execute(
                "UPDATE observations SET observed_at = '2000-01-01T00:00:00+00:00'",
                [],
            )
            .expect("test should alter indexed metadata");
        let store = SqliteObservationStore {
            connection,
            append_pause: None,
        };
        let error = store
            .list(10)
            .expect_err("inconsistent metadata should be rejected");

        assert!(matches!(error, StoreError::MetadataMismatch { .. }));
    }

    #[cfg(unix)]
    #[test]
    /// Rejects initialization when the database path is a symbolic link.
    fn initialize_rejects_symbolic_link_database() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let target = temp_dir.path().join("target.db");
        std::fs::write(&target, []).expect("target should be created");
        let database = temp_dir.path().join("evidra.db");
        symlink(&target, &database).expect("symbolic link should be created");

        let error = SqliteObservationStore::initialize(&database)
            .expect_err("symbolic link should be rejected");

        assert!(matches!(error, StoreError::SymlinkPath { .. }));
    }

    #[cfg(unix)]
    #[test]
    /// Rejects initialization when a SQLite sidecar path is a symbolic link.
    fn initialize_rejects_symbolic_link_sidecar() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let target = temp_dir.path().join("target.wal");
        std::fs::write(&target, []).expect("target should be created");
        let database = temp_dir.path().join("evidra.db");
        let wal = temp_dir.path().join("evidra.db-wal");
        symlink(&target, wal).expect("symbolic link should be created");

        let error = SqliteObservationStore::initialize(&database)
            .expect_err("symbolic-link sidecar should be rejected");

        assert!(matches!(error, StoreError::SymlinkPath { .. }));
    }

    #[test]
    /// Rejects initialization over an existing database without Evidra's identity.
    fn initialize_rejects_unrelated_database() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("unrelated.db");
        rusqlite::Connection::open(&database).expect("unrelated database should be created");

        let error = SqliteObservationStore::initialize(&database)
            .expect_err("unrelated database should be rejected");

        assert!(matches!(error, StoreError::UnexpectedDatabase { .. }));
    }

    #[test]
    /// Rejects opening a database missing an observation append-only trigger.
    fn open_rejects_missing_append_only_trigger() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let store = SqliteObservationStore::initialize(&database).expect("store should initialize");
        drop(store);
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch("DROP TRIGGER observations_reject_delete;")
            .expect("test should remove delete trigger");
        drop(connection);

        let error = SqliteObservationStore::open(&database)
            .expect_err("incomplete schema should be rejected");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rejects opening a database whose update trigger does not prevent mutation.
    fn open_rejects_ineffective_append_only_trigger() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let store = SqliteObservationStore::initialize(&database).expect("store should initialize");
        drop(store);
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP TRIGGER observations_reject_update;
                 CREATE TRIGGER observations_reject_update
                 BEFORE UPDATE ON observations BEGIN SELECT 1; END;",
            )
            .expect("test should replace update trigger");
        drop(connection);

        let error = SqliteObservationStore::open(&database)
            .expect_err("ineffective trigger should be rejected");

        assert!(matches!(
            error,
            StoreError::IncompleteSchema { .. } | StoreError::IncompleteAppendOnlyControl { .. }
        ));
    }

    #[test]
    /// Accepts a valid v1 database during migration preflight validation.
    fn valid_v1_database_passes_migration_preflight() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let expected = observation();
        create_v1_database(&database, Some(&expected));
        let connection = rusqlite::Connection::open(&database).expect("v1 database should open");
        super::configure_connection(&connection).expect("connection should configure");

        let preflight = super::validate_v1(&connection, &database);

        assert!(preflight.is_ok());
    }

    #[test]
    /// Rejects opening a schema-v2 store whose observation table has malformed columns.
    fn v1_preflight_rejects_malformed_observation_columns() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP TRIGGER observations_reject_update;
                 DROP TRIGGER observations_reject_delete;
                 DROP TRIGGER observations_reject_duplicate_insert;
                 DROP INDEX observations_observed_at_idx;
                 DROP TABLE observations;
                 CREATE TABLE observations (
                     id TEXT PRIMARY KEY NOT NULL,
                     document TEXT NOT NULL
                 );",
            )
            .expect("test should replace observation table");
        drop(connection);

        let error = SqliteObservationStore::open(&database)
            .expect_err("malformed columns should be rejected");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rejects opening a schema-v2 store whose observation index targets the wrong columns.
    fn v1_preflight_rejects_wrong_observation_index() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP INDEX observations_observed_at_idx;
                 CREATE INDEX observations_observed_at_idx ON observations (id);",
            )
            .expect("test should replace observation index");
        drop(connection);

        let error = SqliteObservationStore::open(&database)
            .expect_err("wrong observation index should be rejected");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rejects opening a schema-v2 store containing an observation document with invalid JSON.
    fn v1_preflight_rejects_corrupt_observation_document() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        store.append(&observation()).expect("append should succeed");
        drop(store);
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP TRIGGER observations_reject_update;
                 UPDATE observations SET document = '{not-json}';
                 CREATE TRIGGER observations_reject_update
                 BEFORE UPDATE ON observations
                 BEGIN
                     SELECT RAISE(ABORT, 'observations are append-only');
                 END;",
            )
            .expect("test should corrupt observation document");
        drop(connection);

        let error = SqliteObservationStore::open(&database)
            .expect_err("corrupt observation should be rejected");

        assert!(matches!(error, StoreError::Serialization));
    }

    #[test]
    /// Rejects opening a schema-v2 store whose indexed timestamp differs from its document.
    fn v1_preflight_rejects_indexed_metadata_drift() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        store.append(&observation()).expect("append should succeed");
        drop(store);
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP TRIGGER observations_reject_update;
                 UPDATE observations SET observed_at = '2000-01-01T00:00:00+00:00';
                 CREATE TRIGGER observations_reject_update
                 BEFORE UPDATE ON observations
                 BEGIN
                     SELECT RAISE(ABORT, 'observations are append-only');
                 END;",
            )
            .expect("test should alter indexed metadata");
        drop(connection);

        let error =
            SqliteObservationStore::open(&database).expect_err("metadata drift should be rejected");

        assert!(matches!(error, StoreError::MetadataMismatch { .. }));
    }

    #[test]
    /// Rejects an Evidra database whose schema version is unknown.
    fn valid_application_id_with_unknown_version_is_not_migratable() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .pragma_update(None, "user_version", 99)
            .expect("test should change schema version");
        drop(connection);

        let error = SqliteObservationStore::open(&database)
            .expect_err("unknown schema version should be rejected");

        assert!(matches!(
            error,
            StoreError::UnexpectedDatabase {
                user_version: 99,
                ..
            }
        ));
    }

    #[test]
    /// Requires explicit initialization to migrate an otherwise valid v1 database.
    fn open_v1_requires_explicit_migration() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        create_v1_database(&database, None);

        let error = SqliteObservationStore::open(&database)
            .expect_err("ordinary open should require migration");

        assert!(matches!(
            error,
            StoreError::MigrationRequired {
                current_version: 1,
                target_version: 3,
                ..
            }
        ));
    }

    #[test]
    /// Requires explicit migration from a v2 database too, now that the derived layer exists.
    fn open_v2_requires_explicit_migration() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        create_v2_database(&database, None);

        let error = SqliteObservationStore::open(&database)
            .expect_err("ordinary open should require migration");

        assert!(matches!(
            error,
            StoreError::MigrationRequired {
                current_version: 2,
                target_version: 3,
                ..
            }
        ));
    }

    #[test]
    /// Migrates v1 to v3 while preserving serialized observation documents byte-for-byte.
    fn initialize_migrates_v1_without_rewriting_observations() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let expected = observation();
        create_v1_database(&database, Some(&expected));
        let before: String = rusqlite::Connection::open(&database)
            .expect("database should open")
            .query_row("SELECT document FROM observations", [], |row| row.get(0))
            .expect("document should exist");

        let store = SqliteObservationStore::initialize(&database).expect("v1 should migrate to v3");
        let after: String = store
            .connection
            .query_row("SELECT document FROM observations", [], |row| row.get(0))
            .expect("document should remain");
        let version: i32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version should be readable");

        assert_eq!(before, after);
        assert_eq!(version, 3);
        assert_eq!(store.list(10).expect("list should succeed"), vec![expected]);
    }

    #[test]
    /// Leaves the schema version at v1 when migration preflight fails.
    fn failed_migration_remains_v1() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        create_v1_database(&database, None);
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP INDEX observations_observed_at_idx;
                 CREATE INDEX observations_observed_at_idx ON observations (id);",
            )
            .expect("test should corrupt v1 index");
        drop(connection);

        assert!(SqliteObservationStore::initialize(&database).is_err());
        let version: i32 = rusqlite::Connection::open(&database)
            .expect("database should reopen")
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version should remain readable");

        assert_eq!(version, 1);
    }

    #[test]
    /// Rolls back receipt DDL when migration validation fails after DDL execution.
    fn post_ddl_migration_failure_rolls_back_receipt_objects() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        create_v1_database(&database, None);
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "CREATE TABLE agent_harness_receipts (
                    harness TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    source_event_id TEXT NOT NULL,
                    observation_id TEXT NOT NULL
                 );",
            )
            .expect("malformed preexisting receipt table should create");
        drop(connection);

        assert!(SqliteObservationStore::initialize(&database).is_err());
        let connection = rusqlite::Connection::open(&database).expect("database should reopen");
        let version: i32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version should remain readable");
        let created_trigger: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type = 'trigger' AND name = 'agent_harness_receipts_reject_update')",
                [],
                |row| row.get(0),
            )
            .expect("trigger state should be readable");

        assert_eq!(version, 1);
        assert!(!created_trigger);
    }

    #[test]
    /// Migrates v2 to v3 by adding the derived layer without rewriting any observation document.
    fn migrate_v2_to_v3_preserves_observation_documents() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let expected = observation();
        create_v2_database(&database, Some(&expected));
        let before: String = rusqlite::Connection::open(&database)
            .expect("database should open")
            .query_row("SELECT document FROM observations", [], |row| row.get(0))
            .expect("document should exist");

        let store = SqliteObservationStore::initialize(&database).expect("v2 should migrate to v3");
        let after: String = store
            .connection
            .query_row("SELECT document FROM observations", [], |row| row.get(0))
            .expect("document should remain");
        let version: i32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version should be readable");

        assert_eq!(before, after);
        assert_eq!(version, 3);
        assert_eq!(store.list(10).expect("list should succeed"), vec![expected]);
    }

    #[test]
    /// Adds the derived tables to a v2 database that already held receipts.
    fn migrate_v2_to_v3_adds_derived_tables_only() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        create_v2_database(&database, None);
        let before = schema_snapshot(&database);
        let observation_count_before = rusqlite::Connection::open(&database)
            .expect("database should open")
            .query_row("SELECT COUNT(*) FROM observations", [], |row| {
                row.get::<_, i32>(0)
            })
            .expect("count should be readable");

        drop(SqliteObservationStore::initialize(&database).expect("v2 should migrate to v3"));

        let after = schema_snapshot(&database);
        let added = after
            .iter()
            .filter(|entry| !before.contains(entry))
            .map(|(_, name, _)| name.clone())
            .collect::<Vec<_>>();

        for table in [
            "derivations",
            "derivation_facets",
            "derivation_evidence",
            "relationships",
        ] {
            assert!(
                added.iter().any(|name| name == table),
                "expected migration to add {table}"
            );
        }
        // Set containment rather than positional equality: the snapshot is ordered by name, so
        // adding objects shifts every later position even when nothing existing was rewritten.
        for entry in &before {
            assert!(
                after.contains(entry),
                "migration must not rewrite or drop an existing schema object: {entry:?}"
            );
        }
        let observation_count_after = rusqlite::Connection::open(&database)
            .expect("database should reopen")
            .query_row("SELECT COUNT(*) FROM observations", [], |row| {
                row.get::<_, i32>(0)
            })
            .expect("count should remain readable");
        assert_eq!(observation_count_before, observation_count_after);
    }

    #[test]
    /// Leaves the schema version at v2 when derived DDL fails validation after being applied.
    fn failed_derived_migration_remains_v2() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        create_v2_database(&database, None);
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "CREATE TABLE derivation_facets (
                     derivation_id TEXT NOT NULL,
                     namespace TEXT NOT NULL
                 );",
            )
            .expect("malformed preexisting derived table should create");
        drop(connection);

        assert!(SqliteObservationStore::initialize(&database).is_err());
        let connection = rusqlite::Connection::open(&database).expect("database should reopen");
        let version: i32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version should remain readable");
        let created_trigger: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type = 'trigger' AND name = 'derivations_reject_update')",
                [],
                |row| row.get(0),
            )
            .expect("trigger state should be readable");

        assert_eq!(version, 2);
        assert!(!created_trigger);
    }

    #[test]
    /// Accepts a stored derivation whose indexed projections match its document.
    fn derived_rows_survive_validation() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let derivation = facet_derivation();

        let store = reopen_after_mutation(&database, |connection| {
            insert_derivation(connection, &derivation, false);
        })
        .expect("matching projections should validate");

        let stored: String = store
            .connection
            .query_row(
                "SELECT document FROM derivations WHERE id = ?1",
                [derivation.id().as_str()],
                |row| row.get(0),
            )
            .expect("derivation should be readable");
        assert_eq!(
            stored,
            serde_json::to_string(&derivation).expect("serializes")
        );
    }

    #[test]
    /// Rejects a derivation whose indexed facet rows drifted from its document.
    ///
    /// This is the check that matters most for current-view queries: a stale projection would
    /// otherwise answer with a value the record no longer claims.
    fn derived_facet_drift_is_rejected() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let derivation = facet_derivation();

        let error = reopen_after_mutation(&database, |connection| {
            insert_derivation(connection, &derivation, true);
        })
        .expect_err("drifted facet rows must not validate");

        assert!(
            matches!(error, StoreError::DerivedFacetMismatch { .. }),
            "expected a facet projection mismatch, got {error:?}"
        );
    }

    #[test]
    /// Rejects a derivation whose indexed facet rows are missing rather than merely wrong.
    ///
    /// Written as an insert without facets rather than a delete, because the append-only trigger
    /// would refuse the delete and the test would never reach validation.
    fn derived_missing_facet_rows_are_rejected() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let derivation = facet_derivation();
        let document = serde_json::to_string(&derivation).expect("derivation should serialize");

        let error = reopen_after_mutation(&database, |connection| {
            connection
                .execute(
                    "INSERT INTO derivations (id, kind, recorded_at, supersedes, document)
                     VALUES (?1, ?2, ?3, NULL, ?4)",
                    rusqlite::params![
                        derivation.id().as_str(),
                        derivation.kind().as_str(),
                        derivation.recorded_at().to_rfc3339(),
                        document
                    ],
                )
                .expect("row should insert");
        })
        .expect_err("a derivation with no indexed facet rows must not validate");

        assert!(
            matches!(error, StoreError::DerivedFacetMismatch { .. }),
            "expected a facet projection mismatch, got {error:?}"
        );
    }

    #[test]
    /// Rejects a derivation row whose indexed kind disagrees with its document.
    fn derived_column_drift_is_rejected() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let derivation = facet_derivation();
        let document = serde_json::to_string(&derivation).expect("derivation should serialize");

        let error = reopen_after_mutation(&database, |connection| {
            // Written with the wrong kind column rather than updated, because the append-only
            // trigger would refuse the update and the test would never reach validation.
            connection
                .execute(
                    "INSERT INTO derivations (id, kind, recorded_at, supersedes, document)
                     VALUES (?1, 'aggregate', ?2, NULL, ?3)",
                    rusqlite::params![derivation.id().as_str(), DERIVED_AT, document],
                )
                .expect("row should insert");
        })
        .expect_err("an indexed kind that contradicts the document must not validate");

        assert!(
            matches!(error, StoreError::DerivedMetadataMismatch { .. }),
            "expected a metadata mismatch, got {error:?}"
        );
    }

    #[test]
    /// Rejects a derived record whose document cannot be read back as a domain record.
    fn derived_unreadable_document_is_rejected() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");

        let error = reopen_after_mutation(&database, |connection| {
            connection
                .execute(
                    "INSERT INTO derivations (id, kind, recorded_at, supersedes, document)
                     VALUES ('__evidra_bogus__', 'facet', ?1, NULL, '{\"id\":\"nonsense\"}')",
                    [DERIVED_AT],
                )
                .expect("row should insert");
        })
        .expect_err("a document that is not a derivation must not validate");

        assert!(
            matches!(error, StoreError::InvalidDerivedRecord { .. }),
            "expected an unreadable derived record, got {error:?}"
        );
    }

    #[test]
    /// Rejects a derived trigger that has been redefined to do nothing.
    ///
    /// A trigger that exists under the right name but has been emptied passes any name-only check,
    /// which is why the definitions are compared as normalized text.
    fn tampered_derived_trigger_is_rejected() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP TRIGGER relationships_reject_delete;
                 CREATE TRIGGER relationships_reject_delete
                 BEFORE DELETE ON relationships
                 BEGIN SELECT 1; END;",
            )
            .expect("test should neuter the trigger");
        drop(connection);

        let error =
            SqliteObservationStore::open(&database).expect_err("a neutered trigger must not pass");

        assert!(
            matches!(error, StoreError::IncompleteSchema { .. }),
            "expected an incomplete schema, got {error:?}"
        );
    }

    #[test]
    /// Blocks updates, deletes, replacements, and duplicate inserts on every derived table.
    fn derived_tables_reject_mutation() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let derivation = facet_derivation();
        let other = facet_derivation();
        let edge = edge_relationship(derivation.id(), other.id());
        let id_string = derivation.id().as_str();
        let probe_id = id_string.as_str();
        let edge_id = edge.id().as_str();
        let evidence_target = edge.from().key();
        let store = reopen_after_mutation(&database, |connection| {
            insert_derivation(connection, &derivation, false);
            insert_derivation(connection, &other, false);
            insert_relationship(connection, &edge);
            connection
                .execute(
                    "INSERT INTO derivation_evidence (derivation_id, target_key, role, recorded_at)
                     VALUES (?1, ?2, 'refuting', ?3)",
                    rusqlite::params![probe_id, evidence_target, DERIVED_AT],
                )
                .expect("evidence should insert");
        })
        .expect("matching projections should validate");
        let connection = store.connection;

        for (label, result) in [
            (
                "derivations update",
                connection.execute(
                    "UPDATE derivations SET kind = 'facet' WHERE id = ?1",
                    [probe_id],
                ),
            ),
            (
                "derivations delete",
                connection.execute("DELETE FROM derivations WHERE id = ?1", [probe_id]),
            ),
            (
                "derivations replace",
                connection.execute(
                    "INSERT OR REPLACE INTO derivations
                     (id, kind, recorded_at, supersedes, document)
                     VALUES (?1, 'facet', ?2, NULL, '{}')",
                    rusqlite::params![probe_id, DERIVED_AT],
                ),
            ),
            (
                "derivation facets delete",
                connection.execute(
                    "DELETE FROM derivation_facets WHERE derivation_id = ?1",
                    [probe_id],
                ),
            ),
            (
                "derivation evidence delete",
                connection.execute(
                    "DELETE FROM derivation_evidence WHERE derivation_id = ?1",
                    [probe_id],
                ),
            ),
            (
                "relationships delete",
                connection.execute(
                    "DELETE FROM relationships WHERE id = ?1",
                    [edge_id.as_str()],
                ),
            ),
        ] {
            assert!(result.is_err(), "expected {label} to be blocked");
        }
    }

    #[test]
    /// Rejects a facet row whose slot disagrees with which value column is populated.
    fn facet_slot_mismatch_is_rejected() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let derivation = facet_derivation();
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));

        let connection = rusqlite::Connection::open(&database).expect("database should reopen");
        insert_derivation(&connection, &derivation, false);
        let result = connection.execute(
            "INSERT INTO derivation_facets
             (derivation_id, namespace, name, slot, text_value, integer_value, boolean_value,
              confidence, freshness, recorded_at)
             VALUES (?1, 'probe', 'mismatch', 'text', NULL, 7, NULL,
                     'weak', 'current', ?2)",
            rusqlite::params![derivation.id().as_str(), DERIVED_AT],
        );

        assert!(
            result.is_err(),
            "a text slot with only an integer value must be refused"
        );
        drop(connection);
    }

    #[test]
    /// Leaves an already valid v3 schema unchanged during repeated initialization.
    fn initialize_v3_is_non_mutating() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let before = schema_snapshot(&database);

        drop(SqliteObservationStore::initialize(&database).expect("v3 init should be idempotent"));
        let after = schema_snapshot(&database);

        assert_eq!(before, after);
    }

    #[test]
    /// Creates a new database at v3 with the receipt and derived tables present.
    fn new_database_is_created_as_valid_v3() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let store = SqliteObservationStore::initialize(&database).expect("store should initialize");
        let version: i32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version should be readable");

        for table in [
            "agent_harness_receipts",
            "derivations",
            "derivation_facets",
            "derivation_evidence",
            "relationships",
        ] {
            let present: bool = store
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master
                     WHERE type = 'table' AND name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .expect("table presence should be queryable");
            assert!(present, "expected a v3 database to contain {table}");
        }
        assert_eq!(version, 3);
    }

    #[test]
    /// Blocks receipt updates, deletes, replacements, and duplicate inserts.
    fn receipt_update_delete_replace_and_duplicate_insert_are_blocked() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let store = SqliteObservationStore::initialize(&database).expect("store should initialize");
        let value = harness_observation();
        store
            .connection
            .execute(
                "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    value.observation().id().to_string(),
                    value.observation().observed_at().to_rfc3339(),
                    serde_json::to_string(value.observation())
                        .expect("observation should serialize")
                ],
            )
            .expect("observation should insert");
        store
            .connection
            .execute(
                "INSERT INTO agent_harness_receipts
                 (harness, session_id, source_event_id, event_digest, observation_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    value.identity().harness(),
                    value.identity().session_id(),
                    value.identity().source_event_id(),
                    value.event_digest(),
                    value.observation().id().to_string()
                ],
            )
            .expect("receipt should insert");

        assert!(
            store
                .connection
                .execute("UPDATE agent_harness_receipts SET harness = 'changed'", [])
                .is_err()
        );
        assert!(
            store
                .connection
                .execute("DELETE FROM agent_harness_receipts", [])
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "INSERT OR REPLACE INTO agent_harness_receipts
                 SELECT * FROM agent_harness_receipts",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO agent_harness_receipts
                 SELECT * FROM agent_harness_receipts",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "INSERT OR REPLACE INTO agent_harness_receipts
                 (harness, session_id, source_event_id, event_digest, observation_id)
                 VALUES ('different', 'different', 'different', ?1, ?2)",
                    rusqlite::params![value.event_digest(), value.observation().id().to_string()],
                )
                .is_err()
        );
    }

    #[test]
    /// Rejects triggers weakened to block only the validator's probe rows.
    fn open_rejects_probe_only_trigger_definitions() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP TRIGGER observations_reject_update;
                 CREATE TRIGGER observations_reject_update
                 BEFORE UPDATE ON observations
                 WHEN OLD.id = '__evidra_append_only_probe__'
                 BEGIN SELECT RAISE(ABORT, 'observations are append-only'); END;
                 DROP TRIGGER agent_harness_receipts_reject_update;
                 CREATE TRIGGER agent_harness_receipts_reject_update
                 BEFORE UPDATE ON agent_harness_receipts
                 WHEN OLD.observation_id = '__evidra_receipt_probe__'
                 BEGIN SELECT RAISE(ABORT, 'agent harness receipts are append-only'); END;",
            )
            .expect("test should weaken triggers");
        drop(connection);

        let error = SqliteObservationStore::open(&database)
            .expect_err("conditional triggers should be rejected");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rejects a database containing a harness observation without its receipt.
    fn open_rejects_unreceipted_harness_observation() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let store = SqliteObservationStore::initialize(&database).expect("store should initialize");
        let value = harness_observation();
        store
            .connection
            .execute(
                "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    value.observation().id().to_string(),
                    value.observation().observed_at().to_rfc3339(),
                    serde_json::to_string(value.observation())
                        .expect("observation should serialize")
                ],
            )
            .expect("direct harness observation should insert");
        drop(store);

        let error = SqliteObservationStore::open(&database)
            .expect_err("unreceipted harness observation should fail");

        assert!(matches!(error, StoreError::InvalidHarnessReceipt));
    }

    #[test]
    /// Reports corrupt v1 schema before offering migration guidance.
    fn open_rejects_corrupt_v1_before_migration_guidance() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        create_v1_database(&database, None);
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP INDEX observations_observed_at_idx;
                 CREATE INDEX observations_observed_at_idx ON observations (id);",
            )
            .expect("test should corrupt v1 index");
        drop(connection);

        let error =
            SqliteObservationStore::open(&database).expect_err("corrupt v1 should fail validation");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rejects an observation index with ascending rather than descending order.
    fn open_rejects_ascending_observation_index() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "DROP INDEX observations_observed_at_idx;
                 CREATE INDEX observations_observed_at_idx
                 ON observations (observed_at ASC, id ASC);",
            )
            .expect("test should replace index ordering");
        drop(connection);

        let error = SqliteObservationStore::open(&database)
            .expect_err("ascending index should be rejected");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rolls back the observation when its receipt insert fails.
    fn receipt_insert_failure_rolls_back_observation() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let mut store =
            SqliteObservationStore::initialize(&database).expect("store should initialize");
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER test_reject_receipt
                 BEFORE INSERT ON agent_harness_receipts
                 BEGIN SELECT RAISE(ABORT, 'test rejection'); END;",
            )
            .expect("test trigger should create");
        let value = harness_observation();

        assert!(store.append_harness_observation(&value).is_err());
        let observation_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
            .expect("observation count should load");
        let receipt_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM agent_harness_receipts", [], |row| {
                row.get(0)
            })
            .expect("receipt count should load");

        assert_eq!(observation_count, 0);
        assert_eq!(receipt_count, 0);
    }

    /// Replaces the receipt table while restoring its required append-only triggers.
    fn replace_receipt_table(database: &std::path::Path, table_sql: &str) {
        let connection = rusqlite::Connection::open(database).expect("database should open");
        connection
            .execute_batch(
                "DROP TRIGGER agent_harness_receipts_reject_update;
                 DROP TRIGGER agent_harness_receipts_reject_delete;
                 DROP TRIGGER agent_harness_receipts_reject_duplicate_insert;
                 DROP TABLE agent_harness_receipts;",
            )
            .expect("receipt objects should drop");
        connection
            .execute_batch(table_sql)
            .expect("replacement receipt table should create");
        connection
            .execute_batch(
                "CREATE TRIGGER agent_harness_receipts_reject_update
                 BEFORE UPDATE ON agent_harness_receipts
                 BEGIN SELECT RAISE(ABORT, 'agent harness receipts are append-only'); END;
                 CREATE TRIGGER agent_harness_receipts_reject_delete
                 BEFORE DELETE ON agent_harness_receipts
                 BEGIN SELECT RAISE(ABORT, 'agent harness receipts are append-only'); END;
                 CREATE TRIGGER agent_harness_receipts_reject_duplicate_insert
                 BEFORE INSERT ON agent_harness_receipts
                 WHEN EXISTS (
                     SELECT 1 FROM agent_harness_receipts
                     WHERE harness = NEW.harness
                       AND session_id = NEW.session_id
                       AND source_event_id = NEW.source_event_id
                 )
                 OR EXISTS (
                     SELECT 1 FROM agent_harness_receipts
                     WHERE observation_id = NEW.observation_id
                 )
                 BEGIN SELECT RAISE(ABORT, 'agent harness receipts are append-only'); END;",
            )
            .expect("replacement receipt triggers should create");
    }

    #[test]
    /// Rejects v2 when the receipt table omits a required column.
    fn v2_rejects_malformed_receipt_columns() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        replace_receipt_table(
            &database,
            "CREATE TABLE agent_harness_receipts (
                harness TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_event_id TEXT NOT NULL,
                observation_id TEXT NOT NULL UNIQUE,
                PRIMARY KEY (harness, session_id, source_event_id),
                FOREIGN KEY (observation_id) REFERENCES observations(id)
             );",
        );

        let error = SqliteObservationStore::open(&database)
            .expect_err("malformed receipt columns should fail");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rejects v2 when receipt identity columns have the wrong primary-key order.
    fn v2_rejects_wrong_receipt_primary_key() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        replace_receipt_table(
            &database,
            "CREATE TABLE agent_harness_receipts (
                harness TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_event_id TEXT NOT NULL,
                event_digest TEXT NOT NULL,
                observation_id TEXT NOT NULL UNIQUE,
                PRIMARY KEY (harness, source_event_id, session_id),
                FOREIGN KEY (observation_id) REFERENCES observations(id)
             );",
        );

        let error = SqliteObservationStore::open(&database)
            .expect_err("wrong receipt primary key should fail");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rejects v2 when receipts do not reference the observation table.
    fn v2_rejects_missing_receipt_foreign_key() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        replace_receipt_table(
            &database,
            "CREATE TABLE agent_harness_receipts (
                harness TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_event_id TEXT NOT NULL,
                event_digest TEXT NOT NULL,
                observation_id TEXT NOT NULL UNIQUE,
                PRIMARY KEY (harness, session_id, source_event_id)
             );",
        );

        let error = SqliteObservationStore::open(&database)
            .expect_err("missing receipt foreign key should fail");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Rejects v2 when observation receipt uniqueness is enforced only partially.
    fn v2_rejects_partial_observation_id_uniqueness() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        drop(SqliteObservationStore::initialize(&database).expect("store should initialize"));
        replace_receipt_table(
            &database,
            "CREATE TABLE agent_harness_receipts (
                harness TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_event_id TEXT NOT NULL,
                event_digest TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                PRIMARY KEY (harness, session_id, source_event_id),
                FOREIGN KEY (observation_id) REFERENCES observations(id)
             );
             CREATE UNIQUE INDEX partial_receipt_observation_id
             ON agent_harness_receipts (observation_id)
             WHERE observation_id != '';",
        );

        let error =
            SqliteObservationStore::open(&database).expect_err("partial uniqueness should fail");

        assert!(matches!(error, StoreError::IncompleteSchema { .. }));
    }

    #[test]
    /// Detects a receipt identity that differs from its harness observation payload.
    fn receipt_validation_detects_payload_identity_drift() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let store = SqliteObservationStore::initialize(&database).expect("store should initialize");
        let value = harness_observation();
        store
            .connection
            .execute(
                "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    value.observation().id().to_string(),
                    value.observation().observed_at().to_rfc3339(),
                    serde_json::to_string(value.observation())
                        .expect("observation should serialize")
                ],
            )
            .expect("observation should insert");
        store
            .connection
            .execute(
                "INSERT INTO agent_harness_receipts
                 (harness, session_id, source_event_id, event_digest, observation_id)
                 VALUES ('codex', ?1, ?2, ?3, ?4)",
                rusqlite::params![
                    value.identity().session_id(),
                    value.identity().source_event_id(),
                    value.event_digest(),
                    value.observation().id().to_string()
                ],
            )
            .expect("mismatched receipt should insert directly");
        drop(store);

        let error =
            SqliteObservationStore::open(&database).expect_err("identity drift should be rejected");

        assert!(matches!(error, StoreError::InvalidHarnessReceipt));
    }

    #[test]
    /// Detects a receipt digest that differs from its harness observation payload.
    fn receipt_validation_detects_payload_digest_drift() {
        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let database = temp_dir.path().join("evidra.db");
        let store = SqliteObservationStore::initialize(&database).expect("store should initialize");
        let value = harness_observation();
        store
            .connection
            .execute(
                "INSERT INTO observations (id, observed_at, document) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    value.observation().id().to_string(),
                    value.observation().observed_at().to_rfc3339(),
                    serde_json::to_string(value.observation())
                        .expect("observation should serialize")
                ],
            )
            .expect("observation should insert");
        store
            .connection
            .execute(
                "INSERT INTO agent_harness_receipts
                 (harness, session_id, source_event_id, event_digest, observation_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    value.identity().harness(),
                    value.identity().session_id(),
                    value.identity().source_event_id(),
                    "0".repeat(64),
                    value.observation().id().to_string()
                ],
            )
            .expect("drifted receipt should insert directly");
        drop(store);

        let error =
            SqliteObservationStore::open(&database).expect_err("digest drift should be rejected");

        assert!(matches!(error, StoreError::InvalidHarnessReceipt));
    }

    #[cfg(unix)]
    #[test]
    /// Restricts existing directory and database permissions during v1 migration.
    fn migration_restricts_existing_store_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let directory = temp_dir.path().join(".evidra");
        std::fs::create_dir(&directory).expect("store directory should create");
        let database = directory.join("evidra.db");
        create_v1_database(&database, None);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777))
            .expect("directory permissions should change");
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o666))
            .expect("database permissions should change");

        drop(SqliteObservationStore::initialize(&database).expect("migration should succeed"));
        let directory_mode = std::fs::metadata(&directory)
            .expect("directory metadata should load")
            .permissions()
            .mode()
            & 0o777;
        let database_mode = std::fs::metadata(&database)
            .expect("database metadata should load")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(directory_mode, 0o700);
        assert_eq!(database_mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    /// Rejects initialization through a symbolic-link state-directory ancestor.
    fn initialize_rejects_symbolic_link_ancestor() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let real = temp_dir.path().join("real");
        std::fs::create_dir(&real).expect("real directory should create");
        let linked = temp_dir.path().join("linked");
        symlink(&real, &linked).expect("ancestor symlink should create");

        let error = SqliteObservationStore::initialize(&linked.join("nested/evidra.db"))
            .expect_err("symbolic-link ancestor should be rejected");

        assert!(matches!(error, StoreError::SymlinkPath { .. }));
    }

    #[cfg(unix)]
    #[test]
    /// Rejects opening a database through a symbolic-link parent directory.
    fn open_rejects_symbolic_link_parent() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().expect("temporary directory should be created");
        let target = temp_dir.path().join("target");
        let database = target.join("evidra.db");
        let store = SqliteObservationStore::initialize(&database).expect("store should initialize");
        drop(store);
        let linked_parent = temp_dir.path().join("linked");
        symlink(&target, &linked_parent).expect("symbolic-link parent should be created");

        let error = SqliteObservationStore::open(&linked_parent.join("evidra.db"))
            .expect_err("symbolic-link parent should be rejected");

        assert!(matches!(error, StoreError::SymlinkPath { .. }));
    }
}

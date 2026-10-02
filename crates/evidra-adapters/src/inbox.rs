//! Secure repository-local filesystem inbox for agent-harness events.
//!
//! The inbox enforces a single ingestion owner, validates ownership, permissions, file type,
//! link count, and device boundaries, and uses no-follow/no-clobber operations when claiming or
//! quarantining evidence. Startup recovery completes interrupted quarantines and processes
//! existing claims before newly published files without overwriting retained evidence.

use std::fmt;

use thiserror::Error;

/// Source-safe failure reported by the filesystem inbox.
///
/// Variants intentionally omit paths, filenames, and underlying operating-system errors so
/// callers cannot accidentally disclose repository-local evidence in diagnostics.
#[derive(Clone, Copy, PartialEq, Eq, Error)]
pub enum AgentHarnessFileInboxError {
    /// Another ingestion process owns the advisory lock.
    #[error("agent harness ingestion is already running")]
    AlreadyRunning,
    /// Required atomic filesystem behavior is unsupported.
    #[error("agent harness inbox filesystem is unsupported")]
    UnsupportedFilesystem,
    /// Inbox directory creation or state lookup failed.
    #[error("failed to create agent harness inbox directories")]
    CreateDirectory,
    /// Directory inventory or validation failed.
    #[error("failed to inspect agent harness inbox")]
    InspectDirectory,
    /// An unsafe or unrecognized directory entry was found.
    #[error("agent harness inbox contains an unsafe entry")]
    UnsafeEntry,
    /// Atomic claim acquisition failed.
    #[error("failed to claim agent harness event")]
    Claim,
    /// A claimed event could not be opened safely.
    #[error("failed to open claimed agent harness event")]
    OpenClaim,
    /// A completed event could not be removed.
    #[error("failed to complete agent harness event")]
    Complete,
    /// A rejected event could not be quarantined.
    #[error("failed to quarantine agent harness event")]
    Quarantine,
    /// A fixed quarantine reason could not be written.
    #[error("failed to write agent harness quarantine reason")]
    WriteReason,
}

impl fmt::Debug for AgentHarnessFileInboxError {
    /// Formats this value without exposing sensitive evidence.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod implementation {
    use std::collections::{HashSet, VecDeque};
    use std::fs::File;
    use std::io::{BufReader, Read, Write};
    use std::path::Path;

    use evidra_core::{
        AgentHarnessEventSource, AgentHarnessInbox, ClaimedHarnessContent, QuarantineReason,
    };
    use rustix::fs::{
        AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags, fchmod, flock, fstat,
        mkdirat, openat, renameat_with, statat, unlinkat,
    };
    use rustix::io::Errno;
    use rustix::process::geteuid;
    use ulid::Ulid;

    use crate::{AgentHarnessAdapterError, AgentHarnessJsonlSource};

    use super::AgentHarnessFileInboxError;

    const MAX_ENTRIES: usize = 10_000;

    enum PendingClaim {
        Processing(String),
        Ready(String),
    }

    /// Opaque single-owner claim for one processing file.
    pub struct ClaimedHarnessFile {
        name: String,
    }

    impl std::fmt::Debug for ClaimedHarnessFile {
        /// Formats this value without exposing sensitive evidence.
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ClaimedHarnessFile")
                .finish_non_exhaustive()
        }
    }

    /// Exclusive bounded snapshot of a repository-local harness inbox.
    pub struct AgentHarnessFileInbox {
        _state_handle: File,
        inbox_handle: File,
        processing_handle: File,
        quarantine_handle: File,
        _lock_handle: File,
        pending: VecDeque<PendingClaim>,
        device: u64,
        effective_uid: u32,
    }

    impl std::fmt::Debug for AgentHarnessFileInbox {
        /// Formats this value without exposing sensitive evidence.
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("AgentHarnessFileInbox")
                .finish_non_exhaustive()
        }
    }

    impl AgentHarnessFileInbox {
        /// Opens and exclusively locks an existing Evidra state directory.
        ///
        /// # Errors
        ///
        /// Returns a fixed inbox error when filesystem invariants are not satisfied.
        pub fn initialize(state_dir: &Path) -> Result<Self, AgentHarnessFileInboxError> {
            if !state_dir.exists() {
                return Err(AgentHarnessFileInboxError::CreateDirectory);
            }
            let parent = state_dir
                .parent()
                .ok_or(AgentHarnessFileInboxError::CreateDirectory)?;
            let basename = state_dir
                .file_name()
                .ok_or(AgentHarnessFileInboxError::CreateDirectory)?;
            let parent_fd = openat(
                rustix::fs::CWD,
                parent,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|_| AgentHarnessFileInboxError::CreateDirectory)?;
            let state_fd = openat(
                &parent_fd,
                basename,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|_| AgentHarnessFileInboxError::CreateDirectory)?;
            let state_handle = File::from(state_fd);
            let effective_uid = geteuid().as_raw();
            let state_stat =
                fstat(&state_handle).map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
            validate_directory(&state_stat, effective_uid, None)?;
            let device = state_stat.st_dev as u64;

            let lock_handle = open_lock(&state_handle, effective_uid, device)?;
            let inbox_handle = open_child_directory(&state_handle, "inbox", effective_uid, device)?;
            let processing_handle =
                open_child_directory(&state_handle, "processing", effective_uid, device)?;
            let quarantine_handle =
                open_child_directory(&state_handle, "quarantine", effective_uid, device)?;
            let inventory = scan_inventory(
                &inbox_handle,
                &processing_handle,
                &quarantine_handle,
                effective_uid,
                device,
            )?;
            let pending = repair_and_queue(
                inventory,
                &processing_handle,
                &quarantine_handle,
                effective_uid,
                device,
            )?;
            Ok(Self {
                _state_handle: state_handle,
                inbox_handle,
                processing_handle,
                quarantine_handle,
                _lock_handle: lock_handle,
                pending,
                device,
                effective_uid,
            })
        }

        /// Atomically moves a ready file into processing under a fresh ULID name.
        ///
        /// Destination collisions are retried without replacement. Filesystems that cannot
        /// provide the required atomic no-replace rename are rejected rather than used unsafely.
        fn claim_ready(
            &mut self,
            source_name: &str,
        ) -> Result<ClaimedHarnessFile, AgentHarnessFileInboxError> {
            for _ in 0..16 {
                let claim_id = Ulid::new();
                let base = claim_id.to_string();
                let name = format!("{base}.json");
                if entry_exists(&self.processing_handle, &name)?
                    || entry_exists(&self.quarantine_handle, &name)?
                    || entry_exists(&self.quarantine_handle, &format!("{base}.reason"))?
                    || entry_exists(&self.processing_handle, &format!("{base}.reason.tmp"))?
                {
                    continue;
                }
                match renameat_with(
                    &self.inbox_handle,
                    source_name,
                    &self.processing_handle,
                    &name,
                    RenameFlags::NOREPLACE,
                ) {
                    Ok(()) => {
                        return Ok(ClaimedHarnessFile { name });
                    }
                    Err(error) if error == Errno::EXIST => continue,
                    Err(error) if unsupported_rename(error) => {
                        return Err(AgentHarnessFileInboxError::UnsupportedFilesystem);
                    }
                    Err(_) => return Err(AgentHarnessFileInboxError::Claim),
                }
            }
            Err(AgentHarnessFileInboxError::Claim)
        }

        /// Opens a processing claim without following symlinks and revalidates its metadata.
        fn open_claim(
            &self,
            claim: &ClaimedHarnessFile,
        ) -> Result<BufReader<File>, AgentHarnessFileInboxError> {
            let fd = openat(
                &self.processing_handle,
                claim.name.as_str(),
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|_| AgentHarnessFileInboxError::OpenClaim)?;
            let file = File::from(fd);
            let stat = fstat(&file).map_err(|_| AgentHarnessFileInboxError::OpenClaim)?;
            validate_regular(&stat, self.effective_uid, self.device, 0o600, true)?;
            Ok(BufReader::new(file))
        }

        /// Moves a rejected claim and its fixed reason into quarantine without replacement.
        ///
        /// The reason is staged in `processing` first so startup recovery can finish an
        /// interrupted multi-file quarantine while preserving the original evidence.
        fn quarantine_claim(
            &mut self,
            claim: ClaimedHarnessFile,
            reason: QuarantineReason,
        ) -> Result<(), AgentHarnessFileInboxError> {
            let base = claim
                .name
                .strip_suffix(".json")
                .ok_or(AgentHarnessFileInboxError::Quarantine)?;
            let stage_name = format!("{base}.reason.stage.{}", Ulid::new());
            let temp_name = format!("{base}.reason.tmp");
            let reason_name = format!("{base}.reason");
            let stage_fd = openat(
                &self.processing_handle,
                stage_name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| AgentHarnessFileInboxError::WriteReason)?;
            let mut stage = File::from(stage_fd);
            fchmod(&stage, Mode::from_raw_mode(0o600))
                .map_err(|_| AgentHarnessFileInboxError::WriteReason)?;
            stage
                .write_all(reason.to_string().as_bytes())
                .map_err(|_| AgentHarnessFileInboxError::WriteReason)?;
            drop(stage);
            rename_noreplace(
                &self.processing_handle,
                &stage_name,
                &self.processing_handle,
                &temp_name,
                AgentHarnessFileInboxError::WriteReason,
            )?;
            rename_noreplace(
                &self.processing_handle,
                &claim.name,
                &self.quarantine_handle,
                &claim.name,
                AgentHarnessFileInboxError::Quarantine,
            )?;
            rename_noreplace(
                &self.processing_handle,
                &temp_name,
                &self.quarantine_handle,
                &reason_name,
                AgentHarnessFileInboxError::Quarantine,
            )
        }
    }

    impl AgentHarnessInbox for AgentHarnessFileInbox {
        type Claim = ClaimedHarnessFile;
        type Error = AgentHarnessFileInboxError;

        /// Returns the next recovered or newly claimed file from the bounded startup snapshot.
        ///
        /// Recovered processing files are returned before ready files. A ready file is renamed
        /// into processing before its opaque claim is exposed.
        fn next_claim(&mut self) -> Result<Option<Self::Claim>, Self::Error> {
            let Some(pending) = self.pending.pop_front() else {
                return Ok(None);
            };
            match pending {
                PendingClaim::Processing(name) => Ok(Some(ClaimedHarnessFile { name })),
                PendingClaim::Ready(name) => self.claim_ready(&name).map(Some),
            }
        }

        /// Decodes exactly one event from a safely opened processing claim.
        ///
        /// Empty, multi-event, malformed, unsupported, and over-limit content is classified for
        /// quarantine. I/O failures remain operational errors so callers retain the active claim.
        fn read_claim(
            &mut self,
            claim: &Self::Claim,
        ) -> Result<ClaimedHarnessContent, Self::Error> {
            let reader = self.open_claim(claim)?;
            let mut source = AgentHarnessJsonlSource::new(reader);
            let first = match source.next_event() {
                Ok(Some(event)) => event,
                Ok(None) => {
                    return Ok(ClaimedHarnessContent::Quarantine(
                        QuarantineReason::EmptyEvent,
                    ));
                }
                Err(error) => return map_decode_error(error),
            };
            match source.next_event() {
                Ok(None) => Ok(ClaimedHarnessContent::Event(Box::new(first))),
                Ok(Some(_)) => Ok(ClaimedHarnessContent::Quarantine(
                    QuarantineReason::MultipleEvents,
                )),
                Err(error) => map_decode_error(error),
            }
        }

        /// Permanently removes a successfully persisted processing claim.
        fn complete(&mut self, claim: Self::Claim) -> Result<(), Self::Error> {
            unlinkat(
                &self.processing_handle,
                claim.name.as_str(),
                AtFlags::empty(),
            )
            .map_err(|_| AgentHarnessFileInboxError::Complete)
        }

        /// Retains a rejected claim with a stable, non-sensitive reason sidecar.
        fn quarantine(
            &mut self,
            claim: Self::Claim,
            reason: QuarantineReason,
        ) -> Result<(), Self::Error> {
            self.quarantine_claim(claim, reason)
        }
    }

    /// Converts decoder failures into quarantine classifications or an operational read error.
    fn map_decode_error(
        error: AgentHarnessAdapterError,
    ) -> Result<ClaimedHarnessContent, AgentHarnessFileInboxError> {
        let reason = match error {
            AgentHarnessAdapterError::Read { .. } => {
                return Err(AgentHarnessFileInboxError::OpenClaim);
            }
            AgentHarnessAdapterError::RecordTooLarge { .. } => QuarantineReason::RecordTooLarge,
            AgentHarnessAdapterError::BlankInputLimit { .. } => QuarantineReason::BlankInputLimit,
            AgentHarnessAdapterError::InvalidJson { .. } => QuarantineReason::InvalidJson,
            AgentHarnessAdapterError::UnsupportedSchema { .. } => {
                QuarantineReason::UnsupportedSchema
            }
            AgentHarnessAdapterError::InvalidReference { .. } => QuarantineReason::InvalidReference,
            AgentHarnessAdapterError::InvalidEvent { .. } => QuarantineReason::InvalidEvent,
        };
        Ok(ClaimedHarnessContent::Quarantine(reason))
    }

    /// Returns whether a rename error means the required atomic claim protocol is unavailable.
    pub(super) fn unsupported_rename(error: Errno) -> bool {
        error == Errno::XDEV
            || error == Errno::NOSYS
            || error == Errno::INVAL
            || error == Errno::NOTSUP
            || error == Errno::OPNOTSUPP
    }

    /// Renames an entry atomically without replacing an existing destination.
    ///
    /// Unsupported atomic semantics are distinguished from operation-specific failures so the
    /// inbox fails closed on filesystems that cannot preserve evidence safely.
    fn rename_noreplace(
        from_dir: &File,
        from: &str,
        to_dir: &File,
        to: &str,
        operation_error: AgentHarnessFileInboxError,
    ) -> Result<(), AgentHarnessFileInboxError> {
        match renameat_with(from_dir, from, to_dir, to, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            Err(error) if unsupported_rename(error) => {
                Err(AgentHarnessFileInboxError::UnsupportedFilesystem)
            }
            Err(_) => Err(operation_error),
        }
    }

    /// Checks for a directory entry without following a final symlink.
    fn entry_exists(dir: &File, name: &str) -> Result<bool, AgentHarnessFileInboxError> {
        match statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(Errno::NOENT) => Ok(false),
            Err(_) => Err(AgentHarnessFileInboxError::InspectDirectory),
        }
    }

    /// Creates or opens a private inbox child directory and validates its security metadata.
    fn open_child_directory(
        state: &File,
        name: &str,
        uid: u32,
        device: u64,
    ) -> Result<File, AgentHarnessFileInboxError> {
        let created = match mkdirat(state, name, Mode::from_raw_mode(0o700)) {
            Ok(()) => true,
            Err(Errno::EXIST) => false,
            Err(_) => return Err(AgentHarnessFileInboxError::CreateDirectory),
        };
        let fd = openat(
            state,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| AgentHarnessFileInboxError::CreateDirectory)?;
        let file = File::from(fd);
        if created {
            fchmod(&file, Mode::from_raw_mode(0o700))
                .map_err(|_| AgentHarnessFileInboxError::CreateDirectory)?;
        }
        let stat = fstat(&file).map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
        validate_directory(&stat, uid, Some(device))?;
        Ok(file)
    }

    /// Opens the private ingest lock file and acquires a non-blocking exclusive advisory lock.
    fn open_lock(state: &File, uid: u32, device: u64) -> Result<File, AgentHarnessFileInboxError> {
        let (fd, created) = match openat(
            state,
            "ingest.lock",
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => (fd, false),
            Err(Errno::NOENT) => match openat(
                state,
                "ingest.lock",
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::from_raw_mode(0o600),
            ) {
                Ok(fd) => (fd, true),
                Err(Errno::EXIST) => {
                    let fd = openat(
                        state,
                        "ingest.lock",
                        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                        Mode::empty(),
                    )
                    .map_err(|_| AgentHarnessFileInboxError::CreateDirectory)?;
                    (fd, false)
                }
                Err(_) => return Err(AgentHarnessFileInboxError::CreateDirectory),
            },
            Err(_) => return Err(AgentHarnessFileInboxError::CreateDirectory),
        };
        let file = File::from(fd);
        if created {
            fchmod(&file, Mode::from_raw_mode(0o600))
                .map_err(|_| AgentHarnessFileInboxError::CreateDirectory)?;
        }
        let stat = fstat(&file).map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
        validate_regular(&stat, uid, device, 0o600, true)?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(file),
            Err(error) if error == Errno::WOULDBLOCK || error == Errno::AGAIN => {
                Err(AgentHarnessFileInboxError::AlreadyRunning)
            }
            Err(_) => Err(AgentHarnessFileInboxError::CreateDirectory),
        }
    }

    /// Validates that a directory is private, owned by the effective user, and on the expected
    /// device when one is specified.
    fn validate_directory(
        stat: &rustix::fs::Stat,
        uid: u32,
        device: Option<u64>,
    ) -> Result<(), AgentHarnessFileInboxError> {
        let valid = FileType::from_raw_mode(stat.st_mode) == FileType::Directory
            && stat.st_uid == uid
            && (stat.st_mode as u32 & 0o7777) == 0o700
            && device.is_none_or(|expected| stat.st_dev as u64 == expected);
        if valid {
            Ok(())
        } else {
            Err(AgentHarnessFileInboxError::UnsafeEntry)
        }
    }

    /// Validates a regular file's owner, device, single-link status, and permission policy.
    ///
    /// Exact mode checks require the supplied mode. Relaxed checks still require owner-read
    /// access and prohibit all group and other permissions.
    pub(super) fn validate_regular(
        stat: &rustix::fs::Stat,
        uid: u32,
        device: u64,
        mode: u32,
        exact_mode: bool,
    ) -> Result<(), AgentHarnessFileInboxError> {
        let actual_mode = stat.st_mode as u32 & 0o7777;
        let mode_valid = if exact_mode {
            actual_mode == mode
        } else {
            actual_mode & 0o077 == 0 && actual_mode & 0o400 != 0
        };
        let valid = FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
            && stat.st_uid == uid
            && stat.st_nlink == 1
            && stat.st_dev as u64 == device
            && mode_valid;
        if valid {
            Ok(())
        } else {
            Err(AgentHarnessFileInboxError::UnsafeEntry)
        }
    }

    struct Inventory {
        inbox: Vec<String>,
        processing: Vec<String>,
        quarantine: Vec<String>,
    }

    /// Inventories and validates all inbox lifecycle directories within the shared entry limit.
    fn scan_inventory(
        inbox: &File,
        processing: &File,
        quarantine: &File,
        uid: u32,
        device: u64,
    ) -> Result<Inventory, AgentHarnessFileInboxError> {
        let mut count = 0usize;
        let inbox_names = scan_directory(inbox, &mut count)?;
        let processing_names = scan_directory(processing, &mut count)?;
        let quarantine_names = scan_directory(quarantine, &mut count)?;

        for name in &inbox_names {
            if !(ready_name(name) || safe_temp_name(name)) {
                return Err(AgentHarnessFileInboxError::UnsafeEntry);
            }
            validate_entry(inbox, name, uid, device, true)?;
        }
        for name in &processing_names {
            if !(processing_name(name) || reason_temp_name(name) || reason_stage_name(name)) {
                return Err(AgentHarnessFileInboxError::UnsafeEntry);
            }
            validate_entry(processing, name, uid, device, true)?;
        }
        for name in &quarantine_names {
            if !quarantine_name(name) {
                return Err(AgentHarnessFileInboxError::UnsafeEntry);
            }
            validate_entry(quarantine, name, uid, device, true)?;
        }
        Ok(Inventory {
            inbox: inbox_names,
            processing: processing_names,
            quarantine: quarantine_names,
        })
    }

    /// Reads one directory, rejecting non-UTF-8 names and enforcing the aggregate entry limit.
    fn scan_directory(
        handle: &File,
        count: &mut usize,
    ) -> Result<Vec<String>, AgentHarnessFileInboxError> {
        let mut directory =
            Dir::read_from(handle).map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
        let mut names = Vec::new();
        while let Some(entry) = directory.read() {
            let entry = entry.map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| AgentHarnessFileInboxError::UnsafeEntry)?;
            if name == "." || name == ".." {
                continue;
            }
            *count += 1;
            if *count > MAX_ENTRIES {
                return Err(AgentHarnessFileInboxError::InspectDirectory);
            }
            names.push(name.to_owned());
        }
        Ok(names)
    }

    /// Validates one named entry without following symlinks.
    fn validate_entry(
        handle: &File,
        name: &str,
        uid: u32,
        device: u64,
        exact_mode: bool,
    ) -> Result<(), AgentHarnessFileInboxError> {
        let stat = statat(handle, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
        validate_regular(&stat, uid, device, 0o600, exact_mode)
    }

    /// Returns whether a producer filename is a bounded, portable JSON ready name.
    fn ready_name(name: &str) -> bool {
        let bytes = name.as_bytes();
        (6..=128).contains(&bytes.len())
            && name.ends_with(".json")
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }

    /// Returns whether a producer temporary filename uses the accepted bounded character set.
    fn safe_temp_name(name: &str) -> bool {
        let bytes = name.as_bytes();
        (5..=128).contains(&bytes.len())
            && name.ends_with(".tmp")
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }

    /// Returns whether a processing filename is a canonical ULID followed by `.json`.
    fn processing_name(name: &str) -> bool {
        name.strip_suffix(".json")
            .and_then(|value| value.parse::<Ulid>().ok())
            .is_some()
    }

    /// Returns whether a quarantine filename is a canonical ULID JSON or reason sidecar.
    fn quarantine_name(name: &str) -> bool {
        name.strip_suffix(".json")
            .or_else(|| name.strip_suffix(".reason"))
            .and_then(|value| value.parse::<Ulid>().ok())
            .is_some()
    }

    /// Returns whether a filename is the published temporary reason for a ULID claim.
    fn reason_temp_name(name: &str) -> bool {
        name.strip_suffix(".reason.tmp")
            .and_then(|value| value.parse::<Ulid>().ok())
            .is_some()
    }

    /// Returns whether a filename contains valid claim and staging ULIDs for a reason write.
    fn reason_stage_name(name: &str) -> bool {
        let Some((base, stage)) = name.split_once(".reason.stage.") else {
            return false;
        };
        base.parse::<Ulid>().is_ok() && stage.parse::<Ulid>().is_ok()
    }

    /// Repairs recoverable quarantine states and builds the deterministic claim queue.
    ///
    /// Orphaned or malformed recovery artifacts fail closed. Completed processing claims are
    /// sorted ahead of ready files so interrupted work is resumed before new evidence.
    fn repair_and_queue(
        inventory: Inventory,
        processing_handle: &File,
        quarantine_handle: &File,
        uid: u32,
        device: u64,
    ) -> Result<VecDeque<PendingClaim>, AgentHarnessFileInboxError> {
        let mut processing = inventory.processing.into_iter().collect::<HashSet<_>>();
        let mut quarantine = inventory.quarantine.into_iter().collect::<HashSet<_>>();
        let stages = processing
            .iter()
            .filter(|name| reason_stage_name(name))
            .cloned()
            .collect::<Vec<_>>();
        for name in stages {
            if let Some((base, _stage_id)) = name.split_once(".reason.stage.") {
                if !processing.contains(&format!("{base}.json"))
                    && !quarantine.contains(&format!("{base}.json"))
                {
                    return Err(AgentHarnessFileInboxError::UnsafeEntry);
                }
                unlinkat(processing_handle, name.as_str(), AtFlags::empty())
                    .map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
                processing.remove(&name);
            }
        }
        let temp_reasons = processing
            .iter()
            .filter(|name| reason_temp_name(name))
            .cloned()
            .collect::<Vec<_>>();
        for name in temp_reasons {
            let Some(base) = name.strip_suffix(".reason.tmp") else {
                continue;
            };
            let reason_text = read_small_file(processing_handle, &name, uid, device)?;
            parse_reason(&reason_text).ok_or(AgentHarnessFileInboxError::UnsafeEntry)?;
            let json_name = format!("{base}.json");
            let reason_name = format!("{base}.reason");
            if processing.contains(&json_name) {
                rename_noreplace(
                    processing_handle,
                    &json_name,
                    quarantine_handle,
                    &json_name,
                    AgentHarnessFileInboxError::Quarantine,
                )?;
                processing.remove(&json_name);
                quarantine.insert(json_name.clone());
            } else if !quarantine.contains(&json_name) {
                return Err(AgentHarnessFileInboxError::UnsafeEntry);
            }
            rename_noreplace(
                processing_handle,
                &name,
                quarantine_handle,
                &reason_name,
                AgentHarnessFileInboxError::Quarantine,
            )?;
            processing.remove(&name);
            quarantine.insert(reason_name);
        }
        let quarantine_names = quarantine.clone().into_iter().collect::<Vec<_>>();
        for name in quarantine_names {
            if let Some(base) = name.strip_suffix(".json") {
                let reason_name = format!("{base}.reason");
                if !quarantine.contains(&reason_name) {
                    let stage_name = format!("{base}.reason.stage.{}", Ulid::new());
                    write_reason_at(
                        processing_handle,
                        &stage_name,
                        QuarantineReason::IncompleteQuarantine,
                    )?;
                    rename_noreplace(
                        processing_handle,
                        &stage_name,
                        quarantine_handle,
                        &reason_name,
                        AgentHarnessFileInboxError::WriteReason,
                    )?;
                    quarantine.insert(reason_name);
                }
            } else if let Some(base) = name.strip_suffix(".reason") {
                if !quarantine.contains(&format!("{base}.json")) {
                    return Err(AgentHarnessFileInboxError::UnsafeEntry);
                }
                let content = read_small_file(quarantine_handle, &name, uid, device)?;
                parse_reason(&content).ok_or(AgentHarnessFileInboxError::UnsafeEntry)?;
            } else if reason_stage_name(&name) {
                return Err(AgentHarnessFileInboxError::UnsafeEntry);
            }
        }
        let mut processing_queue = processing
            .into_iter()
            .filter(|name| processing_name(name))
            .collect::<Vec<_>>();
        let mut ready = inventory
            .inbox
            .into_iter()
            .filter(|name| ready_name(name))
            .collect::<Vec<_>>();
        processing_queue.sort();
        ready.sort();
        Ok(processing_queue
            .into_iter()
            .map(PendingClaim::Processing)
            .chain(ready.into_iter().map(PendingClaim::Ready))
            .collect())
    }

    /// Creates a private quarantine-reason file without replacing existing evidence.
    fn write_reason_at(
        directory: &File,
        name: &str,
        reason: QuarantineReason,
    ) -> Result<(), AgentHarnessFileInboxError> {
        let fd = openat(
            directory,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| AgentHarnessFileInboxError::WriteReason)?;
        let mut file = File::from(fd);
        fchmod(&file, Mode::from_raw_mode(0o600))
            .map_err(|_| AgentHarnessFileInboxError::WriteReason)?;
        file.write_all(reason.to_string().as_bytes())
            .map_err(|_| AgentHarnessFileInboxError::WriteReason)
    }

    /// Reads a validated reason sidecar while enforcing the 64-byte content limit.
    fn read_small_file(
        directory: &File,
        name: &str,
        uid: u32,
        device: u64,
    ) -> Result<String, AgentHarnessFileInboxError> {
        let fd = openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
        let mut file = File::from(fd);
        let stat = fstat(&file).map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
        validate_regular(&stat, uid, device, 0o600, true)?;
        if stat.st_size > 64 {
            return Err(AgentHarnessFileInboxError::UnsafeEntry);
        }
        let mut content = String::new();
        Read::by_ref(&mut file)
            .take(65)
            .read_to_string(&mut content)
            .map_err(|_| AgentHarnessFileInboxError::InspectDirectory)?;
        if content.len() > 64 {
            return Err(AgentHarnessFileInboxError::UnsafeEntry);
        }
        Ok(content)
    }

    /// Parses the complete set of stable quarantine reason strings.
    fn parse_reason(value: &str) -> Option<QuarantineReason> {
        Some(match value {
            "invalid-json" => QuarantineReason::InvalidJson,
            "unsupported-schema" => QuarantineReason::UnsupportedSchema,
            "invalid-reference" => QuarantineReason::InvalidReference,
            "invalid-event" => QuarantineReason::InvalidEvent,
            "record-too-large" => QuarantineReason::RecordTooLarge,
            "blank-input-limit" => QuarantineReason::BlankInputLimit,
            "empty-event" => QuarantineReason::EmptyEvent,
            "multiple-events" => QuarantineReason::MultipleEvents,
            "identity-conflict" => QuarantineReason::IdentityConflict,
            "incomplete-quarantine" => QuarantineReason::IncompleteQuarantine,
            _ => return None,
        })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod implementation {
    use std::path::Path;

    use evidra_core::{AgentHarnessInbox, ClaimedHarnessContent, QuarantineReason};

    use super::AgentHarnessFileInboxError;

    /// Unsupported-platform fail-closed inbox stub.
    #[derive(Debug)]
    pub struct AgentHarnessFileInbox;

    /// Unsupported-platform opaque claim.
    #[derive(Debug)]
    pub struct ClaimedHarnessFile;

    impl AgentHarnessFileInbox {
        /// Always fails closed on unsupported targets.
        pub fn initialize(_state_dir: &Path) -> Result<Self, AgentHarnessFileInboxError> {
            Err(AgentHarnessFileInboxError::UnsupportedFilesystem)
        }
    }

    impl AgentHarnessInbox for AgentHarnessFileInbox {
        type Claim = ClaimedHarnessFile;
        type Error = AgentHarnessFileInboxError;

        /// Fails closed because secure claim operations are unavailable on this target.
        fn next_claim(&mut self) -> Result<Option<Self::Claim>, Self::Error> {
            Err(AgentHarnessFileInboxError::UnsupportedFilesystem)
        }

        /// Fails closed because claims cannot be opened securely on this target.
        fn read_claim(
            &mut self,
            _claim: &Self::Claim,
        ) -> Result<ClaimedHarnessContent, Self::Error> {
            Err(AgentHarnessFileInboxError::UnsupportedFilesystem)
        }

        /// Fails closed because claim completion is unsupported on this target.
        fn complete(&mut self, _claim: Self::Claim) -> Result<(), Self::Error> {
            Err(AgentHarnessFileInboxError::UnsupportedFilesystem)
        }

        /// Fails closed because evidence cannot be quarantined securely on this target.
        fn quarantine(
            &mut self,
            _claim: Self::Claim,
            _reason: QuarantineReason,
        ) -> Result<(), Self::Error> {
            Err(AgentHarnessFileInboxError::UnsupportedFilesystem)
        }
    }
}

pub use implementation::{AgentHarnessFileInbox, ClaimedHarnessFile};

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use evidra_core::{AgentHarnessInbox, ClaimedHarnessContent, QuarantineReason};
    use tempfile::TempDir;
    use ulid::Ulid;

    use super::{AgentHarnessFileInbox, AgentHarnessFileInboxError};

    /// Creates a private temporary state directory for filesystem security tests.
    fn state_directory() -> TempDir {
        let temp = TempDir::new().expect("temporary directory should create");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
            .expect("state permissions should set");
        temp
    }

    #[test]
    /// Verifies initialization creates each lifecycle directory with owner-only permissions.
    fn initialize_creates_private_same_device_directories() {
        let state = state_directory();

        let _inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");

        for child in ["inbox", "processing", "quarantine"] {
            let metadata =
                std::fs::metadata(state.path().join(child)).expect("child metadata should load");
            assert!(metadata.is_dir());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
    }

    #[test]
    /// Verifies a second inbox instance cannot acquire the active ingestion lock.
    fn second_inbox_lock_is_rejected() {
        let state = state_directory();
        let _first =
            AgentHarnessFileInbox::initialize(state.path()).expect("first inbox should initialize");

        let error = AgentHarnessFileInbox::initialize(state.path())
            .expect_err("second inbox should not acquire lock");

        assert!(matches!(error, AgentHarnessFileInboxError::AlreadyRunning));
    }

    /// Builds a valid versioned agent-harness event fixture.
    fn valid_record() -> String {
        serde_json::json!({
            "schema": "evidra.agent-harness-event/v1",
            "source_event_id": "event-1",
            "occurred_at": "2026-09-19T08:00:00Z",
            "source": { "kind": "agent-harness", "locator": "session#event-1" },
            "subject": { "kind": "repository", "identifier": "/tmp/example" },
            "harness": { "name": "claude-code", "version": null },
            "session_id": "session-1",
            "event_type": "tool-completed",
            "redaction": { "policy": "test", "version": "1", "transformations": [] },
            "excerpts": [],
            "facets": []
        })
        .to_string()
    }

    /// Publishes an owner-only ready-file fixture into the test inbox.
    fn write_ready(state: &TempDir, name: &str, content: &str) {
        let path = state.path().join("inbox").join(name);
        std::fs::write(&path, content).expect("ready event should write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("ready permissions should set");
    }

    #[test]
    /// Verifies a ready file is atomically claimed under a generated ULID and can be completed.
    fn ready_file_claims_to_generated_ulid() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        write_ready(&state, "event-1.json", &valid_record());
        drop(inbox);
        let mut inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should rescan");

        let claim = inbox
            .next_claim()
            .expect("claim should succeed")
            .expect("claim should exist");
        let content = inbox.read_claim(&claim).expect("claim should decode");

        assert!(matches!(content, ClaimedHarnessContent::Event(_)));
        assert!(
            std::fs::read_dir(state.path().join("inbox"))
                .expect("inbox should list")
                .next()
                .is_none()
        );
        assert_eq!(
            std::fs::read_dir(state.path().join("processing"))
                .expect("processing should list")
                .count(),
            1
        );
        inbox.complete(claim).expect("claim should complete");
    }

    #[test]
    /// Verifies malformed JSON is retained with its stable quarantine reason sidecar.
    fn quarantine_preserves_json_and_fixed_reason() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        write_ready(&state, "invalid.json", "{not-json}");
        drop(inbox);
        let mut inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should rescan");
        let claim = inbox
            .next_claim()
            .expect("claim should succeed")
            .expect("claim should exist");

        assert_eq!(
            inbox.read_claim(&claim).expect("claim should classify"),
            ClaimedHarnessContent::Quarantine(QuarantineReason::InvalidJson)
        );
        inbox
            .quarantine(claim, QuarantineReason::InvalidJson)
            .expect("claim should quarantine");

        let files = std::fs::read_dir(state.path().join("quarantine"))
            .expect("quarantine should list")
            .map(|entry| {
                entry
                    .expect("entry should load")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 2);
        let reason = files
            .iter()
            .find(|name| name.ends_with(".reason"))
            .expect("reason should exist");
        assert_eq!(
            std::fs::read_to_string(state.path().join("quarantine").join(reason))
                .expect("reason should read"),
            "invalid-json"
        );
    }

    #[test]
    /// Verifies initialization rejects a ready entry that is a symbolic link.
    fn initialize_rejects_symlink_ready_entry() {
        use std::os::unix::fs::symlink;

        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let target = state.path().join("target.json");
        std::fs::write(&target, valid_record()).expect("target should write");
        symlink(&target, state.path().join("inbox/event.json")).expect("symlink should create");
        drop(inbox);

        let error = AgentHarnessFileInbox::initialize(state.path())
            .expect_err("symlink should be rejected");

        assert!(matches!(error, AgentHarnessFileInboxError::UnsafeEntry));
    }

    #[test]
    /// Verifies initialization rejects a ready file with more than one hard link.
    fn initialize_rejects_hardlinked_ready_entry() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let target = state.path().join("target.json");
        std::fs::write(&target, valid_record()).expect("target should write");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("target permissions should set");
        std::fs::hard_link(&target, state.path().join("inbox/event.json"))
            .expect("hardlink should create");
        drop(inbox);

        let error = AgentHarnessFileInbox::initialize(state.path())
            .expect_err("hardlink should be rejected");

        assert!(matches!(error, AgentHarnessFileInboxError::UnsafeEntry));
    }

    #[test]
    /// Verifies recovery adds an `incomplete-quarantine` reason to orphaned quarantined JSON.
    fn quarantine_json_without_reason_gets_incomplete_reason() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let id = Ulid::new();
        let json = state.path().join(format!("quarantine/{id}.json"));
        std::fs::write(&json, "{}").expect("quarantine JSON should write");
        std::fs::set_permissions(&json, std::fs::Permissions::from_mode(0o600))
            .expect("quarantine permissions should set");
        drop(inbox);

        let _recovered = AgentHarnessFileInbox::initialize(state.path())
            .expect("reasonless quarantine should recover");

        assert_eq!(
            std::fs::read_to_string(state.path().join(format!("quarantine/{id}.reason")))
                .expect("repaired reason should read"),
            "incomplete-quarantine"
        );
    }

    #[test]
    /// Verifies recovery rejects a staged reason whose staging identifier is not a ULID.
    fn malformed_reason_stage_is_rejected() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let id = Ulid::new();
        for name in [
            format!("{id}.json"),
            format!("{id}.reason.stage.not-a-ulid"),
        ] {
            let path = state.path().join("processing").join(name);
            std::fs::write(&path, "{}").expect("processing fixture should write");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("processing permissions should set");
        }
        drop(inbox);

        let error = AgentHarnessFileInbox::initialize(state.path())
            .expect_err("malformed stage should fail");

        assert!(matches!(error, AgentHarnessFileInboxError::UnsafeEntry));
    }

    #[test]
    /// Verifies initialization rejects ready evidence readable by group or other users.
    fn initialize_rejects_wrong_ready_mode() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let path = state.path().join("inbox/event.json");
        std::fs::write(&path, valid_record()).expect("ready event should write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
            .expect("unsafe mode should set");
        drop(inbox);

        let error = AgentHarnessFileInbox::initialize(state.path())
            .expect_err("unsafe ready mode should fail");

        assert!(matches!(error, AgentHarnessFileInboxError::UnsafeEntry));
    }

    #[test]
    /// Verifies a recovered processing claim is returned before a newly ready file.
    fn recovered_processing_claim_precedes_ready_file() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let processing = state
            .path()
            .join(format!("processing/{}.json", Ulid::new()));
        std::fs::write(&processing, valid_record()).expect("processing event should write");
        std::fs::set_permissions(&processing, std::fs::Permissions::from_mode(0o600))
            .expect("processing permissions should set");
        write_ready(&state, "ready.json", "{not-json}");
        drop(inbox);
        let mut inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should recover");

        let claim = inbox
            .next_claim()
            .expect("claim should succeed")
            .expect("claim should exist");

        assert!(matches!(
            inbox.read_claim(&claim).expect("claim should decode"),
            ClaimedHarnessContent::Event(_)
        ));
    }

    #[test]
    /// Verifies recovery finishes a quarantine after its temporary reason was published.
    fn published_reason_temp_finishes_original_quarantine() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let id = Ulid::new();
        for (name, content) in [
            (format!("{id}.json"), "{not-json}"),
            (format!("{id}.reason.tmp"), "invalid-json"),
        ] {
            let path = state.path().join("processing").join(name);
            std::fs::write(&path, content).expect("processing fixture should write");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("processing permissions should set");
        }
        drop(inbox);

        let _recovered = AgentHarnessFileInbox::initialize(state.path())
            .expect("published reason should recover");

        assert!(state.path().join(format!("quarantine/{id}.json")).exists());
        assert_eq!(
            std::fs::read_to_string(state.path().join(format!("quarantine/{id}.reason")))
                .expect("recovered reason should read"),
            "invalid-json"
        );
    }

    #[test]
    /// Verifies the entry limit applies across all lifecycle directories, not per directory.
    fn combined_inventory_bound_is_enforced() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        drop(inbox);
        for index in 0..10_001 {
            let path = state.path().join(format!("inbox/{index}.tmp"));
            std::fs::write(&path, "").expect("temporary producer file should write");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("temporary permissions should set");
        }

        let error = AgentHarnessFileInbox::initialize(state.path())
            .expect_err("oversized inventory should fail");

        assert!(matches!(
            error,
            AgentHarnessFileInboxError::InspectDirectory
        ));
    }

    #[test]
    /// Verifies quarantine destination collisions fail without overwriting retained evidence.
    fn quarantine_destination_collision_never_overwrites() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        write_ready(&state, "invalid.json", "{not-json}");
        drop(inbox);
        let mut inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should rescan");
        let claim = inbox
            .next_claim()
            .expect("claim should succeed")
            .expect("claim should exist");
        let processing_name = std::fs::read_dir(state.path().join("processing"))
            .expect("processing should list")
            .next()
            .expect("processing entry should exist")
            .expect("processing entry should load")
            .file_name();
        let collision = state.path().join("quarantine").join(processing_name);
        std::fs::write(&collision, "preserve-me").expect("collision should write");
        std::fs::set_permissions(&collision, std::fs::Permissions::from_mode(0o600))
            .expect("collision permissions should set");

        let error = inbox
            .quarantine(claim, QuarantineReason::InvalidJson)
            .expect_err("quarantine collision should fail");

        assert!(matches!(error, AgentHarnessFileInboxError::Quarantine));
        assert_eq!(
            std::fs::read_to_string(collision).expect("collision should remain readable"),
            "preserve-me"
        );
    }

    #[test]
    /// Verifies recovery rejects a reason sidecar larger than the fixed read bound.
    fn oversized_reason_sidecar_is_rejected() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let id = Ulid::new();
        for (suffix, content) in [("json", "{}".to_owned()), ("reason", "x".repeat(65))] {
            let path = state.path().join(format!("quarantine/{id}.{suffix}"));
            std::fs::write(&path, content).expect("quarantine fixture should write");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("quarantine permissions should set");
        }
        drop(inbox);

        let error = AgentHarnessFileInbox::initialize(state.path())
            .expect_err("oversized reason should fail");

        assert!(matches!(error, AgentHarnessFileInboxError::UnsafeEntry));
    }

    #[test]
    /// Verifies a cross-device rename is classified as lacking required atomic semantics.
    fn cross_device_errno_is_unsupported() {
        assert!(super::implementation::unsupported_rename(
            rustix::io::Errno::XDEV
        ));
    }

    #[test]
    /// Verifies ready filenames containing characters outside the allowlist are rejected.
    fn unsafe_ready_filename_is_rejected() {
        let state = state_directory();
        let inbox =
            AgentHarnessFileInbox::initialize(state.path()).expect("file inbox should initialize");
        let path = state.path().join("inbox/bad name.json");
        std::fs::write(&path, valid_record()).expect("unsafe event should write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("unsafe event permissions should set");
        drop(inbox);

        let error = AgentHarnessFileInbox::initialize(state.path())
            .expect_err("unsafe filename should fail");

        assert!(matches!(error, AgentHarnessFileInboxError::UnsafeEntry));
    }

    #[test]
    /// Verifies regular-file validation rejects mismatched ownership and device identity.
    fn metadata_validator_rejects_wrong_owner_and_device() {
        let state = state_directory();
        let path = state.path().join("event.json");
        std::fs::write(&path, valid_record()).expect("event should write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("event permissions should set");
        let stat = rustix::fs::stat(&path).expect("event stat should load");
        let uid = stat.st_uid;
        let device = stat.st_dev as u64;

        assert!(
            super::implementation::validate_regular(
                &stat,
                uid.wrapping_add(1),
                device,
                0o600,
                true
            )
            .is_err()
        );
        assert!(
            super::implementation::validate_regular(
                &stat,
                uid,
                device.wrapping_add(1),
                0o600,
                true
            )
            .is_err()
        );
    }
}

use crate::model::{ProviderRefusal, ReconciledState, MAX_CHECKPOINT_LANES, MAX_JOURNAL_BYTES};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

const OWNER_LOCK_FILE: &str = ".owner.lock";
mod atomic;
mod record;
use record::{DurableApplying, DurableLine, DurableTerminal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyingRestore {
    pub(crate) operation: String,
    pub(crate) plan: String,
    pub(crate) provider: String,
    pub(crate) version: String,
    pub(crate) checkpoint: String,
    pub(crate) root_kind: RootKind,
    pub(crate) root: String,
    pub(crate) common_dir: Option<String>,
    pub(crate) git_dir: Option<String>,
    pub(crate) lanes: Vec<String>,
    pub(crate) permission: String,
    pub(crate) admitted_at_ms: i64,
    pub(crate) writes: u64,
    pub(crate) deletes: u64,
    pub(crate) bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootKind {
    Git,
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRestore {
    pub(crate) state: ReconciledState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredRestore {
    Applying(ApplyingRestore),
    Terminal {
        applying: ApplyingRestore,
        terminal: TerminalRestore,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginDisposition {
    Begun,
    AlreadyExists,
}

pub(crate) trait RestoreJournal: core::fmt::Debug + Send {
    fn begin(&mut self, record: &ApplyingRestore) -> Result<BeginDisposition, JournalError>;
    fn settle(&mut self, operation: &str, terminal: &TerminalRestore) -> Result<(), JournalError>;
    fn read(&self, operation: &str) -> Result<Option<StoredRestore>, JournalError>;
}

#[derive(Debug)]
pub struct FileRestoreJournal {
    root: PathBuf,
    owner: File,
    durability: Box<dyn JournalDurability>,
}

impl FileRestoreJournal {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, JournalError> {
        Self::open_with_durability(root, Box::new(FileJournalDurability))
    }

    pub(crate) fn open_with_durability(
        root: impl Into<PathBuf>,
        durability: Box<dyn JournalDurability>,
    ) -> Result<Self, JournalError> {
        let root = root.into();
        let metadata = match fs::symlink_metadata(&root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(JournalError::RootNotProvisioned);
            }
            Err(_) => return Err(JournalError::Unavailable),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(JournalError::RootNotProvisioned);
        }
        let owner_path = root.join(OWNER_LOCK_FILE);
        let owner = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(owner_path)
            .map_err(|_| JournalError::Unavailable)?;
        owner.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => JournalError::Busy,
            std::fs::TryLockError::Error(_) => JournalError::Unavailable,
        })?;
        Ok(Self {
            root,
            owner,
            durability,
        })
    }

    pub(crate) fn path(&self, operation: &str) -> PathBuf {
        let digest = Sha256::digest(operation.as_bytes());
        let mut name = String::with_capacity(digest.len() * 2 + 5);
        for byte in digest {
            use core::fmt::Write as _;
            let _ = write!(&mut name, "{byte:02x}");
        }
        name.push_str(".json");
        self.root.join(name)
    }

    fn raw(&self, operation: &str) -> Result<Option<Vec<u8>>, JournalError> {
        let mut file = match OpenOptions::new().read(true).open(self.path(operation)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(JournalError::Unavailable),
        };
        let initial = file
            .metadata()
            .map_err(|_| JournalError::Unavailable)?
            .len();
        let raw = read_hard_limited(&mut file, initial)?;
        let observed = file
            .metadata()
            .map_err(|_| JournalError::Unavailable)?
            .len();
        validate_observed_length(&raw, observed)?;
        Ok(Some(raw))
    }
}

impl RestoreJournal for FileRestoreJournal {
    fn begin(&mut self, record: &ApplyingRestore) -> Result<BeginDisposition, JournalError> {
        validate_applying(record)?;
        let durable = DurableLine::Applying(Box::new(DurableApplying::from(record)));
        let bytes = serde_json::to_vec(&durable).map_err(|_| JournalError::Corrupt)?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(JournalError::OverBound);
        }
        match self.read(&record.operation) {
            Ok(Some(_)) => return Ok(BeginDisposition::AlreadyExists),
            Ok(None) => {}
            Err(JournalError::Incomplete) => {
                // The engine calls begin only after a fresh plan, authority
                // and provider precondition admission. Never discard evidence.
                self.preserve_incomplete(&record.operation)?;
            }
            Err(error) => return Err(error),
        }
        self.publish(&self.path(&record.operation), &bytes)?;
        Ok(BeginDisposition::Begun)
    }

    fn settle(&mut self, operation: &str, terminal: &TerminalRestore) -> Result<(), JournalError> {
        let current = self.read(operation)?.ok_or(JournalError::Missing)?;
        let StoredRestore::Applying(applying) = current else {
            return Err(JournalError::Conflict);
        };
        let durable = DurableLine::Applying(Box::new(DurableApplying::from(&applying)));
        let mut bytes = serde_json::to_vec(&durable).map_err(|_| JournalError::Corrupt)?;
        bytes.push(b'\n');
        let durable = DurableLine::Terminal(DurableTerminal::from(terminal));
        bytes.extend(serde_json::to_vec(&durable).map_err(|_| JournalError::Corrupt)?);
        if self.raw(operation)?.is_some_and(|raw| raw.contains(&b'\n')) {
            self.preserve_incomplete(operation)?;
        }
        self.publish(&self.path(operation), &bytes)
    }

    fn read(&self, operation: &str) -> Result<Option<StoredRestore>, JournalError> {
        let Some(raw) = self.raw(operation)? else {
            return Ok(None);
        };
        let mut lines = raw.split(|byte| *byte == b'\n');
        let first = lines.next().ok_or(JournalError::Corrupt)?;
        let applying = match serde_json::from_slice::<DurableLine>(first).map_err(|error| {
            if error.is_eof() && !raw.contains(&b'\n') {
                JournalError::Incomplete
            } else {
                JournalError::Corrupt
            }
        })? {
            DurableLine::Applying(value) => ApplyingRestore::try_from(*value)?,
            DurableLine::Terminal(_) => return Err(JournalError::Corrupt),
        };
        if applying.operation != operation {
            return Err(JournalError::Corrupt);
        }
        let second = lines.next();
        if lines.next().is_some() {
            return Err(JournalError::Corrupt);
        }
        match second {
            None => Ok(Some(StoredRestore::Applying(applying))),
            Some([]) => Ok(Some(StoredRestore::Applying(applying))),
            Some(bytes) => {
                let terminal = match serde_json::from_slice::<DurableLine>(bytes) {
                    Err(error) if error.is_eof() => {
                        return Ok(Some(StoredRestore::Applying(applying)))
                    }
                    Err(_) => return Err(JournalError::Corrupt),
                    Ok(value) => match value {
                        DurableLine::Terminal(value) => TerminalRestore::try_from(value)?,
                        DurableLine::Applying(_) => return Err(JournalError::Corrupt),
                    },
                };
                Ok(Some(StoredRestore::Terminal { applying, terminal }))
            }
        }
    }
}

pub(crate) fn read_hard_limited<R: Read>(
    reader: &mut R,
    initial: u64,
) -> Result<Vec<u8>, JournalError> {
    if initial > MAX_JOURNAL_BYTES as u64 {
        return Err(JournalError::OverBound);
    }
    let mut raw = Vec::with_capacity(
        usize::try_from(initial)
            .unwrap_or(MAX_JOURNAL_BYTES + 1)
            .min(MAX_JOURNAL_BYTES + 1),
    );
    reader
        .take((MAX_JOURNAL_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| JournalError::Unavailable)?;
    if raw.len() > MAX_JOURNAL_BYTES {
        return Err(JournalError::OverBound);
    }
    Ok(raw)
}

pub(crate) fn validate_observed_length(raw: &[u8], observed: u64) -> Result<(), JournalError> {
    if observed > MAX_JOURNAL_BYTES as u64 {
        return Err(JournalError::OverBound);
    }
    if observed != raw.len() as u64 {
        return Err(JournalError::Conflict);
    }
    Ok(())
}

impl Drop for FileRestoreJournal {
    fn drop(&mut self) {
        let _ = self.owner.unlock();
    }
}

pub(crate) trait JournalDurability: core::fmt::Debug + Send {
    fn sync_parent(&self, root: &Path) -> Result<(), JournalError>;
}

#[derive(Debug)]
struct FileJournalDurability;

impl JournalDurability for FileJournalDurability {
    fn sync_parent(&self, root: &Path) -> Result<(), JournalError> {
        sync_directory_entry(root)
    }
}

#[cfg(unix)]
fn sync_directory_entry(root: &Path) -> Result<(), JournalError> {
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| JournalError::Unavailable)
}

#[cfg(windows)]
fn sync_directory_entry(root: &Path) -> Result<(), JournalError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const REPARSE: u32 = 0x0000_0400;
    const OPEN_REPARSE: u32 = 0x0020_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const SHARE_ALL: u32 = 7;

    // Mirrors grapt-cli/src/installation/windows_io.rs::sync_directory.
    // Checkpoint cannot depend on the CLI, and the shared domain types crate
    // owns no filesystem I/O. FlushFileBuffers needs GENERIC_WRITE: a read-only
    // handle or the staging file's pre-rename flush cannot persist this rename.
    let directory = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE)
        .share_mode(SHARE_ALL)
        .open(root)
        .map_err(|_| JournalError::Unavailable)?;
    let metadata = directory
        .metadata()
        .map_err(|_| JournalError::Unavailable)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.file_attributes() & REPARSE != 0
    {
        return Err(JournalError::Unavailable);
    }
    directory.sync_all().map_err(|_| JournalError::Unavailable)
}

#[cfg(not(any(unix, windows)))]
fn sync_directory_entry(_root: &Path) -> Result<(), JournalError> {
    Err(JournalError::Unavailable)
}

fn validate_applying(record: &ApplyingRestore) -> Result<(), JournalError> {
    let strings = [
        record.operation.as_str(),
        record.plan.as_str(),
        record.provider.as_str(),
        record.version.as_str(),
        record.checkpoint.as_str(),
        record.permission.as_str(),
    ];
    if strings
        .iter()
        .any(|value| value.is_empty() || value.len() > 256)
        || record.root.is_empty()
        || record.root.len() > 4096
        || record.lanes.is_empty()
        || record.lanes.len() > MAX_CHECKPOINT_LANES
        || record
            .lanes
            .iter()
            .any(|lane| lane.is_empty() || lane.len() > 256)
    {
        return Err(JournalError::OverBound);
    }
    match record.root_kind {
        RootKind::Git => {
            if record.common_dir.as_deref().is_none_or(str::is_empty)
                || record.git_dir.as_deref().is_none_or(str::is_empty)
            {
                return Err(JournalError::Corrupt);
            }
        }
        RootKind::Plain => {
            if record.common_dir.is_some() || record.git_dir.is_some() {
                return Err(JournalError::Corrupt);
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("restore journal root is not a preprovisioned ordinary directory")]
    RootNotProvisioned,
    #[error("restore journal is unavailable")]
    Unavailable,
    #[error("restore journal record is missing")]
    Missing,
    #[error("restore journal record is corrupt")]
    Corrupt,
    #[error(
        "restore journal initial record is incomplete; a fresh validated restore plan is required"
    )]
    Incomplete,
    #[error("restore journal record exceeds its bound")]
    OverBound,
    #[error("restore journal already has a durable owner")]
    Busy,
    #[error("restore journal transition conflicts with durable state")]
    Conflict,
}

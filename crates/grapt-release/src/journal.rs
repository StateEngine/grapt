use grapt_types::{ClockAuthority, MigrationId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

const CHECKPOINT_SCHEMA: &str = "secode.release.migration-checkpoint.v1";
const MAX_CHECKPOINT_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RehearsalPhase {
    SourceAudited,
    CloneCreated,
    BackupVerified,
    Migrated,
    MigrationCompared,
    RollbackRestored,
    RollbackCompared,
    ReadyForHandoff,
}

impl RehearsalPhase {
    pub const ALL: [Self; 8] = [
        Self::SourceAudited,
        Self::CloneCreated,
        Self::BackupVerified,
        Self::Migrated,
        Self::MigrationCompared,
        Self::RollbackRestored,
        Self::RollbackCompared,
        Self::ReadyForHandoff,
    ];

    pub fn ordinal(self) -> u8 {
        match self {
            Self::SourceAudited => 1,
            Self::CloneCreated => 2,
            Self::BackupVerified => 3,
            Self::Migrated => 4,
            Self::MigrationCompared => 5,
            Self::RollbackRestored => 6,
            Self::RollbackCompared => 7,
            Self::ReadyForHandoff => 8,
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::SourceAudited => "01-source-audited.json",
            Self::CloneCreated => "02-clone-created.json",
            Self::BackupVerified => "03-backup-verified.json",
            Self::Migrated => "04-migrated.json",
            Self::MigrationCompared => "05-migration-compared.json",
            Self::RollbackRestored => "06-rollback-restored.json",
            Self::RollbackCompared => "07-rollback-compared.json",
            Self::ReadyForHandoff => "08-ready-for-handoff.json",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct CheckpointEvidence {
    pub source_path: PathBuf,
    pub clone_path: PathBuf,
    pub backup_path: PathBuf,
    pub rollback_path: PathBuf,
    pub source_file_fingerprint: String,
    pub source_domain_digest: String,
    pub clone_file_fingerprint: Option<String>,
    pub clone_domain_digest: Option<String>,
    pub backup_file_fingerprint: Option<String>,
    pub backup_domain_digest: Option<String>,
    pub rollback_file_fingerprint: Option<String>,
    pub rollback_domain_digest: Option<String>,
    pub applied_migrations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Checkpoint {
    schema: String,
    run_id: String,
    ordinal: u8,
    phase: RehearsalPhase,
    recorded_ms: i64,
    previous_checkpoint_sha256: Option<String>,
    evidence: CheckpointEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RehearsalStatus {
    pub schema: &'static str,
    pub run_id: String,
    pub phase: Option<RehearsalPhase>,
    pub completed_checkpoints: usize,
    pub ready_for_handoff: bool,
    pub source_domain_digest: Option<String>,
    pub clone_domain_digest: Option<String>,
    pub rollback_domain_digest: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("migration checkpoint io: {0}")]
    Io(#[from] std::io::Error),
    #[error("migration checkpoint json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("checkpoint {path:?} contains {found} bytes above the {maximum}-byte bound")]
    TooLarge {
        path: PathBuf,
        maximum: u64,
        found: u64,
    },
    #[error("checkpoint {path:?} does not match this run or phase")]
    WrongIdentity { path: PathBuf },
    #[error("checkpoint {path:?} does not match the already-observed evidence")]
    EvidenceConflict { path: PathBuf },
    #[error("checkpoint chain is broken at {path:?}")]
    BrokenChain { path: PathBuf },
    #[error("checkpoint {later:?} exists after missing earlier phase {missing:?}")]
    Gap {
        missing: RehearsalPhase,
        later: PathBuf,
    },
    #[error("the checkpoint directory has no readable run identity")]
    MissingRunIdentity,
}

#[derive(Debug)]
pub(crate) struct Journal {
    directory: PathBuf,
    run_id: MigrationId,
    checkpoints: Vec<(Checkpoint, Vec<u8>)>,
}

impl Journal {
    pub(crate) fn open(root: &Path, run_id: MigrationId) -> Result<Self, JournalError> {
        let directory = root.join("checkpoints");
        std::fs::create_dir_all(&directory)?;
        let checkpoints = load_chain(&directory, Some(run_id.as_str()))?;
        Ok(Self {
            directory,
            run_id,
            checkpoints,
        })
    }

    pub(crate) fn last_evidence(&self) -> Option<&CheckpointEvidence> {
        self.checkpoints
            .last()
            .map(|(checkpoint, _)| &checkpoint.evidence)
    }

    pub(crate) fn has(&self, phase: RehearsalPhase) -> bool {
        self.checkpoints
            .iter()
            .any(|(checkpoint, _)| checkpoint.phase == phase)
    }

    pub(crate) fn record(
        &mut self,
        phase: RehearsalPhase,
        evidence: CheckpointEvidence,
        clock: &dyn ClockAuthority,
    ) -> Result<(), JournalError> {
        let expected = self.checkpoints.len().saturating_add(1);
        if usize::from(phase.ordinal()) != expected {
            if let Some((existing, _)) = self
                .checkpoints
                .iter()
                .find(|(checkpoint, _)| checkpoint.phase == phase)
            {
                if existing.evidence == evidence {
                    return Ok(());
                }
                return Err(JournalError::EvidenceConflict {
                    path: self.directory.join(phase.file_name()),
                });
            }
            return Err(JournalError::Gap {
                missing: RehearsalPhase::ALL[self.checkpoints.len()],
                later: self.directory.join(phase.file_name()),
            });
        }
        let previous_checkpoint_sha256 =
            self.checkpoints.last().map(|(_, bytes)| sha256_hex(bytes));
        let checkpoint = Checkpoint {
            schema: CHECKPOINT_SCHEMA.to_owned(),
            run_id: self.run_id.as_str().to_owned(),
            ordinal: phase.ordinal(),
            phase,
            recorded_ms: clock.now().as_millis(),
            previous_checkpoint_sha256,
            evidence,
        };
        let bytes = serde_json::to_vec(&checkpoint)?;
        if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
            return Err(JournalError::TooLarge {
                path: self.directory.join(phase.file_name()),
                maximum: MAX_CHECKPOINT_BYTES,
                found: bytes.len() as u64,
            });
        }
        let path = self.directory.join(phase.file_name());
        let pending = path.with_extension("json.pending");
        if pending.exists() {
            std::fs::remove_file(&pending)?;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending)
        {
            Ok(mut file) => {
                file.write_all(&bytes)?;
                file.sync_all()?;
                match std::fs::hard_link(&pending, &path) {
                    Ok(()) => std::fs::remove_file(&pending)?,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        std::fs::remove_file(&pending)?;
                        let existing = read_checkpoint(&path)?;
                        if existing != checkpoint {
                            return Err(JournalError::EvidenceConflict { path });
                        }
                        return Ok(());
                    }
                    Err(error) => return Err(JournalError::Io(error)),
                }
            }
            Err(error) => return Err(JournalError::Io(error)),
        }
        self.checkpoints.push((checkpoint, bytes));
        Ok(())
    }
}

pub(crate) fn inspect(root: &Path) -> Result<RehearsalStatus, JournalError> {
    let directory = root.join("checkpoints");
    let checkpoints = load_chain(&directory, None)?;
    let run_id = checkpoints
        .first()
        .map(|(checkpoint, _)| checkpoint.run_id.as_str())
        .ok_or(JournalError::MissingRunIdentity)?;
    Ok(status_from(&checkpoints, run_id))
}

fn status_from(checkpoints: &[(Checkpoint, Vec<u8>)], run_id: &str) -> RehearsalStatus {
    let evidence = checkpoints
        .last()
        .map(|(checkpoint, _)| &checkpoint.evidence);
    RehearsalStatus {
        schema: "grapt.release.migration-status.v1",
        run_id: run_id.to_owned(),
        phase: checkpoints.last().map(|(checkpoint, _)| checkpoint.phase),
        completed_checkpoints: checkpoints.len(),
        ready_for_handoff: checkpoints
            .iter()
            .any(|(checkpoint, _)| checkpoint.phase == RehearsalPhase::ReadyForHandoff),
        source_domain_digest: evidence.map(|value| value.source_domain_digest.clone()),
        clone_domain_digest: evidence.and_then(|value| value.clone_domain_digest.clone()),
        rollback_domain_digest: evidence.and_then(|value| value.rollback_domain_digest.clone()),
    }
}

fn load_chain(
    directory: &Path,
    expected_run_id: Option<&str>,
) -> Result<Vec<(Checkpoint, Vec<u8>)>, JournalError> {
    let mut checkpoints = Vec::new();
    let mut missing = None;
    for phase in RehearsalPhase::ALL {
        let path = directory.join(phase.file_name());
        if !path.exists() {
            if missing.is_none() {
                missing = Some(phase);
            }
            continue;
        }
        if let Some(missing) = missing {
            return Err(JournalError::Gap {
                missing,
                later: path,
            });
        }
        let bytes = read_bounded(&path)?;
        let checkpoint: Checkpoint = serde_json::from_slice(&bytes)?;
        if checkpoint.schema != CHECKPOINT_SCHEMA
            || checkpoint.ordinal != phase.ordinal()
            || checkpoint.phase != phase
            || expected_run_id
                .map(|expected| checkpoint.run_id != expected)
                .unwrap_or(false)
        {
            return Err(JournalError::WrongIdentity { path });
        }
        let expected_previous = checkpoints
            .last()
            .map(|(_, previous_bytes): &(Checkpoint, Vec<u8>)| sha256_hex(previous_bytes));
        if checkpoint.previous_checkpoint_sha256 != expected_previous {
            return Err(JournalError::BrokenChain { path });
        }
        checkpoints.push((checkpoint, bytes));
    }
    Ok(checkpoints)
}

fn read_checkpoint(path: &Path) -> Result<Checkpoint, JournalError> {
    let bytes = read_bounded(path)?;
    serde_json::from_slice(&bytes).map_err(JournalError::from)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, JournalError> {
    let file = std::fs::File::open(path)?;
    let found = file.metadata()?.len();
    if found > MAX_CHECKPOINT_BYTES {
        return Err(JournalError::TooLarge {
            path: path.to_path_buf(),
            maximum: MAX_CHECKPOINT_BYTES,
            found,
        });
    }
    let mut bytes = Vec::with_capacity(found as usize);
    file.take(MAX_CHECKPOINT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err(JournalError::TooLarge {
            path: path.to_path_buf(),
            maximum: MAX_CHECKPOINT_BYTES,
            found: bytes.len() as u64,
        });
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

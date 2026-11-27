//! Deterministic external-provider fixture for checkpoint acceptance.
//!
//! This module is feature-gated and is not a product snapshot engine. Its
//! provider directory is deliberately separate from the disposable workspace;
//! the product reaches it only through `grapt-checkpoint`'s public provider
//! contract. It exists to execute the real product synchronization boundary.

use crate::checkpoint::{CheckpointProductError, WorkspaceCheckpointProduct};
use grapt_checkpoint::{
    AuthorityEvidence, CheckpointEngine, CheckpointError, CheckpointProvider, CheckpointSelector,
    FileRestoreJournal, ProviderApplyOutcome, ProviderApplyReceipt, ProviderCaptureOutcome,
    ProviderContract, ProviderForkOutcome, ProviderInspection, ProviderLane,
    ProviderOperationState, ProviderPlan, ProviderPreconditionObservation, ProviderRefusal,
    ProviderResolveOutcome, RescueSupport, RestoreAuthorityDecision, RestoreAuthorityPort,
    VcsObserver, WorkspaceObservation,
};
use grapt_types::{
    CheckpointFidelity, CheckpointHeadId, CheckpointLaneId, CheckpointLineage,
    CheckpointOperationId, CheckpointProviderId, CheckpointProviderVersion, ClockAuthority,
    IdAuthority, IdToken, PathIdentity, PermissionId, RealizedEnforcement,
    RestoreApplicationAdmission, RestoreConsequences, SettledRestorePlan, Timestamp, VcsBaseline,
    WorkspaceCheckpointId, WorkspaceCheckpointRef, WorktreeIdentity,
};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

mod git;

const MAX_FIXTURE_BYTES: usize = 4096;
const MAX_FIXTURE_CHECKPOINTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixtureStepReceipt {
    pub seq: u64,
    pub operation: String,
    pub outcome: String,
    /// The VCS baseline a capture pinned, when the step captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn run_fixture_corpus(
    base: &Path,
    actions_path: &Path,
) -> Result<Vec<FixtureStepReceipt>, FixtureError> {
    let workspace = base.join("workspace");
    let provider_root = base.join("external-provider");
    let journal_root = base.join("restore-journal");
    fs::create_dir_all(&workspace).map_err(|_| FixtureError::WorkspaceUnavailable)?;
    fs::create_dir_all(&provider_root).map_err(|_| FixtureError::ProviderUnavailable)?;
    fs::create_dir_all(&journal_root).map_err(|_| FixtureError::JournalUnavailable)?;
    let raw = fs::read(actions_path).map_err(|_| FixtureError::CorpusUnreadable)?;
    if raw.len() > 64 * 1024 {
        return Err(FixtureError::CorpusOverBound);
    }
    let text = std::str::from_utf8(&raw).map_err(|_| FixtureError::CorpusInvalid)?;
    let actions = text
        .lines()
        .map(|line| {
            serde_json::from_str::<FixtureAction>(line).map_err(|_| FixtureError::CorpusInvalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if actions.is_empty() || actions.len() > 32 {
        return Err(FixtureError::CorpusOverBound);
    }

    let mut product = Some(open_fixture_product(
        &workspace,
        &provider_root,
        &journal_root,
        1_700_000_000_000,
        FixtureApplyMode::InterruptAfterApply,
    )?);
    let workspace_identity = fs::canonicalize(&workspace)
        .map_err(|_| FixtureError::WorkspaceUnavailable)?
        .to_str()
        .map(normalize_path)
        .ok_or(FixtureError::WorkspaceUnavailable)?;
    let mut checkpoint = None;
    let mut plan = None;
    let mut last_commit = None;
    let mut receipts = Vec::with_capacity(actions.len());
    let mut previous_seq = 0;
    for action in actions {
        let seq = action.seq();
        if seq != previous_seq + 1 {
            return Err(FixtureError::CorpusInvalid);
        }
        previous_seq = seq;
        let mut baseline = None;
        let step = || -> Result<(&str, &str), FixtureError> {
            Ok(match action {
                FixtureAction::Write { value, .. } => {
                    write_fixture_content(&workspace.join("note.txt"), value.as_bytes())?;
                    ("write", "written")
                }
                FixtureAction::Capture {
                    operation,
                    expected,
                    ..
                } => {
                    let active = product.as_ref().ok_or(FixtureError::ProductUnavailable)?;
                    let receipt = active.capture(
                        CheckpointOperationId::new(&operation)
                            .ok_or(FixtureError::CorpusInvalid)?,
                        PathIdentity::canonical(&workspace_identity),
                        vec![CheckpointLaneId::new("files").ok_or(FixtureError::CorpusInvalid)?],
                    )?;
                    let observed = match receipt.outcome() {
                        grapt_types::CheckpointCaptureOutcome::Captured { anchor, .. }
                            if anchor.fidelity().is_exact() =>
                        {
                            checkpoint = Some(anchor.checkpoint().clone());
                            baseline = Some(git::describe(anchor.baseline()));
                            "captured"
                        }
                        grapt_types::CheckpointCaptureOutcome::Refused { .. } => "refused",
                        _ => return Err(FixtureError::UnexpectedOutcome),
                    };
                    if observed != expected.as_deref().unwrap_or("captured") {
                        return Err(FixtureError::UnexpectedOutcome);
                    }
                    ("capture", observed)
                }
                FixtureAction::Mutate { value, .. } => {
                    write_fixture_content(&workspace.join("note.txt"), value.as_bytes())?;
                    ("mutate", "mutated")
                }
                FixtureAction::Plan {
                    operation,
                    expected_writes,
                    expected_deletes,
                    expected_bytes,
                    ..
                } => {
                    let active = product.as_ref().ok_or(FixtureError::ProductUnavailable)?;
                    let source = checkpoint.clone().ok_or(FixtureError::MissingCheckpoint)?;
                    let planned =
                        active.plan_restore(grapt_checkpoint::RestorePlanRequest::new(
                            CheckpointOperationId::new(operation)
                                .ok_or(FixtureError::CorpusInvalid)?,
                            source,
                            PathIdentity::canonical(&workspace_identity),
                            vec![CheckpointLaneId::new("files")
                                .ok_or(FixtureError::CorpusInvalid)?],
                            Timestamp::from_millis(1_700_000_060_000),
                            RealizedEnforcement::HarnessOnly,
                        )?)?;
                    if planned.consequences().writes() != expected_writes
                        || planned.consequences().deletes() != expected_deletes
                        || planned.consequences().bytes() != expected_bytes
                    {
                        return Err(FixtureError::UnexpectedOutcome);
                    }
                    plan = Some(planned);
                    ("plan", "planned")
                }
                FixtureAction::Apply { expected, .. } => {
                    let active = product.as_ref().ok_or(FixtureError::ProductUnavailable)?;
                    let planned = plan.take().ok_or(FixtureError::MissingPlan)?;
                    let observed = match active.apply_restore(planned)? {
                        grapt_checkpoint::RestoreApplyOutcome::Applied(_) => "applied",
                        grapt_checkpoint::RestoreApplyOutcome::Refused(_) => "refused",
                        grapt_checkpoint::RestoreApplyOutcome::Ambiguous { .. } => "ambiguous",
                        grapt_checkpoint::RestoreApplyOutcome::Quarantined(_) => "quarantined",
                    };
                    if observed != expected {
                        return Err(FixtureError::UnexpectedOutcome);
                    }
                    ("apply", observed)
                }
                FixtureAction::Restart { .. } => {
                    let previous = product.take();
                    drop(previous);
                    product = Some(open_fixture_product(
                        &workspace,
                        &provider_root,
                        &journal_root,
                        1_700_000_000_001,
                        FixtureApplyMode::Complete,
                    )?);
                    ("restart", "reopened")
                }
                FixtureAction::Recover {
                    operation,
                    expected,
                    ..
                } => {
                    let active = product.as_ref().ok_or(FixtureError::ProductUnavailable)?;
                    let operation =
                        CheckpointOperationId::new(operation).ok_or(FixtureError::CorpusInvalid)?;
                    let observed = match active.recover(&operation) {
                        Err(CheckpointProductError::Checkpoint(CheckpointError::Journal(_))) => {
                            "corrupt"
                        }
                        Err(error) => return Err(error.into()),
                        Ok(grapt_checkpoint::RestoreRecoveryOutcome::Applied(_)) => "applied",
                        Ok(grapt_checkpoint::RestoreRecoveryOutcome::Refused(_)) => "refused",
                        Ok(grapt_checkpoint::RestoreRecoveryOutcome::Quarantined(_)) => {
                            "quarantined"
                        }
                    };
                    if observed != expected {
                        return Err(FixtureError::UnexpectedOutcome);
                    }
                    ("recover", observed)
                }
                FixtureAction::Read { expected, .. } => {
                    let observed =
                        PersistentFixtureProvider::read_bounded(&workspace.join("note.txt"))
                            .map_err(|_| FixtureError::WorkspaceUnavailable)?;
                    if observed != expected.as_bytes() {
                        return Err(FixtureError::UnexpectedOutcome);
                    }
                    ("read", "matched")
                }
                FixtureAction::GitInit { .. } => {
                    git::init(&workspace)?;
                    ("git-init", "initialized")
                }
                FixtureAction::GitCommit { message, .. } => {
                    last_commit = Some(git::commit(&workspace, &message)?);
                    ("git-commit", "committed")
                }
                FixtureAction::VcsRead { expected, .. } => {
                    let observed = git::observe(&workspace);
                    let matched = match expected.as_str() {
                        // Nothing but an explicit corpus commit may move HEAD.
                        "last-commit" => {
                            last_commit.is_some() && observed.head() == last_commit.as_ref()
                        }
                        "not-versioned" => observed == VcsBaseline::NotVersioned,
                        _ => return Err(FixtureError::CorpusInvalid),
                    };
                    if !matched {
                        return Err(FixtureError::UnexpectedOutcome);
                    }
                    ("vcs-read", "matched")
                }
                FixtureAction::Negotiate {
                    version, expected, ..
                } => {
                    let active = product.as_ref().ok_or(FixtureError::ProductUnavailable)?;
                    let observed = match active.negotiate(
                        &CheckpointProviderVersion::new(&version)
                            .ok_or(FixtureError::CorpusInvalid)?,
                        vec![CheckpointLaneId::new("files").ok_or(FixtureError::CorpusInvalid)?],
                    ) {
                        Ok(_) => "negotiated",
                        Err(CheckpointProductError::Checkpoint(
                            CheckpointError::ProviderContractMismatch,
                        )) => "refused",
                        Err(error) => return Err(error.into()),
                    };
                    if observed != expected {
                        return Err(FixtureError::UnexpectedOutcome);
                    }
                    ("negotiate", observed)
                }
                FixtureAction::CorruptJournal { operation, .. } => {
                    // The journal names records by its own scheme; the fixture finds
                    // the record that carries this operation rather than rederiving it.
                    let needle = format!("\"{operation}\"");
                    let mut corrupted = 0_u32;
                    for entry in
                        fs::read_dir(&journal_root).map_err(|_| FixtureError::JournalUnavailable)?
                    {
                        let path = entry.map_err(|_| FixtureError::JournalUnavailable)?.path();
                        if path
                            .extension()
                            .is_some_and(|extension| extension == "json")
                            && PersistentFixtureProvider::read_bounded(&path).is_ok_and(|bytes| {
                                String::from_utf8_lossy(&bytes).contains(&needle)
                            })
                        {
                            write_fixture_content(&path, b"{not a journal record")?;
                            corrupted += 1;
                        }
                    }
                    if corrupted != 1 {
                        return Err(FixtureError::UnexpectedOutcome);
                    }
                    ("corrupt-journal", "corrupted")
                }
            })
        };
        let (operation, outcome) = step().map_err(|error| match error {
            FixtureError::UnexpectedOutcome => FixtureError::StepOutcome { seq },
            other => other,
        })?;
        receipts.push(FixtureStepReceipt {
            seq,
            operation: operation.to_owned(),
            outcome: outcome.to_owned(),
            baseline,
        });
    }
    Ok(receipts)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
enum FixtureAction {
    Write {
        seq: u64,
        value: String,
    },
    Capture {
        seq: u64,
        operation: String,
        #[serde(default)]
        expected: Option<String>,
    },
    Mutate {
        seq: u64,
        value: String,
    },
    Plan {
        seq: u64,
        operation: String,
        expected_writes: u64,
        expected_deletes: u64,
        expected_bytes: u64,
    },
    Apply {
        seq: u64,
        expected: String,
    },
    Restart {
        seq: u64,
    },
    Recover {
        seq: u64,
        operation: String,
        expected: String,
    },
    Read {
        seq: u64,
        expected: String,
    },
    GitInit {
        seq: u64,
    },
    GitCommit {
        seq: u64,
        message: String,
    },
    VcsRead {
        seq: u64,
        expected: String,
    },
    Negotiate {
        seq: u64,
        version: String,
        expected: String,
    },
    CorruptJournal {
        seq: u64,
        operation: String,
    },
}

impl FixtureAction {
    fn seq(&self) -> u64 {
        match self {
            Self::Write { seq, .. }
            | Self::Capture { seq, .. }
            | Self::Mutate { seq, .. }
            | Self::Plan { seq, .. }
            | Self::Apply { seq, .. }
            | Self::Restart { seq }
            | Self::Recover { seq, .. }
            | Self::Read { seq, .. }
            | Self::GitInit { seq }
            | Self::GitCommit { seq, .. }
            | Self::VcsRead { seq, .. }
            | Self::Negotiate { seq, .. }
            | Self::CorruptJournal { seq, .. } => *seq,
        }
    }
}

fn write_fixture_content(path: &Path, bytes: &[u8]) -> Result<(), FixtureError> {
    if bytes.len() > MAX_FIXTURE_BYTES {
        return Err(FixtureError::CorpusOverBound);
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|_| FixtureError::WorkspaceUnavailable)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| FixtureError::WorkspaceUnavailable)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureApplyMode {
    Complete,
    InterruptAfterApply,
    RemainAmbiguous,
}

pub fn open_fixture_product(
    workspace: &Path,
    provider_root: &Path,
    journal_root: &Path,
    now_ms: i64,
    apply_mode: FixtureApplyMode,
) -> Result<WorkspaceCheckpointProduct, FixtureError> {
    fs::create_dir_all(workspace).map_err(|_| FixtureError::WorkspaceUnavailable)?;
    fs::create_dir_all(provider_root).map_err(|_| FixtureError::ProviderUnavailable)?;
    fs::create_dir_all(journal_root).map_err(|_| FixtureError::JournalUnavailable)?;
    let workspace = fs::canonicalize(workspace).map_err(|_| FixtureError::WorkspaceUnavailable)?;
    let provider_root =
        fs::canonicalize(provider_root).map_err(|_| FixtureError::ProviderUnavailable)?;
    let journal_root =
        fs::canonicalize(journal_root).map_err(|_| FixtureError::JournalUnavailable)?;
    if provider_root.starts_with(&workspace) || journal_root.starts_with(&workspace) {
        return Err(FixtureError::StorageInsideWorkspace);
    }
    let observer = PlainFixtureObserver::new(&workspace)?;
    let provider = PersistentFixtureProvider::open(provider_root, workspace, apply_mode)?;
    let journal = FileRestoreJournal::open(journal_root).map_err(CheckpointError::from)?;
    let engine = CheckpointEngine::new(
        Box::new(provider),
        Box::new(observer),
        journal,
        Box::new(FixtureAuthority),
        Arc::new(FixtureClock(now_ms)),
        Arc::new(FixtureIds::default()),
    )?;
    Ok(WorkspaceCheckpointProduct::new(engine))
}

#[derive(Debug)]
struct PlainFixtureObserver {
    root: PathIdentity,
    workspace: PathBuf,
}

impl PlainFixtureObserver {
    fn new(workspace: &Path) -> Result<Self, FixtureError> {
        let root = workspace
            .to_str()
            .map(normalize_path)
            .ok_or(FixtureError::WorkspaceUnavailable)?;
        Ok(Self {
            root: PathIdentity::canonical(root),
            workspace: workspace.to_path_buf(),
        })
    }
}

impl VcsObserver for PlainFixtureObserver {
    fn observe(&mut self, _root: &PathIdentity) -> WorkspaceObservation {
        WorkspaceObservation::new(
            WorktreeIdentity::Plain {
                root: self.root.clone(),
            },
            git::observe(&self.workspace),
        )
    }
}

struct PersistentFixtureProvider {
    provider_root: PathBuf,
    workspace: PathBuf,
    apply_mode: FixtureApplyMode,
}

impl core::fmt::Debug for PersistentFixtureProvider {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PersistentFixtureProvider")
            .field("provider_root", &"<external fixture storage>")
            .field("workspace", &"<disposable workspace>")
            .field("apply_mode", &self.apply_mode)
            .finish()
    }
}

impl PersistentFixtureProvider {
    fn open(
        provider_root: PathBuf,
        workspace: PathBuf,
        apply_mode: FixtureApplyMode,
    ) -> Result<Self, FixtureError> {
        Ok(Self {
            provider_root,
            workspace,
            apply_mode,
        })
    }

    fn content_path(&self) -> PathBuf {
        self.workspace.join("note.txt")
    }

    fn checkpoint_path(
        &self,
        checkpoint: &WorkspaceCheckpointId,
    ) -> Result<PathBuf, ProviderRefusal> {
        path_safe(checkpoint.as_str())?;
        Ok(self
            .provider_root
            .join(format!("{}.payload", checkpoint.as_str())))
    }

    fn operation_path(
        &self,
        operation: &CheckpointOperationId,
    ) -> Result<PathBuf, ProviderRefusal> {
        path_safe(operation.as_str())?;
        Ok(self
            .provider_root
            .join(format!("operation-{}.receipt", operation.as_str())))
    }

    fn read_bounded(path: &Path) -> Result<Vec<u8>, ProviderRefusal> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|_| ProviderRefusal::Unavailable)?;
        let size = file
            .metadata()
            .map_err(|_| ProviderRefusal::Unavailable)?
            .len();
        if size > MAX_FIXTURE_BYTES as u64 {
            return Err(ProviderRefusal::Conflict);
        }
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)
            .map_err(|_| ProviderRefusal::Unavailable)?;
        Ok(bytes)
    }

    fn next_checkpoint(&self) -> Result<WorkspaceCheckpointId, ProviderRefusal> {
        let count = fs::read_dir(&self.provider_root)
            .map_err(|_| ProviderRefusal::Unavailable)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".payload"))
            .take(MAX_FIXTURE_CHECKPOINTS + 1)
            .count();
        if count >= MAX_FIXTURE_CHECKPOINTS {
            return Err(ProviderRefusal::Conflict);
        }
        WorkspaceCheckpointId::new(format!("fixture-{}", count + 1))
            .ok_or(ProviderRefusal::Conflict)
    }

    fn find_latest(&self) -> Result<WorkspaceCheckpointId, ProviderRefusal> {
        let mut latest = None;
        for ordinal in 1..=MAX_FIXTURE_CHECKPOINTS {
            let checkpoint = WorkspaceCheckpointId::new(format!("fixture-{ordinal}"))
                .ok_or(ProviderRefusal::Conflict)?;
            if self.checkpoint_path(&checkpoint)?.exists() {
                latest = Some(checkpoint);
            }
        }
        latest.ok_or(ProviderRefusal::CheckpointUnknown)
    }

    fn write_operation(
        &self,
        operation: &CheckpointOperationId,
        consequences: &RestoreConsequences,
        state: &str,
    ) -> Result<(), ProviderRefusal> {
        let path = self.operation_path(operation)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .map_err(|_| ProviderRefusal::Unavailable)?;
        write!(
            file,
            "{state}|{}|{}|{}",
            consequences.writes(),
            consequences.deletes(),
            consequences.bytes()
        )
        .map_err(|_| ProviderRefusal::Unavailable)?;
        file.sync_all().map_err(|_| ProviderRefusal::Unavailable)
    }

    fn read_operation(
        &self,
        operation: &CheckpointOperationId,
    ) -> Result<ProviderOperationState, ProviderRefusal> {
        let path = self.operation_path(operation)?;
        if !path.exists() {
            return Ok(ProviderOperationState::Missing);
        }
        let raw =
            String::from_utf8(Self::read_bounded(&path)?).map_err(|_| ProviderRefusal::Conflict)?;
        let mut parts = raw.split('|');
        let state = parts.next().ok_or(ProviderRefusal::Conflict)?;
        let writes = parse_count(parts.next())?;
        let deletes = parse_count(parts.next())?;
        let bytes = parse_count(parts.next())?;
        if parts.next().is_some() {
            return Err(ProviderRefusal::Conflict);
        }
        let receipt = ProviderApplyReceipt::new(
            operation.clone(),
            RestoreConsequences::new(writes, deletes, bytes),
        );
        match state {
            "applied" => Ok(ProviderOperationState::Applied(receipt)),
            "ambiguous" => Ok(ProviderOperationState::Ambiguous),
            _ => Err(ProviderRefusal::Conflict),
        }
    }

    fn replace_content(&self, bytes: &[u8]) -> Result<(), ProviderRefusal> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(self.content_path())
            .map_err(|_| ProviderRefusal::Unavailable)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ProviderRefusal::Unavailable)
    }
}

impl CheckpointProvider for PersistentFixtureProvider {
    fn contract(&self) -> Result<ProviderContract, CheckpointError> {
        ProviderContract::new(
            CheckpointProviderId::new("fixture-provider")
                .ok_or(CheckpointError::InvalidProviderContract)?,
            CheckpointProviderVersion::new("1").ok_or(CheckpointError::InvalidProviderContract)?,
            vec![ProviderLane::new(
                CheckpointLaneId::new("files").ok_or(CheckpointError::InvalidProviderContract)?,
                true,
                true,
            )],
            RescueSupport::Supported,
        )
    }

    fn capture(
        &mut self,
        _operation: &CheckpointOperationId,
        _root: &WorktreeIdentity,
        _lanes: &[CheckpointLaneId],
    ) -> ProviderCaptureOutcome {
        let checkpoint = match self.next_checkpoint() {
            Ok(checkpoint) => checkpoint,
            Err(_) => {
                return ProviderCaptureOutcome::Refused {
                    reason: grapt_types::CaptureRefusalReason::ProviderUnavailable,
                };
            }
        };
        let bytes = match Self::read_bounded(&self.content_path()) {
            Ok(bytes) => bytes,
            Err(_) => {
                return ProviderCaptureOutcome::Refused {
                    reason: grapt_types::CaptureRefusalReason::ProviderUnavailable,
                };
            }
        };
        let path = match self.checkpoint_path(&checkpoint) {
            Ok(path) => path,
            Err(_) => {
                return ProviderCaptureOutcome::Refused {
                    reason: grapt_types::CaptureRefusalReason::ProviderUnavailable,
                };
            }
        };
        let mut file = match OpenOptions::new().create_new(true).write(true).open(path) {
            Ok(file) => file,
            Err(_) => {
                return ProviderCaptureOutcome::Refused {
                    reason: grapt_types::CaptureRefusalReason::ProviderUnavailable,
                };
            }
        };
        if file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .is_err()
        {
            return ProviderCaptureOutcome::Refused {
                reason: grapt_types::CaptureRefusalReason::ProviderUnavailable,
            };
        }
        ProviderCaptureOutcome::Captured {
            checkpoint,
            lineage: CheckpointLineage::Root,
            fidelity: CheckpointFidelity::Exact,
        }
    }

    fn resolve(&mut self, selector: &CheckpointSelector) -> ProviderResolveOutcome {
        let result = match selector {
            CheckpointSelector::Immutable(reference) => Ok(reference.checkpoint().clone()),
            CheckpointSelector::Head { .. } => self.find_latest(),
        };
        match result {
            Ok(checkpoint) => ProviderResolveOutcome::Resolved { checkpoint },
            Err(reason) => ProviderResolveOutcome::Refused { reason },
        }
    }

    fn inspect(
        &mut self,
        checkpoint: &WorkspaceCheckpointRef,
    ) -> Result<ProviderInspection, ProviderRefusal> {
        if !self.checkpoint_path(checkpoint.checkpoint())?.exists() {
            return Err(ProviderRefusal::CheckpointUnknown);
        }
        Ok(ProviderInspection::new(
            checkpoint.checkpoint().clone(),
            vec![CheckpointLaneId::new("files").ok_or(ProviderRefusal::LaneUnknown)?],
            CheckpointFidelity::Exact,
            CheckpointLineage::Root,
        ))
    }

    fn fork(
        &mut self,
        _operation: &CheckpointOperationId,
        parent: &WorkspaceCheckpointRef,
        head: &CheckpointHeadId,
    ) -> ProviderForkOutcome {
        let result = (|| {
            let bytes = Self::read_bounded(&self.checkpoint_path(parent.checkpoint())?)?;
            let checkpoint = self.next_checkpoint()?;
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(self.checkpoint_path(&checkpoint)?)
                .map_err(|_| ProviderRefusal::Unavailable)?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| ProviderRefusal::Unavailable)?;
            Ok::<_, ProviderRefusal>(checkpoint)
        })();
        match result {
            Ok(checkpoint) => ProviderForkOutcome::Forked {
                checkpoint,
                head: head.clone(),
            },
            Err(reason) => ProviderForkOutcome::Refused { reason },
        }
    }

    fn plan_restore(
        &mut self,
        _operation: &CheckpointOperationId,
        checkpoint: &WorkspaceCheckpointRef,
        _root: &WorktreeIdentity,
        _lanes: &[CheckpointLaneId],
    ) -> Result<ProviderPlan, ProviderRefusal> {
        let checkpoint_bytes = Self::read_bounded(&self.checkpoint_path(checkpoint.checkpoint())?)?;
        let current = Self::read_bounded(&self.content_path())?;
        let writes = u64::from(current != checkpoint_bytes);
        ProviderPlan::new(
            RestoreConsequences::new(writes, 0, checkpoint_bytes.len() as u64),
            Vec::new(),
            Vec::new(),
        )
        .map_err(|_| ProviderRefusal::Conflict)
    }

    fn observe_preconditions(
        &mut self,
        plan: &SettledRestorePlan,
    ) -> Result<ProviderPreconditionObservation, ProviderRefusal> {
        ProviderPreconditionObservation::new(plan.preconditions().to_vec())
            .map_err(|_| ProviderRefusal::Conflict)
    }

    fn apply_restore(&mut self, admission: RestoreApplicationAdmission) -> ProviderApplyOutcome {
        let plan = admission.plan();
        let bytes = match Self::read_bounded(&match self
            .checkpoint_path(plan.checkpoint().checkpoint())
        {
            Ok(path) => path,
            Err(reason) => return ProviderApplyOutcome::Refused(reason),
        }) {
            Ok(bytes) => bytes,
            Err(reason) => return ProviderApplyOutcome::Refused(reason),
        };
        if self.replace_content(&bytes).is_err() {
            return ProviderApplyOutcome::Refused(ProviderRefusal::Unavailable);
        }
        let receipt =
            ProviderApplyReceipt::new(plan.operation().clone(), plan.consequences().clone());
        match self.apply_mode {
            FixtureApplyMode::Complete => {
                if self
                    .write_operation(plan.operation(), plan.consequences(), "applied")
                    .is_err()
                {
                    return ProviderApplyOutcome::Interrupted;
                }
                ProviderApplyOutcome::Applied(receipt)
            }
            FixtureApplyMode::InterruptAfterApply => {
                let _written =
                    self.write_operation(plan.operation(), plan.consequences(), "applied");
                ProviderApplyOutcome::Interrupted
            }
            FixtureApplyMode::RemainAmbiguous => {
                let _written =
                    self.write_operation(plan.operation(), plan.consequences(), "ambiguous");
                ProviderApplyOutcome::Interrupted
            }
        }
    }

    fn inspect_operation(&mut self, operation: &CheckpointOperationId) -> ProviderOperationState {
        self.read_operation(operation)
            .unwrap_or(ProviderOperationState::Ambiguous)
    }

    fn reconcile_operation(&mut self, operation: &CheckpointOperationId) -> ProviderOperationState {
        self.inspect_operation(operation)
    }
}

#[derive(Debug)]
struct FixtureAuthority;

impl RestoreAuthorityPort for FixtureAuthority {
    fn decide(&mut self, plan: &SettledRestorePlan) -> RestoreAuthorityDecision {
        let permission = match PermissionId::from_str("per_checkpoint_fixture") {
            Ok(permission) => permission,
            Err(_) => return RestoreAuthorityDecision::Refused,
        };
        RestoreAuthorityDecision::Granted {
            capability: plan.required_capability().clone(),
            evidence: AuthorityEvidence::new(permission),
        }
    }
}

#[derive(Debug)]
struct FixtureClock(i64);

impl ClockAuthority for FixtureClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

#[derive(Debug, Default)]
struct FixtureIds(AtomicU64);

impl IdAuthority for FixtureIds {
    fn mint(&self) -> IdToken {
        let next = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        match IdToken::new(format!("fixture-{next}")) {
            Some(token) => token,
            None => panic!("deterministic fixture identity construction failed"),
        }
    }
}

fn parse_count(raw: Option<&str>) -> Result<u64, ProviderRefusal> {
    raw.ok_or(ProviderRefusal::Conflict)?
        .parse()
        .map_err(|_| ProviderRefusal::Conflict)
}

fn path_safe(raw: &str) -> Result<(), ProviderRefusal> {
    if raw.is_empty()
        || raw.len() > 128
        || raw
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'-' && byte != b'_')
    {
        return Err(ProviderRefusal::Conflict);
    }
    Ok(())
}

fn normalize_path(raw: &str) -> String {
    raw.replace('\\', "/")
}

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("disposable workspace is unavailable")]
    WorkspaceUnavailable,
    #[error("external fixture provider storage is unavailable")]
    ProviderUnavailable,
    #[error("restore journal is unavailable")]
    JournalUnavailable,
    #[error("provider or journal storage must not live inside the workspace")]
    StorageInsideWorkspace,
    #[error("checkpoint acceptance corpus is unreadable")]
    CorpusUnreadable,
    #[error("checkpoint acceptance corpus is invalid")]
    CorpusInvalid,
    #[error("checkpoint acceptance corpus exceeds its bound")]
    CorpusOverBound,
    #[error("checkpoint product instance is unavailable")]
    ProductUnavailable,
    #[error("checkpoint acceptance action requires an earlier capture")]
    MissingCheckpoint,
    #[error("checkpoint acceptance action requires an earlier plan")]
    MissingPlan,
    #[error("checkpoint acceptance action observed the wrong outcome")]
    UnexpectedOutcome,
    #[error("checkpoint acceptance step {seq} observed the wrong outcome")]
    StepOutcome { seq: u64 },
    #[error("the fixture Git repository could not be changed")]
    GitUnavailable,
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
    #[error(transparent)]
    Product(#[from] CheckpointProductError),
}

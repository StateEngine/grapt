//! Deterministic storage and lifecycle fault hooks.
//!
//! Models crash-before-commit, crash-after-commit-before-ack, `ENOSPC`/disk-full,
//! and SQLite busy/stalled responses while enforcing that no duplicate durable
//! terminal record occurs across restart or reconnect.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::schedule::FaultSchedule;
use crate::seed::FaultSeed;

/// Lifecycle operation point where a storage fault can be injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StorageOperation {
    /// Attempting to durably commit a terminal record.
    CommitTerminal,
    /// Reading committed durable state after restart/reconnect.
    ReadState,
}

/// Fault injected by [`StorageFaultScript`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageFault {
    /// Process crashes before the durable commit lands.
    CrashBeforeCommit,
    /// Durable commit succeeds, then process crashes before acknowledging the caller.
    CrashAfterCommitBeforeAck,
    /// Underlying filesystem returns `ENOSPC` / disk full.
    Enospc,
    /// SQLite returns `SQLITE_BUSY` (retryable lock contention).
    SqliteBusy,
    /// Storage backend stalls and refuses the write honestly without partial state.
    Stalled,
}

/// Outcome of a single [`TerminalLedger::commit_terminal`] attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageOutcome {
    /// Newly committed as a durable terminal record.
    Committed(TerminalRecord),
    /// Already committed durably under this idempotency key; deduplicated on reconnect.
    AlreadyCommitted(TerminalRecord),
}

/// One durable terminal record in [`TerminalLedger`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRecord {
    pub sequence: u64,
    pub idempotency_key: String,
    pub payload: String,
}

/// Errors returned by [`TerminalLedger`] when a storage fault fires.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TerminalCommitError {
    #[error("process crashed before durable commit for key '{key}'")]
    CrashBeforeCommit { key: String },
    #[error("process crashed after durable commit before ack for key '{key}'")]
    CrashAfterCommitBeforeAck { key: String },
    #[error("storage full (ENOSPC) while committing key '{key}'")]
    Enospc { key: String },
    #[error("sqlite busy while committing key '{key}'")]
    SqliteBusy { key: String },
    #[error("storage stalled while committing key '{key}'")]
    Stalled { key: String },
}

/// Invariant violation reported by [`TerminalLedger::assert_no_duplicate_terminals`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StorageError {
    #[error("duplicate durable terminal detected for idempotency key '{key}'")]
    DuplicateTerminal { key: String },
}

/// Deterministic script mapping 1-based operation indices to [`StorageFault`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageFaultScript {
    seed: FaultSeed,
    faults_by_attempt: BTreeMap<(StorageOperation, u64), StorageFault>,
}

impl StorageFaultScript {
    /// Construct an empty storage fault script tagged with `seed`.
    #[must_use]
    pub fn new(seed: FaultSeed) -> Self {
        Self {
            seed,
            faults_by_attempt: BTreeMap::new(),
        }
    }

    /// Inject `fault` on the 1-based `attempt` of `operation`.
    #[must_use]
    pub fn inject(
        mut self,
        operation: StorageOperation,
        attempt: u64,
        fault: StorageFault,
    ) -> Self {
        self.faults_by_attempt.insert((operation, attempt), fault);
        self
    }

    /// Generate a deterministic seeded script covering commit faults.
    #[must_use]
    pub fn seeded(seed: FaultSeed, fault_attempts: u64) -> Self {
        let mut rng = seed.derive("storage-faults").rng();
        let catalog = [
            StorageFault::CrashBeforeCommit,
            StorageFault::CrashAfterCommitBeforeAck,
            StorageFault::Enospc,
            StorageFault::SqliteBusy,
            StorageFault::Stalled,
        ];
        let mut script = Self::new(seed);
        for attempt in 1..=fault_attempts {
            if let Some(fault) = rng.choose(&catalog) {
                script = script.inject(StorageOperation::CommitTerminal, attempt, *fault);
            }
        }
        script
    }

    /// Return the seed associated with this script.
    #[must_use]
    pub const fn seed(&self) -> FaultSeed {
        self.seed
    }

    /// Look up any fault scheduled for `(operation, attempt)`.
    #[must_use]
    pub fn fault_for(&self, operation: StorageOperation, attempt: u64) -> Option<StorageFault> {
        self.faults_by_attempt.get(&(operation, attempt)).copied()
    }
}

/// Deterministic durable terminal ledger with restart/reconnect simulation.
#[derive(Debug, Clone)]
pub struct TerminalLedger {
    script: StorageFaultScript,
    schedule: Option<FaultSchedule>,
    commit_attempts: u64,
    restarts: u64,
    durable_log: Vec<TerminalRecord>,
    acked_keys: BTreeSet<String>,
}

impl TerminalLedger {
    /// Create a new ledger governed by `script`.
    #[must_use]
    pub fn new(script: StorageFaultScript) -> Self {
        Self {
            script,
            schedule: None,
            commit_attempts: 0,
            restarts: 0,
            durable_log: Vec::new(),
            acked_keys: BTreeSet::new(),
        }
    }

    /// Attach a [`FaultSchedule`] so storage fault events appear in the shared transcript.
    #[must_use]
    pub fn with_schedule(mut self, schedule: FaultSchedule) -> Self {
        self.schedule = Some(schedule);
        self
    }

    /// Simulate a process restart: volatile ack state is cleared while durable log survives.
    pub fn simulate_restart(&mut self) {
        self.restarts = self.restarts.saturating_add(1);
        self.acked_keys.clear();
        self.log_event(
            format!("restart#{}", self.restarts),
            format!("durable_records={}", self.durable_log.len()),
        );
    }

    /// Attempt to durably commit a terminal record under `idempotency_key`.
    ///
    /// Deduplicates already-committed keys across restart/reconnect so no duplicate
    /// durable terminal can be appended even after `CrashAfterCommitBeforeAck`.
    ///
    /// # Errors
    /// Returns [`TerminalCommitError`] when the active [`StorageFaultScript`] injects a fault.
    pub fn commit_terminal(
        &mut self,
        idempotency_key: &str,
        payload: &str,
    ) -> Result<StorageOutcome, TerminalCommitError> {
        self.commit_attempts = self.commit_attempts.saturating_add(1);
        let attempt = self.commit_attempts;
        let fault = self
            .script
            .fault_for(StorageOperation::CommitTerminal, attempt);

        if let Some(fault) = fault {
            self.log_event(
                format!("commit#{attempt} key={idempotency_key}"),
                format!("{fault:?}"),
            );
            return self.apply_commit_fault(fault, idempotency_key, payload);
        }

        if let Some(existing) = self.find_durable(idempotency_key) {
            self.acked_keys.insert(idempotency_key.to_owned());
            self.log_event(
                format!("commit#{attempt} key={idempotency_key}"),
                "AlreadyCommitted".to_owned(),
            );
            return Ok(StorageOutcome::AlreadyCommitted(existing));
        }

        let record = self.append_durable(idempotency_key, payload);
        self.acked_keys.insert(idempotency_key.to_owned());
        self.log_event(
            format!("commit#{attempt} key={idempotency_key}"),
            format!("Committed(seq={})", record.sequence),
        );
        Ok(StorageOutcome::Committed(record))
    }

    /// Verify that every durable terminal record has a unique idempotency key.
    ///
    /// # Errors
    /// Returns [`StorageError::DuplicateTerminal`] if any key appears more than once.
    pub fn assert_no_duplicate_terminals(&self) -> Result<(), StorageError> {
        let mut seen = BTreeSet::new();
        for record in &self.durable_log {
            if !seen.insert(record.idempotency_key.as_str()) {
                return Err(StorageError::DuplicateTerminal {
                    key: record.idempotency_key.clone(),
                });
            }
        }
        Ok(())
    }

    /// Return all durably committed terminal records in commit order.
    #[must_use]
    pub fn durable_records(&self) -> &[TerminalRecord] {
        &self.durable_log
    }

    /// Return the number of simulated restarts performed on this ledger.
    #[must_use]
    pub const fn restart_count(&self) -> u64 {
        self.restarts
    }

    fn apply_commit_fault(
        &mut self,
        fault: StorageFault,
        idempotency_key: &str,
        payload: &str,
    ) -> Result<StorageOutcome, TerminalCommitError> {
        match fault {
            StorageFault::CrashBeforeCommit => Err(TerminalCommitError::CrashBeforeCommit {
                key: idempotency_key.to_owned(),
            }),
            StorageFault::CrashAfterCommitBeforeAck => {
                if self.find_durable(idempotency_key).is_none() {
                    let _ = self.append_durable(idempotency_key, payload);
                }
                Err(TerminalCommitError::CrashAfterCommitBeforeAck {
                    key: idempotency_key.to_owned(),
                })
            }
            StorageFault::Enospc => Err(TerminalCommitError::Enospc {
                key: idempotency_key.to_owned(),
            }),
            StorageFault::SqliteBusy => Err(TerminalCommitError::SqliteBusy {
                key: idempotency_key.to_owned(),
            }),
            StorageFault::Stalled => Err(TerminalCommitError::Stalled {
                key: idempotency_key.to_owned(),
            }),
        }
    }

    fn find_durable(&self, idempotency_key: &str) -> Option<TerminalRecord> {
        self.durable_log
            .iter()
            .find(|r| r.idempotency_key == idempotency_key)
            .cloned()
    }

    fn append_durable(&mut self, idempotency_key: &str, payload: &str) -> TerminalRecord {
        let sequence = u64::try_from(self.durable_log.len()).unwrap_or(0) + 1;
        let record = TerminalRecord {
            sequence,
            idempotency_key: idempotency_key.to_owned(),
            payload: payload.to_owned(),
        };
        self.durable_log.push(record.clone());
        record
    }

    fn log_event(&self, subject: String, action: String) {
        if let Some(schedule) = &self.schedule {
            schedule.record_event("storage", subject, action);
        }
    }
}

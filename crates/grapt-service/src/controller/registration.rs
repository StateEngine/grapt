use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    EnvironmentReceipt, OwnerReceipt, ReplacementOutcome, ServiceGeneration,
    ServiceRegistrationVersion, ServiceSlotId, UpgradeDisposition,
};

#[expect(
    unused_imports,
    reason = "SV25's compile-valid reclaim counterexample names this decoder refusal"
)]
use crate::registration::{RegistrationDecodeError, RegistrationRecord};
use crate::store::{CasOutcome, ElectedControllerAuthority, PostCommitUnlockWarning};

use super::discovery::{Discovered, Observation, SlotState};
use super::refusal::{ControlRefusal, UpgradeTransitionRefusal};
use super::transition::{Provenance, Retirement, StopOutcome};
/// A candidate proven ready, and not yet committed.
///
/// The only value that can be rolled back, and the value
/// [`ServiceController::commit`] consumes. See the module note for why that
/// consumption is the enforcement mechanism rather than a convention.
#[derive(Debug)]
pub struct PreparedCandidate {
    slot: ServiceSlotId,
    expected: Option<Vec<u8>>,
    provenance: Provenance,
    minted: OwnerReceipt,
    next_bytes: Vec<u8>,
}

/// A candidate admitted specifically as an upgrade.
///
/// This wrapper is the type-level boundary between ordinary replacement and
/// upgrade. It can be constructed only after both recorded versions validate
/// as SemVer and the candidate has strictly greater precedence. Consequently,
/// an ordinary [`PreparedCandidate`] cannot be passed to
/// [`ServiceController::commit_upgrade`], and equal/lower/unorderable versions
/// never reach a registration CAS.
#[derive(Debug)]
pub struct PreparedUpgrade {
    prepared: PreparedCandidate,
    incumbent_version: ServiceRegistrationVersion,
    candidate_version: ServiceRegistrationVersion,
}

impl PreparedUpgrade {
    pub fn minted_owner(&self) -> &OwnerReceipt {
        self.prepared.minted_owner()
    }

    pub fn incumbent_version(&self) -> &ServiceRegistrationVersion {
        &self.incumbent_version
    }

    pub fn candidate_version(&self) -> &ServiceRegistrationVersion {
        &self.candidate_version
    }

    /// Abandon a not-yet-committed upgrade through the ordinary total rollback
    /// transition. No registration write occurs.
    pub fn rollback(self) -> RollbackReceipt {
        self.prepared.rollback()
    }
}

impl PreparedCandidate {
    /// The owner receipt the controller minted for this candidate.
    pub fn minted_owner(&self) -> &OwnerReceipt {
        &self.minted
    }

    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// The exact incumbent bytes this candidate's commit will compare against.
    pub fn expected_bytes(&self) -> Option<&[u8]> {
        self.expected.as_deref()
    }

    /// Abandon the candidate. Total, and writes nothing.
    ///
    /// No `Result`, and no store access: nothing was committed, so the
    /// incumbent's ownership is untouched by construction rather than by a
    /// compensating write that could itself fail. A rollback that could fail
    /// would leave ownership in doubt at exactly the moment master law 3 says
    /// it is certain.
    pub fn rollback(self) -> RollbackReceipt {
        RollbackReceipt {
            slot: self.slot,
            provenance: self.provenance,
            abandoned: self.minted,
        }
    }
}

/// The receipt for an abandoned candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackReceipt {
    slot: ServiceSlotId,
    provenance: Provenance,
    abandoned: OwnerReceipt,
}

impl RollbackReceipt {
    /// Always [`ReplacementOutcome::RolledBack`]. Distinct from
    /// `FailedAfterCommit`, which is the outcome for a candidate that *did*
    /// become current — the two are never interchangeable.
    pub fn outcome(&self) -> ReplacementOutcome {
        ReplacementOutcome::RolledBack
    }

    pub fn slot(&self) -> &ServiceSlotId {
        &self.slot
    }

    pub fn abandoned(&self) -> &OwnerReceipt {
        &self.abandoned
    }

    /// The incumbent that remains the owner, when there was one.
    pub fn incumbent(&self) -> Option<&OwnerReceipt> {
        match &self.provenance {
            Provenance::Succeeding { incumbent } => Some(incumbent),
            Provenance::EmptySlot | Provenance::Reclaimed { .. } => None,
        }
    }
}

/// The receipt for an attempted switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchReceipt {
    outcome: ReplacementOutcome,
    committed: Option<OwnerReceipt>,
    provenance: Provenance,
    retirement: Retirement,
    observed: Option<Vec<u8>>,
    switched_at: Timestamp,
    unlock_warning: Option<PostCommitUnlockWarning>,
}

/// The distinct typed result of the upgrade transition.
///
/// It preserves the exact stored registration strings for audit/replay while
/// semantic projection was used only to admit the transition. Build metadata
/// is therefore neither discarded from the durable fact nor allowed to affect
/// precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeReceipt {
    switch: SwitchReceipt,
    incumbent_version: ServiceRegistrationVersion,
    candidate_version: ServiceRegistrationVersion,
}

impl UpgradeReceipt {
    pub fn outcome(&self) -> ReplacementOutcome {
        self.switch.outcome()
    }

    pub fn committed_owner(&self) -> Option<&OwnerReceipt> {
        self.switch.committed_owner()
    }

    pub fn incumbent_version(&self) -> &ServiceRegistrationVersion {
        &self.incumbent_version
    }

    pub fn candidate_version(&self) -> &ServiceRegistrationVersion {
        &self.candidate_version
    }

    pub fn switch_receipt(&self) -> &SwitchReceipt {
        &self.switch
    }

    pub fn candidate_failed_after_commit(self) -> Result<Self, ControlRefusal> {
        Ok(Self {
            switch: self.switch.candidate_failed_after_commit()?,
            incumbent_version: self.incumbent_version,
            candidate_version: self.candidate_version,
        })
    }
}

impl SwitchReceipt {
    /// The landed classification. This crate declares no outcome vocabulary of
    /// its own; [`ReplacementOutcome`] is the sole authority.
    pub fn outcome(&self) -> ReplacementOutcome {
        self.outcome.clone()
    }

    /// The owner now registered, when the switch committed.
    pub fn committed_owner(&self) -> Option<&OwnerReceipt> {
        self.committed.as_ref()
    }

    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    pub fn retirement(&self) -> &Retirement {
        &self.retirement
    }

    /// What was actually in the slot when the compare failed.
    pub fn observed(&self) -> Option<&[u8]> {
        self.observed.as_deref()
    }

    pub fn switched_at(&self) -> Timestamp {
        self.switched_at
    }

    /// Evidence that the registration changed even though explicit unlock
    /// then failed. This is a warning on a committed external effect.
    pub fn unlock_warning(&self) -> Option<&PostCommitUnlockWarning> {
        self.unlock_warning.as_ref()
    }

    /// Whether the candidate became the current registration.
    pub fn committed(&self) -> bool {
        matches!(
            self.outcome,
            ReplacementOutcome::Replaced
                | ReplacementOutcome::ReplacedRetirePending
                | ReplacementOutcome::FailedAfterCommit
        )
    }

    /// Record that the candidate failed **after** it became current.
    ///
    /// The registration is deliberately left alone: master's diagram says
    /// `FailedAfterCommit { current registration identifies candidate }`, so
    /// the truthful record is that the candidate *is* the owner and has failed
    /// — not that the incumbent was restored. Restoring the incumbent here
    /// would be a second, unwitnessed ownership transfer performed on the
    /// strength of a failure report.
    ///
    /// Refused on a receipt that never committed, because a switch that lost
    /// its race has no current registration to have failed.
    pub fn candidate_failed_after_commit(self) -> Result<SwitchReceipt, ControlRefusal> {
        if !self.committed() {
            return Err(ControlRefusal::NotCommitted);
        }
        Ok(SwitchReceipt {
            outcome: ReplacementOutcome::FailedAfterCommit,
            ..self
        })
    }
}

/// The elected controller for one registration slot in one environment.
#[derive(Debug)]
pub struct ServiceController<'a> {
    pub(super) slot: ServiceSlotId,
    pub(super) environment: EnvironmentReceipt,
    pub(super) authority: &'a ElectedControllerAuthority,
    clock: &'a dyn ClockAuthority,
}

impl<'a> ServiceController<'a> {
    /// There is no zero-argument constructor and no `Default`: a controller
    /// that forgot its injected clock does not compile.
    pub fn new(
        slot: ServiceSlotId,
        environment: EnvironmentReceipt,
        authority: &'a ElectedControllerAuthority,
        clock: &'a dyn ClockAuthority,
    ) -> Self {
        Self {
            slot,
            environment,
            authority,
            clock,
        }
    }

    pub fn slot(&self) -> &ServiceSlotId {
        &self.slot
    }

    pub fn environment(&self) -> &EnvironmentReceipt {
        &self.environment
    }

    /// Prove a candidate ready to take the slot, without taking it.
    ///
    /// Every one of master law 1's five pre-switch conditions is checked here,
    /// each with its own refusal, and none of them writes anything. A candidate
    /// that fails any of them never reaches a CAS, which is what makes
    /// "incumbent remains owner" true by construction rather than by cleanup.
    pub fn prepare(
        &self,
        discovered: &Discovered,
        candidate: &Observation,
    ) -> Result<PreparedCandidate, ControlRefusal> {
        let (provenance, required_generation) = match discovered.state() {
            SlotState::Empty => (Provenance::EmptySlot, ServiceGeneration::FIRST),
            SlotState::Unreadable(error) => {
                return Err(ControlRefusal::UnreadableRegistration {
                    source: error.clone(),
                })
            }
            SlotState::Registered(record) => {
                let incumbent = record.owner();
                if incumbent.is_same_owner(candidate.declared()) {
                    return Err(ControlRefusal::CandidateIsTheIncumbent);
                }
                if incumbent.endpoint() == candidate.declared().endpoint() {
                    return Err(ControlRefusal::EndpointNotIndependent {
                        endpoint: candidate.declared().endpoint().to_string(),
                    });
                }
                let next = incumbent
                    .generation()
                    .next()
                    .ok_or(ControlRefusal::GenerationExhausted)?;
                (
                    Provenance::Succeeding {
                        incumbent: incumbent.clone(),
                    },
                    next,
                )
            }
        };
        self.admit(candidate, discovered, provenance, required_generation)
    }

    /// Admit a candidate through the distinct upgrade transition.
    ///
    /// The comparison is against the exact incumbent bytes retained by
    /// `discover`; the later CAS closes the race. Only strictly greater SemVer
    /// precedence reaches ordinary candidate preparation. Equal precedence
    /// (including build-only changes), downgrade, and either side's invalid
    /// semantic projection are typed refusals and perform no write.
    pub fn prepare_upgrade(
        &self,
        discovered: &Discovered,
        candidate: &Observation,
    ) -> Result<PreparedUpgrade, UpgradeTransitionRefusal> {
        let incumbent_version = match discovered.state() {
            SlotState::Empty => return Err(UpgradeTransitionRefusal::NoIncumbent),
            SlotState::Unreadable(error) => {
                return Err(ControlRefusal::UnreadableRegistration {
                    source: error.clone(),
                }
                .into())
            }
            SlotState::Registered(record) => record.version().clone(),
        };
        let candidate_version = candidate.version().clone();
        match ServiceRegistrationVersion::upgrade_disposition(
            &incumbent_version,
            &candidate_version,
        )? {
            UpgradeDisposition::Upgrade => {}
            UpgradeDisposition::SamePrecedence => {
                return Err(UpgradeTransitionRefusal::SamePrecedence {
                    incumbent: incumbent_version,
                    candidate: candidate_version,
                })
            }
            UpgradeDisposition::Downgrade => {
                return Err(UpgradeTransitionRefusal::Downgrade {
                    incumbent: incumbent_version,
                    candidate: candidate_version,
                })
            }
        }
        let prepared = self.prepare(discovered, candidate)?;
        Ok(PreparedUpgrade {
            prepared,
            incumbent_version,
            candidate_version,
        })
    }

    /// Take a slot whose bytes decode to no record.
    ///
    /// A separate transition from [`prepare`](Self::prepare) on purpose. The
    /// prior generation is not knowable from bytes that do not decode, so
    /// ownership necessarily restarts at [`ServiceGeneration::FIRST`] — and a
    /// generation that restarts must be an operation somebody named, not a
    /// silent consequence of a corrupt file. The CAS still compares the exact
    /// unreadable bytes, so a slot that has been repaired or retaken in the
    /// meantime loses the race rather than being overwritten.
    pub fn reclaim(
        &self,
        discovered: &Discovered,
        candidate: &Observation,
    ) -> Result<PreparedCandidate, ControlRefusal> {
        let SlotState::Unreadable(error) = discovered.state() else {
            return Err(ControlRefusal::NothingToReclaim);
        };
        let provenance = Provenance::Reclaimed {
            because: error.clone(),
        };
        self.admit(candidate, discovered, provenance, ServiceGeneration::FIRST)
    }

    /// The candidate checks common to `prepare` and `reclaim`, and the mint.
    fn admit(
        &self,
        candidate: &Observation,
        discovered: &Discovered,
        provenance: Provenance,
        required_generation: ServiceGeneration,
    ) -> Result<PreparedCandidate, ControlRefusal> {
        let declared = candidate.declared();
        if declared.slot() != &self.slot {
            return Err(ControlRefusal::NotThisSlot {
                expected: self.slot.to_string(),
                declared: declared.slot().to_string(),
            });
        }
        if declared.environment() != &self.environment {
            return Err(ControlRefusal::NotTheExpectedEnvironment {
                expected: self.environment.to_string(),
                declared: declared.environment().to_string(),
            });
        }
        // The authenticated health check. `?` carries R4D-1's `Unattributed`
        // refusal through unchanged.
        let attributed = candidate.attributed()?;
        if !declared.is_same_owner(attributed) {
            return Err(ControlRefusal::ProbeAttributedAnotherOwner {
                declared: declared.instance().to_string(),
                attributed: attributed.instance().to_string(),
            });
        }
        // Identity matching does not imply the process matches: `is_same_owner`
        // excludes the PID by design, so this is a separate law.
        if declared.pid() != attributed.pid() {
            return Err(ControlRefusal::PidDoesNotMatchTheProbe {
                declared: declared.pid().value(),
                attributed: attributed.pid().value(),
            });
        }
        if declared.endpoint() != attributed.endpoint() {
            return Err(ControlRefusal::EndpointDoesNotMatchTheProbe {
                declared: declared.endpoint().to_string(),
                attributed: attributed.endpoint().to_string(),
            });
        }
        if declared.generation() != required_generation {
            return Err(ControlRefusal::GenerationNotSuccessor {
                declared: declared.generation().value(),
                required: required_generation.value(),
            });
        }
        let minted = OwnerReceipt::new(
            self.slot.clone(),
            declared.instance().clone(),
            self.environment.clone(),
            required_generation,
            declared.pid(),
            declared.endpoint().clone(),
        );
        let record = RegistrationRecord::new(
            minted.clone(),
            candidate.version().clone(),
            self.clock.now(),
        )?;
        Ok(PreparedCandidate {
            slot: self.slot.clone(),
            expected: discovered.bytes.clone(),
            provenance,
            minted,
            next_bytes: record.encode(),
        })
    }

    /// Commit the switch, consuming the candidate.
    ///
    /// The candidate is consumed rather than borrowed so that the pre-commit
    /// rollback becomes unnameable the instant a commit is attempted. See the
    /// module note.
    pub fn commit(
        &self,
        prepared: PreparedCandidate,
        retirement: Retirement,
    ) -> Result<SwitchReceipt, ControlRefusal> {
        if prepared.slot != self.slot {
            return Err(ControlRefusal::PreparedForAnotherSlot {
                prepared: prepared.slot.to_string(),
                controller: self.slot.to_string(),
            });
        }
        let cas = self.authority.compare_and_swap(
            self.authority.mutation_permit(),
            prepared.expected.as_deref(),
            Some(&prepared.next_bytes),
        )?;
        let switched_at = self.clock.now();
        match cas {
            CasOutcome::Lost { observed } => Ok(SwitchReceipt {
                outcome: ReplacementOutcome::LostRace,
                committed: None,
                provenance: prepared.provenance,
                retirement,
                observed,
                switched_at,
                unlock_warning: None,
            }),
            CasOutcome::Committed { unlock_warning } => {
                // Master law 4: after the commit, a retirement that did not
                // happen is a warning on a success. It is never `RolledBack`,
                // because the candidate is already the current registration.
                let outcome = match &retirement {
                    Retirement::NoIncumbent | Retirement::Retired => ReplacementOutcome::Replaced,
                    Retirement::TimedOut | Retirement::Failed { .. } => {
                        ReplacementOutcome::ReplacedRetirePending
                    }
                };
                Ok(SwitchReceipt {
                    outcome,
                    committed: Some(prepared.minted),
                    provenance: prepared.provenance,
                    retirement,
                    observed: None,
                    switched_at,
                    unlock_warning,
                })
            }
        }
    }

    /// Commit a previously admitted upgrade and return its distinct receipt.
    pub fn commit_upgrade(
        &self,
        prepared: PreparedUpgrade,
        retirement: Retirement,
    ) -> Result<UpgradeReceipt, ControlRefusal> {
        let PreparedUpgrade {
            prepared,
            incumbent_version,
            candidate_version,
        } = prepared;
        let switch = self.commit(prepared, retirement)?;
        Ok(UpgradeReceipt {
            switch,
            incumbent_version,
            candidate_version,
        })
    }

    /// Release the slot.
    ///
    /// Only the registered owner may release it. The CAS compares the exact
    /// discovered bytes, so a slot that changed hands between the read and the
    /// release loses the race instead of evicting the new owner.
    pub fn stop(
        &self,
        discovered: &Discovered,
        owner: &OwnerReceipt,
    ) -> Result<StopOutcome, ControlRefusal> {
        let record = match discovered.state() {
            SlotState::Empty => return Ok(StopOutcome::SlotAlreadyEmpty),
            SlotState::Unreadable(error) => {
                return Err(ControlRefusal::UnreadableRegistration {
                    source: error.clone(),
                })
            }
            SlotState::Registered(record) => record,
        };
        if record.owner() != owner {
            return Ok(StopOutcome::NotTheOwner {
                registered: record.owner().clone(),
            });
        }
        match self.authority.compare_and_swap(
            self.authority.mutation_permit(),
            discovered.bytes.as_deref(),
            None,
        )? {
            CasOutcome::Committed { unlock_warning } => Ok(StopOutcome::Released {
                owner: record.owner().clone(),
                unlock_warning,
            }),
            CasOutcome::Lost { observed } => Ok(StopOutcome::LostRace { observed }),
        }
    }
}

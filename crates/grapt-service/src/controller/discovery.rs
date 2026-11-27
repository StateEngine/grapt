use grapt_types::service::{OwnerReceipt, ServiceRegistrationVersion};

use crate::receipt::{AttachmentRefusal, ProbeOutcome};
use crate::registration::{RegistrationDecodeError, RegistrationRecord};

use super::refusal::ControlRefusal;
use super::registration::ServiceController;
use super::transition::{EnsureOutcome, ReplacementReason};
/// What the caller observed about one service instance.
///
/// `declared` is what the instance says it is; `probe` is what a health check
/// actually returned. Keeping them apart is the whole point — R4D-1 established
/// that the frozen desktop's `checkHealth` accepts any 2xx, so a claim and an
/// attribution are different facts and a controller that conflated them could
/// switch to an engine it never identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    declared: OwnerReceipt,
    probe: ProbeOutcome,
    version: ServiceRegistrationVersion,
}

impl Observation {
    pub fn new(
        declared: OwnerReceipt,
        probe: ProbeOutcome,
        version: ServiceRegistrationVersion,
    ) -> Self {
        Self {
            declared,
            probe,
            version,
        }
    }

    pub fn declared(&self) -> &OwnerReceipt {
        &self.declared
    }

    pub fn probe(&self) -> &ProbeOutcome {
        &self.probe
    }

    pub fn version(&self) -> &ServiceRegistrationVersion {
        &self.version
    }

    /// The owner a probe actually attributed, or the R4D-1 refusal for a probe
    /// that proved only liveness.
    pub(super) fn attributed(&self) -> Result<&OwnerReceipt, AttachmentRefusal> {
        match &self.probe {
            ProbeOutcome::HealthyUnattributed => Err(AttachmentRefusal::Unattributed),
            ProbeOutcome::Attributed(owner) => Ok(owner),
        }
    }
}

/// What the discovery slot holds.
///
/// Three states, not two. `Unreadable` is the one that carries the crash: a
/// registration that cannot be decoded is neither an owner nor an empty slot,
/// and collapsing it into either would be a real defect. Read as `Empty` it
/// would let a candidate take a slot a live incumbent still serves; read as an
/// owner it would be an owner nobody wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    Empty,
    Registered(RegistrationRecord),
    Unreadable(RegistrationDecodeError),
}

/// One discovery read: the exact bytes observed, and what they meant.
///
/// The bytes are retained because the CAS compares *these*, not a re-read. A
/// `prepare` that re-read the slot would open a window between the decision and
/// the compare, which is precisely the check-then-act race the CAS exists to
/// close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub(super) bytes: Option<Vec<u8>>,
    state: SlotState,
}

impl Discovered {
    pub fn state(&self) -> &SlotState {
        &self.state
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }

    /// The registered owner, when the slot holds a readable one.
    pub fn registered_owner(&self) -> Option<&OwnerReceipt> {
        match &self.state {
            SlotState::Registered(record) => Some(record.owner()),
            SlotState::Empty | SlotState::Unreadable(_) => None,
        }
    }
}

impl<'a> ServiceController<'a> {
    /// Read the slot once, keeping the exact bytes for a later compare.
    pub fn discover(&self) -> Result<Discovered, ControlRefusal> {
        let bytes = self.authority.read_registration()?;
        let state = match &bytes {
            None => SlotState::Empty,
            Some(bytes) => match RegistrationRecord::decode(bytes) {
                Ok(record) => {
                    if record.owner().slot() != &self.slot {
                        return Err(ControlRefusal::DurableSlotMismatch {
                            controller: self.slot.to_string(),
                            registered: record.owner().slot().to_string(),
                        });
                    }
                    if record.owner().environment() != &self.environment {
                        return Err(ControlRefusal::DurableEnvironmentMismatch {
                            controller: self.environment.to_string(),
                            registered: record.owner().environment().to_string(),
                        });
                    }
                    SlotState::Registered(record)
                }
                Err(error) => SlotState::Unreadable(error),
            },
        };
        Ok(Discovered { bytes, state })
    }

    /// May the incumbent stand?
    ///
    /// `incumbent` is what the caller probed at the registered endpoint, and is
    /// `None` when the caller did not probe. A registration whose owner no
    /// probe attributed is **stale**, not healthy: master requires an
    /// authenticated health check, and R4D-1 established that a 2xx alone
    /// attributes nothing.
    pub fn ensure(
        &self,
        discovered: &Discovered,
        incumbent: Option<&Observation>,
        required_version: &ServiceRegistrationVersion,
    ) -> EnsureOutcome {
        let record = match discovered.state() {
            SlotState::Empty => {
                return EnsureOutcome::ReplacementRequired {
                    reason: ReplacementReason::SlotEmpty,
                }
            }
            SlotState::Unreadable(error) => {
                return EnsureOutcome::ReplacementRequired {
                    reason: ReplacementReason::UnreadableRegistration(error.clone()),
                }
            }
            SlotState::Registered(record) => record,
        };
        let stale = EnsureOutcome::ReplacementRequired {
            reason: ReplacementReason::StaleRegistration {
                registered: record.owner().clone(),
            },
        };
        // An unattributed probe, a probe that named somebody else, or no probe
        // at all all mean the same thing: nothing proved this registration
        // still has a live writer.
        let Some(observation) = incumbent else {
            return stale;
        };
        let Ok(attributed) = observation.attributed() else {
            return stale;
        };
        if !record.owner().is_same_owner(attributed) {
            return stale;
        }
        if observation.version() != record.version() {
            return EnsureOutcome::ReplacementRequired {
                reason: ReplacementReason::ObservedVersionMismatch {
                    registered: record.version().clone(),
                    observed: observation.version().clone(),
                    required: required_version.clone(),
                },
            };
        }
        if record.version() != required_version {
            return EnsureOutcome::ReplacementRequired {
                reason: ReplacementReason::IncompatibleVersion {
                    registered: record.version().clone(),
                    required: required_version.clone(),
                },
            };
        }
        EnsureOutcome::ReusedIncumbent {
            owner: record.owner().clone(),
        }
    }
}

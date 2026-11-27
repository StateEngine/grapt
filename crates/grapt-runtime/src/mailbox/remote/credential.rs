//! Who owns the credential retry budget: the provision generation, never the
//! binding. Only an authenticated provision selects a generation; every I/O
//! outcome is stamped with the generation its attempt read, and an outcome
//! from any other generation cannot count, reset, park or clear anything.
//!
//! The generation and the budget are process memory and restart from nothing
//! together with the service, which needs a fresh provision anyway. Neither
//! may be persisted without the other.
use std::num::NonZeroU64;

/// Floor-changed refusals reopened before the capability must be rebound.
pub const FLOOR_RETRIES: u8 = 3;

/// One authenticated provision of a session's capability. Unprovisioned is the
/// absence of a generation, never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProvisionGeneration(NonZeroU64);

impl ProvisionGeneration {
    pub const FIRST: Self = Self(NonZeroU64::MIN);

    /// The generation after this one; `None` when the counter is exhausted.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Why the current generation must be rebound. The first cause is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCause {
    /// Floor-changed refusals outlasted [`FLOOR_RETRIES`].
    Floor,
    /// The source refused the session's project binding.
    Principal,
    /// Authentication refused, or no capability is held at all.
    Capability,
}

/// An observed I/O outcome, classified by the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialEvent {
    FloorChanged,
    ProjectMismatch,
    Unauthenticated,
    /// Unavailable, deadline or any other transport refusal: no credential fact.
    Transport,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The current generation may try again.
    Retry,
    /// The current generation is refused until a newer provision.
    Park(RefusalCause),
    /// Stale, future or unprovisioned: the outcome changes nothing.
    Ignore,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialBudget {
    generation: Option<ProvisionGeneration>,
    floor: u8,
    refusal: Option<RefusalCause>,
}

impl CredentialBudget {
    /// Adopt a provision newer than the one held, however many were skipped:
    /// a fresh allowance and no refusal. An older or equal one changes nothing.
    pub fn provisioned(&mut self, generation: ProvisionGeneration) {
        if self.generation.is_none_or(|held| generation > held) {
            *self = Self {
                generation: Some(generation),
                floor: 0,
                refusal: None,
            };
        }
    }

    /// No capability is held at all, which only a provision can repair.
    pub fn absent(&mut self) {
        self.refusal.get_or_insert(RefusalCause::Capability);
    }

    pub fn refusal(&self) -> Option<RefusalCause> {
        self.refusal
    }

    /// Apply one outcome stamped with the generation its attempt read.
    pub fn observe(
        &mut self,
        stamp: Option<ProvisionGeneration>,
        event: CredentialEvent,
    ) -> Disposition {
        if stamp.is_none() || stamp != self.generation {
            return Disposition::Ignore;
        }
        let refused = match event {
            CredentialEvent::Live => {
                self.floor = 0;
                return self.refusal.map_or(Disposition::Retry, Disposition::Park);
            }
            CredentialEvent::Transport => {
                return self.refusal.map_or(Disposition::Retry, Disposition::Park)
            }
            CredentialEvent::FloorChanged if self.refusal.is_none() => {
                self.floor = self.floor.saturating_add(1);
                if self.floor <= FLOOR_RETRIES {
                    return Disposition::Retry;
                }
                RefusalCause::Floor
            }
            CredentialEvent::FloorChanged => RefusalCause::Floor,
            CredentialEvent::ProjectMismatch => RefusalCause::Principal,
            CredentialEvent::Unauthenticated => RefusalCause::Capability,
        };
        Disposition::Park(*self.refusal.get_or_insert(refused))
    }
}

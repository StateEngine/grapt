//! The `service ensure|stop|status` verbs, rendered.
//!
//! Every transition decision here belongs to `grapt-service`. This module
//! decides nothing about ownership; it turns the landed outcomes into the one
//! JSON object the binary prints, and it is the place two laws are carried
//! that only a composition root can get wrong:
//!
//! - a lost registration CAS names the winner and is never rendered as a
//!   success;
//! - `status` observes and never mutates.
//!
//! Both are rendering and call-shape laws rather than domain laws, which is
//! why they live in the binary rather than being re-witnessed against the
//! library that already owns the transitions themselves.

use grapt_service::{
    ControlRefusal, FileRegistrationStore, LockAttemptPolicy, RegistrationRecord,
    RegistrationStore, StopOutcome, StoreError, SwitchReceipt,
};
use grapt_types::ReplacementOutcome;
use std::path::Path;

/// What a service verb achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceOutcome {
    /// The candidate took the slot.
    Switched { owner: String },
    /// The slot held this owner and now holds nothing.
    Released { owner: String },
    /// There was nothing to release.
    SlotAlreadyEmpty,
    /// Somebody else holds the slot; releasing it is not ours to do.
    NotTheOwner { registered: String },
    /// A read-only observation of the slot.
    Observed {
        state: String,
        owner: Option<String>,
    },
}

/// Why a service verb refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRefusal {
    /// The compare-and-swap found different bytes than it expected. Somebody
    /// else committed first, and this run changed nothing.
    LostRace { winner: String },
    /// The controller refused before any effect.
    Control { reason: String },
    /// The registration slot could not be read or written.
    Store { reason: String },
}

impl ServiceRefusal {
    /// The stable machine label for this refusal.
    pub fn label(&self) -> &'static str {
        match self {
            ServiceRefusal::LostRace { .. } => "lost_registration_race",
            ServiceRefusal::Control { .. } => "control_refusal",
            ServiceRefusal::Store { .. } => "store_error",
        }
    }

    /// The operator-facing detail.
    pub fn detail(&self) -> String {
        match self {
            ServiceRefusal::LostRace { winner } => {
                format!("the registration slot was taken first by {winner}")
            }
            ServiceRefusal::Control { reason } | ServiceRefusal::Store { reason } => reason.clone(),
        }
    }
}

/// Name whoever actually holds the slot, from the bytes the CAS observed.
///
/// The landed CAS reports `observed: Option<Vec<u8>>` rather than a receipt,
/// so naming the winner means decoding those exact bytes. An undecodable or
/// absent observation is reported as such and never as "nobody": the CAS was
/// still lost, and a refusal that cannot name the winner must say that plainly
/// rather than imply the slot was free.
#[allow(
    dead_code,
    reason = "reachable only through a compare-and-swap, which E1a is DEFERRED from \n              reaching rather than blocked from reaching: ServiceController::new \n              requires a production ClockAuthority, the only two production \n              authority adapters live in grapt-store, and the ruled lawful cure is \n              RELOCATION of those adapters, not an edge to that crate. The owner is \n              the controller lane, gated behind the adapter-relocation lane. Retained \n              here because this vocabulary is what that lane inherits."
)]
pub fn winner_from(observed: Option<&[u8]>) -> String {
    match observed {
        None => "an unobserved writer (the slot was empty at the compare)".to_owned(),
        Some(bytes) => match RegistrationRecord::decode(bytes) {
            Ok(record) => record.owner().instance().to_string(),
            Err(_) => "an unreadable registration".to_owned(),
        },
    }
}

/// Render the outcome of an `ensure` that reached a compare-and-swap.
///
/// A lost race is a refusal. It is never `Ok`: the caller did not take the
/// slot, and reporting success here would tell an operator a daemon owns a
/// registration another process holds — the precise failure master law 7
/// reserves the elected controller to prevent.
#[allow(
    dead_code,
    reason = "EB08 is DEFERRED to the controller lane, gated behind the \n              adapter-relocation lane; see winner_from"
)]
pub fn render_ensure(receipt: &SwitchReceipt) -> Result<ServiceOutcome, ServiceRefusal> {
    match receipt.outcome() {
        ReplacementOutcome::LostRace => Err(ServiceRefusal::LostRace {
            winner: winner_from(receipt.observed()),
        }),
        _ => Ok(ServiceOutcome::Switched {
            owner: receipt
                .committed_owner()
                .map(|owner| owner.instance().to_string())
                .unwrap_or_else(|| "no committed owner".to_owned()),
        }),
    }
}

/// Render the outcome of a `stop`.
#[allow(
    dead_code,
    reason = "DEFERRED to the controller lane with render_ensure; see winner_from"
)]
pub fn render_stop(outcome: &StopOutcome) -> Result<ServiceOutcome, ServiceRefusal> {
    match outcome {
        StopOutcome::Released { owner, .. } => Ok(ServiceOutcome::Released {
            owner: owner.instance().to_string(),
        }),
        StopOutcome::SlotAlreadyEmpty => Ok(ServiceOutcome::SlotAlreadyEmpty),
        StopOutcome::NotTheOwner { registered } => Ok(ServiceOutcome::NotTheOwner {
            registered: registered.instance().to_string(),
        }),
        StopOutcome::LostRace { observed } => Err(ServiceRefusal::LostRace {
            winner: winner_from(observed.as_deref()),
        }),
    }
}

/// Observe the registration slot. Reads, and takes no lock.
///
/// This deliberately does **not** build a [`grapt_service::ServiceController`]:
/// a controller requires an `ElectedControllerAuthority`, and acquiring one
/// takes the election lock — a filesystem effect. `status` that locked the slot
/// would make observing it a mutation, so the read goes through the bounded
/// store directly and the authority is never acquired.
pub fn run_status(
    registration: &Path,
    policy: LockAttemptPolicy,
) -> Result<ServiceOutcome, ServiceRefusal> {
    let store = FileRegistrationStore::new(registration, policy);
    let bytes = store.read()?;
    Ok(match bytes {
        None => ServiceOutcome::Observed {
            state: "empty".to_owned(),
            owner: None,
        },
        Some(bytes) => match RegistrationRecord::decode(&bytes) {
            Ok(record) => ServiceOutcome::Observed {
                state: "registered".to_owned(),
                owner: Some(record.owner().instance().to_string()),
            },
            Err(_) => ServiceOutcome::Observed {
                state: "unreadable".to_owned(),
                owner: None,
            },
        },
    })
}

impl From<StoreError> for ServiceRefusal {
    fn from(error: StoreError) -> Self {
        ServiceRefusal::Store {
            reason: error.to_string(),
        }
    }
}

impl From<ControlRefusal> for ServiceRefusal {
    fn from(refusal: ControlRefusal) -> Self {
        ServiceRefusal::Control {
            reason: refusal.to_string(),
        }
    }
}

impl ServiceOutcome {
    /// The stable machine label for this outcome.
    pub fn label(&self) -> &'static str {
        match self {
            ServiceOutcome::Switched { .. } => "switched",
            ServiceOutcome::Released { .. } => "released",
            ServiceOutcome::SlotAlreadyEmpty => "slot_already_empty",
            ServiceOutcome::NotTheOwner { .. } => "not_the_owner",
            ServiceOutcome::Observed { .. } => "observed",
        }
    }

    /// What an observation actually saw: its state, and the owner when the slot
    /// names one.
    ///
    /// [`label`](Self::label) alone cannot carry this. It returns the constant
    /// `"observed"` for all three observations, so a receipt rendered from it
    /// tells an operator that a `status` happened and nothing about what it
    /// found — an absent registration, a slot held by somebody, and undecodable
    /// bytes all render alike. That is the same defect as an exit disposition
    /// chosen by the binary rather than reported by the host, in the other verb.
    ///
    /// `None` is returned for the transition outcomes rather than a wildcard
    /// arm, so a sixth variant must decide for itself whether it is an
    /// observation instead of silently becoming one that saw nothing.
    pub fn observation(&self) -> Option<(&str, Option<&str>)> {
        match self {
            ServiceOutcome::Observed { state, owner } => Some((state.as_str(), owner.as_deref())),
            ServiceOutcome::Switched { .. }
            | ServiceOutcome::Released { .. }
            | ServiceOutcome::SlotAlreadyEmpty
            | ServiceOutcome::NotTheOwner { .. } => None,
        }
    }

    /// The operator-facing prose companion.
    ///
    /// It says exactly what the receipt says. Prose that named a different
    /// state from the JSON object beside it would make the two streams
    /// disagree about one observation, and the reader has no way to know which
    /// to believe.
    pub fn prose(&self) -> String {
        match self.observation() {
            Some((state, Some(owner))) => format!("service observed {state}, owner {owner}"),
            Some((state, None)) => format!("service observed {state}"),
            None => format!("service {}", self.label()),
        }
    }
}

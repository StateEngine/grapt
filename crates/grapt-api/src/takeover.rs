//! The bounded, process-local registry of live recipient connections.
//!
//! Exactly one live lease per recipient. A newer valid acquisition for the same
//! recipient displaces the prior one and the prior holder is told so, with a
//! typed signal naming the generation it lost.
//!
//! ## Ownership, and what this deliberately is not
//!
//! Ruling R4HA-06 places the *generic* mechanism here rather than in a
//! downstream subscription lane, and it is worth recording why the alternative
//! was rejected: `DisplacedByTakeover` originates in two unlanded preflights and
//! R3c2-T explicitly DEFERS the takeover binding rather than ruling it. A
//! downstream lane owning the registry would mean the mechanism is defined by
//! whichever consumer landed first. So this module owns displacement as a
//! typed, transport-neutral fact, and a downstream lane maps that fact onto its
//! own close outcome.
//!
//! **Nothing here is persisted.** No durable row, no event, no store
//! transaction; a lease does not survive the process, which is exactly what
//! makes "process-local" true rather than aspirational.
//!
//! ## Currency is equality, again
//!
//! A lease claims current ownership only when its generation EQUALS the
//! registry's live generation for that recipient — the same reasoning as the
//! directory generation, for the same reason. `>=` would let a displaced lease
//! answer "yes, I am current" forever.
//!
//! ## Release is explicit, and that is a disclosed choice
//!
//! A `Drop`-based release reads better in a design note, but it requires the
//! lease to hold a back-reference to the registry, which means interior
//! mutability and a registry that two owners can observe at once. This crate is
//! synchronous domain law; an explicit [`RecipientConnectionRegistry::release`]
//! that CONSUMES the lease keeps the registry behind a single `&mut` and keeps
//! the bound observable from one place. The lease is still non-clone, so it
//! cannot be released twice.

use std::collections::BTreeMap;

use grapt_types::{AuthenticatedClientId, SeatRecipientId};

use crate::auth::AuthenticatedSeatPrincipal;
use crate::error::ApiRefusal;

/// Which acquisition of one recipient a lease belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseGeneration(u64);

impl LeaseGeneration {
    /// The generation of a recipient's first acquisition.
    pub const FIRST: LeaseGeneration = LeaseGeneration(1);

    pub fn value(self) -> u64 {
        self.0
    }

    /// Saturating for the same reason the directory generation is: a wrap would
    /// make a long-displaced lease current again.
    fn next(self) -> Self {
        LeaseGeneration(self.0.saturating_add(1))
    }
}

/// A checked positive capacity, with no `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxTakeoverSlots(usize);

impl MaxTakeoverSlots {
    pub fn try_new(value: usize) -> Result<Self, ApiRefusal> {
        if value == 0 {
            return Err(ApiRefusal::BoundNotPositive {
                bound: "max_takeover_slots",
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> usize {
        self.0
    }
}

/// Ownership of one recipient's live connection.
///
/// Deliberately **not `Clone`** — a cloned lease would be a second holder of a
/// single-owner claim, and the whole displacement law is that there is one.
/// No serialization route either: this crate takes no serde dependency, so a
/// lease cannot round-trip through a wire format and be replayed as ownership.
#[derive(PartialEq, Eq)]
pub struct RecipientLease {
    recipient: SeatRecipientId,
    generation: LeaseGeneration,
    client: AuthenticatedClientId,
}

impl RecipientLease {
    pub fn recipient(&self) -> &SeatRecipientId {
        &self.recipient
    }

    pub fn generation(&self) -> LeaseGeneration {
        self.generation
    }

    /// The client identity that acquired this lease, as audit truth.
    pub fn client(&self) -> &AuthenticatedClientId {
        &self.client
    }
}

impl core::fmt::Debug for RecipientLease {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("RecipientLease(<live, process-local>)")
    }
}

/// The typed signal handed to a caller whose acquisition displaced another.
///
/// Names the generation that was displaced, not the holder: the prior holder's
/// identity is not the new holder's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Displacement {
    displaced_generation: LeaseGeneration,
}

impl Displacement {
    pub fn displaced_generation(self) -> LeaseGeneration {
        self.displaced_generation
    }
}

/// The result of one acquisition.
#[derive(Debug)]
pub struct Acquisition {
    lease: RecipientLease,
    displaced: Option<Displacement>,
}

impl Acquisition {
    /// The new lease.
    pub fn lease(self) -> RecipientLease {
        self.lease
    }

    /// Borrow the new lease without consuming the acquisition.
    pub fn lease_ref(&self) -> &RecipientLease {
        &self.lease
    }

    /// The displacement signal, if this acquisition took the recipient from a
    /// prior holder.
    pub fn displaced(&self) -> Option<Displacement> {
        self.displaced
    }
}

/// The process-local registry.
#[derive(Debug)]
pub struct RecipientConnectionRegistry {
    live: BTreeMap<SeatRecipientId, LeaseGeneration>,
    max: MaxTakeoverSlots,
}

impl RecipientConnectionRegistry {
    pub fn new(max: MaxTakeoverSlots) -> Self {
        Self {
            live: BTreeMap::new(),
            max,
        }
    }

    /// How many recipients currently hold a live lease.
    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Acquire the live lease for this principal's recipient.
    ///
    /// The recipient comes from the principal — never from a request — so a
    /// caller cannot acquire a seat it did not authenticate as.
    ///
    /// Bound: a **new distinct** recipient is refused when the registry is
    /// full. A same-recipient takeover is not refused at capacity and does not
    /// grow the slot set: the incoming lease replaces the outgoing one under a
    /// key that is already counted, so a takeover storm on one recipient cannot
    /// consume another recipient's slot.
    pub fn acquire(
        &mut self,
        principal: &AuthenticatedSeatPrincipal,
    ) -> Result<Acquisition, ApiRefusal> {
        let recipient = principal.recipient().clone();
        match self.live.get(&recipient).copied() {
            Some(prior) => {
                let generation = prior.next();
                self.live.insert(recipient.clone(), generation);
                Ok(Acquisition {
                    lease: RecipientLease {
                        recipient,
                        generation,
                        client: principal.client().clone(),
                    },
                    displaced: Some(Displacement {
                        displaced_generation: prior,
                    }),
                })
            }
            None => {
                if self.live.len() >= self.max.value() {
                    return Err(ApiRefusal::TakeoverBoundExceeded {
                        max: self.max.value(),
                    });
                }
                let generation = LeaseGeneration::FIRST;
                self.live.insert(recipient.clone(), generation);
                Ok(Acquisition {
                    lease: RecipientLease {
                        recipient,
                        generation,
                        client: principal.client().clone(),
                    },
                    displaced: None,
                })
            }
        }
    }

    /// Whether this lease is the recipient's current owner.
    ///
    /// **Equality.** A displaced lease is not current, and never becomes
    /// current again.
    pub fn is_current(&self, lease: &RecipientLease) -> bool {
        self.live.get(lease.recipient()).copied() == Some(lease.generation())
    }

    /// Release a lease, freeing the recipient's slot.
    ///
    /// Consumes the lease, so it cannot be released twice. A **displaced** lease
    /// releases nothing: its generation is no longer live, and freeing the slot
    /// on its behalf would evict the holder that displaced it.
    pub fn release(&mut self, lease: RecipientLease) -> Result<(), ApiRefusal> {
        if !self.is_current(&lease) {
            return Err(ApiRefusal::LeaseDisplaced);
        }
        self.live.remove(lease.recipient());
        Ok(())
    }
}

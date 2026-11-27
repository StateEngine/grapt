//! The database-gated service transition, and the truthful recovery
//! disposition that follows one.
//!
//! # The hole this closes
//!
//! Electing a service and owning a database were two unrelated acts. The
//! controller proves exactly one owner holds a slot, and it proves it well —
//! identity rather than PID, compare-and-swap rather than last-writer-wins,
//! generations that cannot restart silently. What it never asked is *which
//! database the elected owner is about to write*, so nothing stopped an engine
//! from taking the slot and opening a store no migration had ever been
//! rehearsed against.
//!
//! Ratified requirement 7 ("cutover is one elected service transition with a
//! truthful rollback outcome") is precisely the demand that those two facts
//! become one transition. This module is that composition and nothing more: it
//! adds no second election, no second registration, and no migration engine.
//! Every byte that reaches the registration still goes through
//! [`ServiceController::prepare`] and its compare-and-swap.
//!
//! # What it refuses to be
//!
//! A [`MigrationEligibility`] is evidence, never authority — the migration
//! operator refuses `cutover` by design and this module does not lend it that
//! power. Holding evidence entitles nobody to a slot; it only allows a
//! candidate to be *prepared*, which is still subject to every pre-switch
//! condition the controller already enforces. This crate reads no file, opens
//! no database and starts no process, so the fingerprint of the database about
//! to be served must be supplied by the owner that can read it.

use grapt_types::{DatabaseFingerprint, MigrationEligibility};
use thiserror::Error;

use crate::controller::ServiceController;
use crate::controller::{ControlRefusal, Discovered, Observation, PreparedCandidate, Retirement};

/// Why a database-gated transition was not admitted.
#[derive(Debug, Error)]
pub enum DatabaseTransitionRefusal {
    #[error(
        "the transition would serve database {serving}, but the rehearsed evidence proves \
         candidate {proven}: a database no comparison covered cannot be activated"
    )]
    ServingDatabaseIsNotTheProvenCandidate {
        proven: DatabaseFingerprint,
        serving: DatabaseFingerprint,
    },
    #[error(
        "the source moved after it was audited: rehearsal audited {audited}, observed {observed}. \
         The migrated candidate does not contain the writes that landed in between, and \
         activating it would discard them silently"
    )]
    SourceMovedSinceItWasAudited {
        audited: DatabaseFingerprint,
        observed: DatabaseFingerprint,
    },
    #[error(
        "the incumbent's retirement was {retirement}, so it may still be writing to the source; \
         a source observation taken now could be stale before the switch commits"
    )]
    IncumbentMayStillBeWriting { retirement: &'static str },
    #[error(transparent)]
    Control(#[from] ControlRefusal),
}

impl DatabaseTransitionRefusal {
    /// Stable corpus/mutation label, following the controller's convention:
    /// a delegated control refusal keeps ITS label rather than collapsing into
    /// a generic one, so a corpus row still names the precise fact.
    pub fn label(&self) -> &'static str {
        match self {
            DatabaseTransitionRefusal::ServingDatabaseIsNotTheProvenCandidate { .. } => {
                "transition-serving-database-unproven"
            }
            DatabaseTransitionRefusal::SourceMovedSinceItWasAudited { .. } => {
                "transition-source-moved-since-audit"
            }
            DatabaseTransitionRefusal::IncumbentMayStillBeWriting { .. } => {
                "transition-incumbent-may-still-be-writing"
            }
            DatabaseTransitionRefusal::Control(refusal) => refusal.label(),
        }
    }
}

/// Proof that the source was observed AFTER the incumbent stopped writing to
/// it.
///
/// # The race this closes
///
/// A rehearsal audits the source, migrates a copy, and mints eligibility. The
/// candidate is that copy. If the incumbent writes to the SOURCE after the
/// audit, the candidate does not contain those writes — and activating it
/// discards them **silently**, because the loss happens at activation, not at
/// rollback, so no recovery disposition would ever report it.
///
/// Eligibility already refuses to be *minted* from a moved source. That is not
/// enough on its own: it fires when evidence is taken, and says nothing about a
/// write landing after a fresh receipt is already in hand. The window between
/// final validation and activation is the one that matters, and it is closed by
/// requiring two things at once rather than either alone:
///
/// 1. the incumbent is **not still writing** — this value cannot be built from a
///    retirement that timed out or failed; and
/// 2. the source is observed **after** that fact, and the observation still
///    matches what the rehearsal audited.
///
/// Ordering is what makes it a proof rather than a coincidence. An observation
/// taken while the incumbent was live could go stale one instruction later;
/// taken after retirement, nothing is left running that could move it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuiescedSource {
    observed: DatabaseFingerprint,
}

impl QuiescedSource {
    /// Build the proof from an observed retirement plus a source fingerprint
    /// read after it.
    ///
    /// `NoIncumbent` qualifies for the same reason `Retired` does: there is
    /// nothing running that could write. `TimedOut` and `Failed` do NOT — a
    /// process that would not go away is precisely the one that may still be
    /// writing, and admitting it would let the transition proceed on the
    /// strength of an observation that could already be wrong.
    ///
    /// The caller supplies the fingerprint because this crate reads no files.
    /// It must be read AFTER the retirement it is paired with; that ordering is
    /// the caller's obligation and is what the isolated rehearsal exercises.
    pub fn observed_after(
        retirement: &Retirement,
        observed: DatabaseFingerprint,
    ) -> Result<Self, DatabaseTransitionRefusal> {
        match retirement {
            Retirement::Retired | Retirement::NoIncumbent => Ok(Self { observed }),
            Retirement::TimedOut => Err(DatabaseTransitionRefusal::IncumbentMayStillBeWriting {
                retirement: "timed-out",
            }),
            Retirement::Failed { .. } => {
                Err(DatabaseTransitionRefusal::IncumbentMayStillBeWriting {
                    retirement: "failed",
                })
            }
        }
    }

    pub fn observed(&self) -> &DatabaseFingerprint {
        &self.observed
    }
}

/// Prepare a candidate that will serve a migrated database.
///
/// Two questions, both of which must hold:
///
/// **Is the source still what the rehearsal audited?** Answered against a
/// [`QuiescedSource`], which can only exist if the incumbent is not still
/// writing. A source that moved between the audit and here means the candidate
/// is missing those writes, so activating it would lose them silently.
///
/// **Is the database this candidate will write the one the rehearsal proved?**
/// Answered against the evidence's CANDIDATE, not its source. The source is the
/// pre-migration database; serving it would mean serving unmigrated data under
/// an engine that believes the migration happened.
///
/// The eligibility and the quiesced observation are both REQUIRED, not
/// optional. A caller cannot express "activate without rehearsed evidence" or
/// "activate without checking the source held still" through this function at
/// all, which is stronger than refusing either at runtime would be.
///
/// On success the caller holds an ordinary [`PreparedCandidate`] and commits it
/// through the controller exactly as any other switch commits. Nothing here
/// bypasses, wraps or replaces that path.
pub fn prepare_database_transition(
    controller: &ServiceController<'_>,
    discovered: &Discovered,
    candidate: &Observation,
    serving: &DatabaseFingerprint,
    quiesced: &QuiescedSource,
    eligibility: &MigrationEligibility,
) -> Result<PreparedCandidate, DatabaseTransitionRefusal> {
    // The race first: a moved source is a data-loss condition, and it stays
    // true no matter what else is right.
    if !eligibility.describes_source(quiesced.observed()) {
        return Err(DatabaseTransitionRefusal::SourceMovedSinceItWasAudited {
            audited: eligibility.source().file().clone(),
            observed: quiesced.observed().clone(),
        });
    }
    let proven = eligibility.candidate().file();
    if proven != serving {
        return Err(
            DatabaseTransitionRefusal::ServingDatabaseIsNotTheProvenCandidate {
                proven: proven.clone(),
                serving: serving.clone(),
            },
        );
    }
    Ok(controller.prepare(discovered, candidate)?)
}

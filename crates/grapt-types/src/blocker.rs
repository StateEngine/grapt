//! Permission/Form blocker state machine (contract §"Permission and Form
//! blocker lifecycle"). One shared machine, different terminal payloads per
//! blocker family.
//!
//! Reason-required law (`CORRECT`, D1-empirically grounded): the oracle's SQL
//! has a nullable `suspended_reason` and Permission reads `?? "process-loss"`.
//! Here `Suspended` cannot exist without its reason — decode of a suspended
//! row lacking a reason must reject (migration quarantines/reports it).
//! D1 also proved 10 real `replied` rows still carry a lingering
//! `suspended_reason`: on TERMINAL rows the column is historical metadata and
//! must not round-trip into `Suspended`.
//!
//! Family law (`CORRECT`): the machine is parameterised by a *sealed family*,
//! not by two independently chosen payload types. An earlier shape,
//! `BlockerState<Reply, Reason = SuspensionReason>`, was wrong in three ways
//! that a convention cannot fix:
//!
//! 1. `Reply` and `Reason` were independently substitutable, so
//!    `BlockerState<FormAnswer, PermissionSuspensionReason>` — a Form suspended
//!    for a Permission's reason — was a legal type.
//! 2. The `Reason` default meant the shared reason silently applied whenever a
//!    caller wrote the one-parameter form, so the canonical families were
//!    opt-in rather than mandatory.
//! 3. The shared `SuspensionReason` remained a public third owner of a
//!    vocabulary that `PermissionSuspensionReason` and `FormSuspensionReason`
//!    already own exhaustively.
//!
//! [`BlockerFamily`] fixes all three: the family chooses *both* payloads
//! together, the trait is sealed so no downstream crate can declare a fourth
//! family, and there is no default. The shared `SuspensionReason` is deleted
//! rather than deprecated — see ownership probes 03 and 03b.

use crate::form::{FormAnswer, FormSuspensionReason};
use crate::permission::{PermissionReply, PermissionSuspensionReason};
use core::fmt;

mod sealed {
    /// Sealing marker. Implemented only for the canonical families declared in
    /// this module, so [`super::BlockerFamily`] cannot be implemented outside
    /// this crate and a downstream caller cannot introduce a family whose
    /// payloads the lifecycle owners do not exhaustively match.
    pub trait BlockerFamilySealed {}
}

/// A blocker family: the settlement payload and the suspension reason chosen
/// **together**.
///
/// Sealed. The two associated types are what makes cross-family mixing a type
/// error rather than a convention: there is no way to name a state carrying a
/// [`FormAnswer`] alongside a [`PermissionSuspensionReason`], because no family
/// declares that pair.
pub trait BlockerFamily: sealed::BlockerFamilySealed {
    /// The payload a settled blocker of this family carries.
    type Reply: fmt::Debug + Clone + PartialEq + Eq;
    /// The reason a suspended blocker of this family carries.
    type Reason: fmt::Debug + Clone + Copy + PartialEq + Eq;
}

/// The Permission family: replies with a [`PermissionReply`], suspends with a
/// [`PermissionSuspensionReason`].
///
/// Uninhabited: it is a type-level tag, never a value. Nothing can hold one, so
/// it cannot be smuggled into a runtime decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionBlocker {}

/// The Form family: answers with a [`FormAnswer`], suspends with a
/// [`FormSuspensionReason`].
///
/// Uninhabited for the same reason as [`PermissionBlocker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormBlocker {}

impl sealed::BlockerFamilySealed for PermissionBlocker {}
impl BlockerFamily for PermissionBlocker {
    type Reply = PermissionReply;
    type Reason = PermissionSuspensionReason;
}

impl sealed::BlockerFamilySealed for FormBlocker {}
impl BlockerFamily for FormBlocker {
    type Reply = FormAnswer;
    type Reason = FormSuspensionReason;
}

/// Who cancelled. (Only the user cancels today; closed enum so a future actor
/// forces every match site to decide.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CancelledBy {
    User,
}

/// Shared blocker state, parameterised by its sealed [`BlockerFamily`].
///
/// `Suspended` carries no reply, no answer, and no live waiter — by
/// construction. Both payloads come from the one family parameter, so a
/// Permission cannot be suspended for a Form's reason: that pair is unnameable.
///
/// There is deliberately **no** default family. Writing `BlockerState` without
/// naming the family does not compile, which is what forces every owner to say
/// which lifecycle it is folding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockerState<F: BlockerFamily> {
    Issued,
    Settled(F::Reply),
    Cancelled(CancelledBy),
    Suspended(F::Reason),
}

impl<F: BlockerFamily> BlockerState<F> {
    /// Settled or Cancelled: no outgoing transition.
    pub fn is_terminal(&self) -> bool {
        matches!(self, BlockerState::Settled(_) | BlockerState::Cancelled(_))
    }

    /// The suspension reason, if this blocker is suspended.
    ///
    /// Terminal rows never yield one: the oracle's historical
    /// `suspended_reason` column on a `replied` row is metadata, not state.
    pub fn suspension_reason(&self) -> Option<F::Reason> {
        match self {
            BlockerState::Suspended(reason) => Some(*reason),
            _ => None,
        }
    }
}

/// The Permission blocker state.
pub type PermissionBlockerState = BlockerState<PermissionBlocker>;
/// The Form blocker state.
pub type FormBlockerState = BlockerState<FormBlocker>;

/// Waiter liveness is projection state, independent of durable blocker state:
/// a blocker created without `ask()` is visible but has no live waiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiter {
    None,
    Live(RuntimeGeneration),
}

/// Process-local runtime generation backing a live waiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeGeneration(u64);

impl RuntimeGeneration {
    pub const FIRST: RuntimeGeneration = RuntimeGeneration(0);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

/// Normalized CAS outcome — the two asymmetric sites in the oracle's
/// `permission.ts` become one vocabulary; no uncaught defect after a primary
/// event committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasOutcome {
    Applied,
    AlreadyTerminal,
    NotLive,
}

// `AuthoritySource` and `LayerName` used to live here as `{Layer, Fallback}`.
// The ratified law supersedes that two-variant vocabulary and moves ownership
// to `crate::authority`, where a default-policy decision names its exact policy
// version instead of collapsing into a bare `Fallback` marker.

/// A settlement ticket is consumed by its one successful transition —
/// `settle` takes `self`, so double settlement is a compile error
/// (ownership probe #6). Conditional SQL protects cross-process races;
/// this ownership protects in-process double settlement.
///
/// Family-typed for the same reason as [`BlockerState`]: a ticket carrying a
/// Form answer cannot settle a Permission blocker.
#[derive(Debug)]
pub struct SettlementTicket<F: BlockerFamily> {
    reply: F::Reply,
}

impl<F: BlockerFamily> SettlementTicket<F> {
    pub fn new(reply: F::Reply) -> Self {
        Self { reply }
    }

    pub fn settle(self) -> BlockerState<F> {
        BlockerState::Settled(self.reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspended_always_carries_its_reason() {
        // The only way to be suspended is with a reason — probe #3's runtime
        // half. (The compile-fail half lands with the probe suite.)
        let s: PermissionBlockerState =
            BlockerState::Suspended(PermissionSuspensionReason::ProcessLoss);
        assert!(!s.is_terminal());
        assert_eq!(
            s.suspension_reason(),
            Some(PermissionSuspensionReason::ProcessLoss)
        );
    }

    #[test]
    fn settled_and_cancelled_are_terminal() {
        let settled: PermissionBlockerState = BlockerState::Settled(PermissionReply::Once);
        assert!(settled.is_terminal());
        let c: FormBlockerState = BlockerState::Cancelled(CancelledBy::User);
        assert!(c.is_terminal());
    }

    #[test]
    fn a_terminal_row_never_reports_a_suspension_reason() {
        // D1: 10 real `replied` rows still carry a lingering suspended_reason.
        // A settled state has nowhere to put one, so the column cannot
        // round-trip into `Suspended`.
        let settled: PermissionBlockerState = BlockerState::Settled(PermissionReply::Reject);
        assert_eq!(settled.suspension_reason(), None);
    }

    #[test]
    fn each_family_owns_both_of_its_payloads() -> Result<(), Box<dyn std::error::Error>> {
        // Both payloads travel with the family parameter. Naming a Permission
        // reply beside a Form reason is not merely discouraged — no family
        // declares that pair, so the type cannot be written at all. Probes 03
        // and 03b hold the compile-fail halves.
        let permission: PermissionBlockerState =
            BlockerState::Suspended(PermissionSuspensionReason::ProcessLoss);
        let form: FormBlockerState = BlockerState::Suspended(FormSuspensionReason::Shutdown);
        assert!(!permission.is_terminal());
        assert!(!form.is_terminal());

        let answered: FormBlockerState =
            BlockerState::Settled(FormAnswer::new("yes").ok_or("answer")?);
        assert!(answered.is_terminal());
        Ok(())
    }

    #[test]
    fn ticket_settles_exactly_once() {
        let t = SettlementTicket::<PermissionBlocker>::new(PermissionReply::Once);
        let settled = t.settle();
        assert_eq!(settled, BlockerState::Settled(PermissionReply::Once));
        // `t.settle()` again would not compile: settle(self) consumed it.
    }
}

//! The selection receipt: the durable fact that a given engine was attached,
//! and the refusals that keep it truthful.
//!
//! # Why a receipt and not a URL
//!
//! The frozen desktop's health probe (`checkHealth`) accepts **any** 2xx from
//! `GET /api/health`. That proves something listened at the endpoint; it does
//! not prove which engine answered. If a selection naming the Rust engine in
//! fact reached a stale retained sidecar on the same port, a differential over
//! the two would report perfect parity — a green produced by measuring one
//! engine twice.
//!
//! So attachment is admitted from an owner identity, never from liveness. The
//! identity coordinates are the landed ones:
//! `{ServiceSlotId, ServiceInstanceId, EnvironmentReceipt}`, with PID and
//! endpoint travelling as attributes. A reused PID therefore cannot inherit
//! ownership, which is exactly the confusion a port-reuse race would otherwise
//! create.

use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{Endpoint, OwnerReceipt};
use thiserror::Error;

use crate::selection::EngineSelection;

/// What a health probe produced.
///
/// The two variants exist so that "the service answered" and "the service named
/// its owner" cannot be the same fact. A probe that only proves liveness has no
/// path to a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// A 2xx answer that named no owner. Liveness without attribution.
    HealthyUnattributed,
    /// The service named the owner of its registration slot.
    Attributed(OwnerReceipt),
}

/// Why an attachment or a differential was refused.
///
/// Each variant is a distinct law. They are separate so a witness for one
/// cannot pass by tripping another — the failure classes this lane must keep
/// independent are exactly these.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AttachmentRefusal {
    /// A probe answered but named no owner, so nothing can be attributed to it.
    #[error(
        "the probe answered but named no owner: a health answer proves liveness, not which engine served it"
    )]
    Unattributed,
    /// The observed owner is not the one this receipt recorded.
    #[error(
        "the observed owner is not the owner this selection recorded: the recorded attachment is stale and must be re-admitted, not reused"
    )]
    NotTheRecordedOwner,
    /// Both sides of a differential name the same owner.
    #[error(
        "both sides of the differential name the same owner: this is one engine measured twice, not a comparison"
    )]
    SameOwnerOnBothSides,
    /// Both sides of a differential name the same engine.
    #[error(
        "both sides of the differential name the same engine selection: a differential compares two different engines"
    )]
    SameSelectionOnBothSides,
}

impl AttachmentRefusal {
    /// The stable label used in the projection and the corpus.
    ///
    /// Fixed independently of the variant names for the same reason
    /// [`EngineSelection::label`](crate::EngineSelection::label) is: a recorded
    /// refusal must keep meaning the same thing after a rename, and a reader
    /// comparing an old projection against a new build must not silently see a
    /// different word for the same law.
    pub fn label(&self) -> &'static str {
        match self {
            AttachmentRefusal::Unattributed => "unattributed",
            AttachmentRefusal::NotTheRecordedOwner => "not-the-recorded-owner",
            AttachmentRefusal::SameOwnerOnBothSides => "same-owner-on-both-sides",
            AttachmentRefusal::SameSelectionOnBothSides => "same-selection-on-both-sides",
        }
    }
}

/// The durable fact that an engine was attached.
///
/// It owns exactly three coordinates — the selection, the owner receipt, and
/// the instant it was minted. The endpoint is deliberately **not** a field:
/// it is read through the owner, so there is one carrier for it and no way for
/// a receipt to disagree with itself about where it attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionReceipt {
    selection: EngineSelection,
    owner: OwnerReceipt,
    selected_at: Timestamp,
}

impl SelectionReceipt {
    /// Admit an attachment, or refuse it.
    ///
    /// This is the **only** constructor. There is no `new` taking a
    /// [`Timestamp`], because such a constructor would let a caller supply
    /// ambient time and defeat the injected clock the deterministic-authority
    /// law requires. The clock is borrowed, never stored: a receipt is a fact
    /// about an instant, not a handle to a clock.
    pub fn admit(
        selection: EngineSelection,
        probe: ProbeOutcome,
        clock: &dyn ClockAuthority,
    ) -> Result<Self, AttachmentRefusal> {
        let owner = match probe {
            ProbeOutcome::HealthyUnattributed => return Err(AttachmentRefusal::Unattributed),
            ProbeOutcome::Attributed(owner) => owner,
        };
        Ok(Self {
            selection,
            owner,
            selected_at: clock.now(),
        })
    }

    pub fn selection(&self) -> EngineSelection {
        self.selection
    }

    pub fn owner(&self) -> &OwnerReceipt {
        &self.owner
    }

    pub fn selected_at(&self) -> Timestamp {
        self.selected_at
    }

    /// Where this receipt attached, read through the owner.
    ///
    /// Not a stored field. If the corpus repeats the endpoint alongside the
    /// owner, the owner's value is the authority and the two must be equal.
    pub fn endpoint(&self) -> &Endpoint {
        self.owner.endpoint()
    }

    /// Whether this recorded attachment still describes the observed service.
    ///
    /// Compares owner IDENTITY, so a service that restarted into a new PID or a
    /// new ephemeral port is still the same owner, while a different instance
    /// that happens to have inherited the PID is not.
    pub fn still_current(&self, observed: &OwnerReceipt) -> Result<(), AttachmentRefusal> {
        if self.owner.is_same_owner(observed) {
            Ok(())
        } else {
            Err(AttachmentRefusal::NotTheRecordedOwner)
        }
    }
}

/// Two receipts admitted as a comparison of two genuinely different engines.
///
/// Constructing one is the only way to claim a differential, and the two
/// refusals below are what stop the degenerate case that would otherwise report
/// perfect parity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedDifferential {
    left: SelectionReceipt,
    right: SelectionReceipt,
}

impl AdmittedDifferential {
    /// Admit a pair, or refuse it.
    ///
    /// Refuses when both sides name the same owner (one engine measured twice)
    /// and when both name the same engine selection (not a comparison at all).
    /// The two checks are separate because they fail for different reasons and
    /// a single witness for both would leave one of them unproven.
    pub fn admit(
        left: SelectionReceipt,
        right: SelectionReceipt,
    ) -> Result<Self, AttachmentRefusal> {
        if left.owner().is_same_owner(right.owner()) {
            return Err(AttachmentRefusal::SameOwnerOnBothSides);
        }
        if left.selection() == right.selection() {
            return Err(AttachmentRefusal::SameSelectionOnBothSides);
        }
        Ok(Self { left, right })
    }

    pub fn left(&self) -> &SelectionReceipt {
        &self.left
    }

    pub fn right(&self) -> &SelectionReceipt {
        &self.right
    }
}

/// The selector, as a state rather than a stored string.
///
/// `Retained` is the frozen desktop's own behaviour when `defaultServerUrl` is
/// unset. `Attached` carries the receipt that admitted the attachment. The
/// switch is reversible by construction: [`revert`](Self::revert) is total and
/// consumes the attachment, so no state can be reached from which the retained
/// engine is unreachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorState {
    /// No endpoint selected; the desktop spawns its retained sidecar.
    Retained,
    /// Attached to the engine this receipt admitted.
    Attached(SelectionReceipt),
}

impl SelectorState {
    /// Attach, replacing whatever was selected before.
    pub fn attach(receipt: SelectionReceipt) -> Self {
        SelectorState::Attached(receipt)
    }

    /// Return to the retained engine.
    ///
    /// Total on purpose: reverting is always available, from every state,
    /// without consulting the receipt. That totality *is* the reversibility
    /// claim — a `revert` that could decline would make the selector one-way.
    pub fn revert(self) -> Self {
        SelectorState::Retained
    }

    /// The receipt for the current attachment, if any.
    pub fn receipt(&self) -> Option<&SelectionReceipt> {
        match self {
            SelectorState::Retained => None,
            SelectorState::Attached(receipt) => Some(receipt),
        }
    }
}

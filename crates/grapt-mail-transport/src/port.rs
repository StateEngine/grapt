//! The sealed port: seven operations and nothing else.
//!
//! # Why seven
//!
//! The admission packet defined six — open, subscribe-and-snapshot, fetch page,
//! acknowledge delivery, next change, close. Ruling J1 commissioned exactly
//! those "PLUS a distinct bulk-capable DISMISS (archive-without-read-claim)".
//! Seven is therefore the whole surface; an eighth needs a new ruling, and a
//! sixth would mean dismissal got folded into something it is not.
//!
//! # Why the methods are async, and why this crate has no runtime
//!
//! The sync/async constitution puts provider and network reads on the async
//! side, so a blocking port would be wrong at the boundary that actually does
//! I/O. These methods therefore return `impl Future`, matching the shape the
//! workspace already uses at its provider edge (`grapt_runtime::ProviderReader`
//! returns `impl Future<Output = Option<AttemptFragment>> + Send`).
//!
//! No executor is a dependency here. The futures are DRIVEN by the R3b-2C
//! runtime owner, which owns cancellation, reconnect scheduling and fair
//! slicing. Owning a runtime in this crate would hand the transport a
//! scheduling responsibility the lane split explicitly denies it.
//!
//! # What an implementation may never do
//!
//! Expose a foreign client, registry row, ledger cursor, authentication token or
//! raw JSON to store or runtime; read a foreign registry file, private table or
//! authority record; or write a foreign ledger directly. Only the supported
//! public delivery-acknowledgement operation may create the foreign-owned
//! delivery receipt.

use crate::ack::{DeliveryAckOutcome, DeliveryAdmissionAck};
use crate::binding::{MailBinding, OpenRequest};
use crate::cursor::{ArmedMailboxSubscription, MailSnapshot, RemoteMailPage};
use crate::identity::ExclusiveSourceCursor;
use crate::limits::ProviderLimits;
use crate::operation::OperationFailure;
use crate::signal::{BulkDismissOutcome, BulkDismissRequest, ChangeSignal};
use core::future::Future;

/// Where a reconnecting reader wants to resume from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResumeRequest {
    from_checkpoint: Option<ExclusiveSourceCursor>,
}

impl ResumeRequest {
    /// A first connection with no durable checkpoint yet.
    ///
    /// The provider must start at its retained origin. It must NOT start at the
    /// head: mail that arrived before this seat ever booted is still owed.
    pub fn cold() -> Self {
        ResumeRequest {
            from_checkpoint: None,
        }
    }

    /// A reconnect resuming strictly after a durable checkpoint.
    pub fn from_checkpoint(cursor: ExclusiveSourceCursor) -> Self {
        ResumeRequest {
            from_checkpoint: Some(cursor),
        }
    }

    pub fn checkpoint(&self) -> Option<&ExclusiveSourceCursor> {
        self.from_checkpoint.as_ref()
    }

    pub fn is_cold(&self) -> bool {
        self.from_checkpoint.is_none()
    }
}

/// The simultaneous bounds one page must satisfy.
///
/// All of them apply at once. A page ends before the next WHOLE message would
/// breach any one of them, and a single message that cannot fit the total bound
/// fails typed rather than being truncated — a half message is not a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLimits {
    max_messages: u32,
    max_aggregate_body_bytes: usize,
}

impl PageLimits {
    pub fn new(max_messages: u32, max_aggregate_body_bytes: usize) -> Self {
        PageLimits {
            max_messages,
            max_aggregate_body_bytes,
        }
    }

    /// The bounds a settled binding negotiated.
    pub fn from_settled(limits: &crate::limits::SettledTransportLimits) -> Self {
        PageLimits {
            max_messages: limits.messages_per_page(),
            max_aggregate_body_bytes: limits.aggregate_body_bytes_per_page(),
        }
    }

    pub fn max_messages(&self) -> u32 {
        self.max_messages
    }

    pub fn max_aggregate_body_bytes(&self) -> usize {
        self.max_aggregate_body_bytes
    }
}

/// A caller-supplied finite shutdown budget, in injected ticks.
///
/// Ticks, not wall time: the clock is injected, and a production adapter lowers
/// ticks to time only at its named async edge. That keeps the fake and the real
/// adapter running the same vectors deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownBudget {
    max_ticks: u32,
    max_joined_tasks: u32,
}

impl ShutdownBudget {
    pub fn new(max_ticks: u32, max_joined_tasks: u32) -> Self {
        ShutdownBudget {
            max_ticks,
            max_joined_tasks,
        }
    }

    pub fn max_ticks(&self) -> u32 {
        self.max_ticks
    }

    pub fn max_joined_tasks(&self) -> u32 {
        self.max_joined_tasks
    }
}

/// Proof that a close actually stopped what it owned.
///
/// A warning-only close is insufficient, which is why this is a receipt with
/// counts rather than a unit success: the reconciliation addendum found the
/// current surfaces return no joined-task proof at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownReceipt {
    joined_tasks: u32,
    ticks_used: u32,
}

impl ShutdownReceipt {
    pub fn new(joined_tasks: u32, ticks_used: u32) -> Self {
        ShutdownReceipt {
            joined_tasks,
            ticks_used,
        }
    }

    pub fn joined_tasks(&self) -> u32 {
        self.joined_tasks
    }

    pub fn ticks_used(&self) -> u32 {
        self.ticks_used
    }
}

/// The seal.
///
/// `Sealed` is `pub` inside a PRIVATE module, so it is nameable everywhere
/// inside this crate and nowhere outside it. A downstream crate cannot write
/// `impl Sealed`, and therefore cannot satisfy the supertrait bound on
/// [`RemoteSeatMailTransportV1`] however much of the public API it can see.
///
/// This is the structural half of what `binding.rs` used to assert in prose. A
/// public trait plus a public `MailBinding::new` meant an external crate could
/// mint a binding naming any `verified_principal` it liked, with no adapter and
/// no verification of any kind — and `verify_expectations` cannot catch it,
/// because it compares the caller's request against a receipt the same caller
/// authored, which is vacuous on a forged binding.
mod sealed {
    /// Implemented only by crate-owned adapters.
    pub trait Sealed {}
}

pub(crate) use sealed::Sealed;

/// `remote-seat-mail-transport/v1`.
///
/// One verified recipient converges from one foreign durable mail source. The
/// implementation owns authentication, protocol negotiation, strict source DTO
/// decode, conversion, transport I/O and failure classification, and exposes
/// none of them.
///
/// # Sealed: callable by anyone, implementable by no one outside this crate
///
/// Public consumers may CALL every operation here. They may not IMPLEMENT the
/// trait: it has a private supertrait, so `impl RemoteSeatMailTransportV1 for
/// MyAdapter` does not compile downstream. The real-integration release
/// admits the crate-owned HTTP adapter alongside the deterministic fake;
/// external adapters still cannot manufacture authenticated authority.
///
/// The seal matters because a [`MailBinding`] is this crate's proof that an
/// AUTHENTICATED open happened, and R3b-2B/2C are expected to read
/// `receipt().verified_principal()` as authority. Anything that could return a
/// binding could manufacture that authority.
///
/// Probe `03` proves a binding cannot be constructed directly and probe `04`
/// proves this trait cannot be implemented externally.
pub trait RemoteSeatMailTransportV1: Sealed {
    /// Verifies the binding and settles its immutable limit profile.
    ///
    /// The provider maps its AUTHENTICATED principal to the expected recipient.
    /// A message's own `to` field can neither establish nor widen that mapping.
    /// Under ruling J2 a second opener is not refused: it takes over, and the
    /// returned binding's receipt names whoever it displaced.
    fn open(
        &mut self,
        request: OpenRequest,
        limits: ProviderLimits,
    ) -> impl Future<Output = Result<MailBinding, OperationFailure>> + Send;

    /// The ONE admitted handshake: arm a subscription and fix a snapshot at one
    /// linearization point.
    ///
    /// There is no separate list-then-subscribe path, and ruling J3 refuses a v1
    /// multi-call fallback. An implementation that cannot do this atomically
    /// does not implement v1.
    fn subscribe_and_snapshot(
        &mut self,
        binding: &MailBinding,
        resume: ResumeRequest,
    ) -> impl Future<Output = Result<ArmedMailboxSubscription, OperationFailure>> + Send;

    /// One bounded page of the fixed snapshot, strictly after `after`.
    ///
    /// `after` absent means the snapshot's retained origin — never the head.
    fn fetch_snapshot_page(
        &mut self,
        binding: &MailBinding,
        snapshot: &MailSnapshot,
        after: Option<&ExclusiveSourceCursor>,
        page: PageLimits,
    ) -> impl Future<Output = Result<RemoteMailPage, OperationFailure>> + Send;

    /// Records that one exact message was durably admitted here.
    ///
    /// Never marks read or handled, answers, resolves, archives, dismisses,
    /// supersedes or closes anything.
    fn acknowledge_delivery(
        &mut self,
        binding: &MailBinding,
        ack: DeliveryAdmissionAck,
    ) -> impl Future<Output = Result<DeliveryAckOutcome, OperationFailure>> + Send;

    /// Ruling J1's seventh operation: bulk archive-without-read-claim.
    ///
    /// Distinct from [`Self::acknowledge_delivery`] and from any read
    /// acknowledgement. A dismisser never becomes a reader.
    fn dismiss(
        &mut self,
        binding: &MailBinding,
        request: BulkDismissRequest,
    ) -> impl Future<Output = Result<BulkDismissOutcome, OperationFailure>> + Send;

    /// The next payload-free advisory signal, or the typed reason the
    /// subscription ended.
    // The returned poll owns its coordinates. Other operations can run while
    // it waits; dropping the future cancels the pending request. No task detaches.
    fn next_change(
        &mut self,
        subscription: &mut ArmedMailboxSubscription,
    ) -> impl Future<Output = Result<ChangeSignal, OperationFailure>> + Send + use<Self>;

    /// Cancels the subscription and joins owned transport tasks within a finite
    /// budget, returning proof or a typed timeout.
    ///
    /// Takes the binding BY VALUE, so a closed binding cannot be used again.
    fn close(
        &mut self,
        binding: MailBinding,
        budget: ShutdownBudget,
    ) -> impl Future<Output = Result<ShutdownReceipt, OperationFailure>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LifecycleOutcome;

    #[test]
    fn a_cold_resume_starts_at_the_origin_not_the_head() {
        let cold = ResumeRequest::cold();
        assert!(cold.is_cold());
        assert!(cold.checkpoint().is_none());
    }

    #[test]
    fn a_warm_resume_carries_its_exact_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
        let cursor = ExclusiveSourceCursor::new("cur-7").ok_or("cursor")?;
        let warm = ResumeRequest::from_checkpoint(cursor);
        assert!(!warm.is_cold());
        assert_eq!(
            warm.checkpoint().map(ExclusiveSourceCursor::as_str),
            Some("cur-7")
        );
        Ok(())
    }

    #[test]
    fn page_limits_are_taken_from_the_settled_profile_not_reinvented() {
        // A deliberately smaller provider profile.
        let settled = crate::test_support::provider_with(
            4_096,
            8,
            16_384,
            crate::limits::GRAPT_MAX_DIAGNOSTIC_BYTES,
        )
        .settle();

        let page = PageLimits::from_settled(&settled);
        assert_eq!(page.max_messages(), 8);
        assert_eq!(page.max_aggregate_body_bytes(), 16_384);
    }

    #[test]
    fn a_shutdown_receipt_reports_counts_rather_than_a_bare_success() {
        let receipt = ShutdownReceipt::new(3, 12);
        assert_eq!(receipt.joined_tasks(), 3);
        assert_eq!(receipt.ticks_used(), 12);
        // The timeout side is a different fact carrying the remaining count.
        let timeout = LifecycleOutcome::ShutdownBudgetExhausted { tasks_remaining: 2 };
        assert_ne!(
            timeout,
            LifecycleOutcome::ShutdownBudgetExhausted { tasks_remaining: 0 }
        );
    }
}

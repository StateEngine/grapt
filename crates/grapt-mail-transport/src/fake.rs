//! The deterministic fake.
//!
//! Ruling J7 trimmed the compatibility environment to this: a deterministic fake
//! only in v1, with the credentialed real suite deferred to the
//! fork-as-daily-driver milestone. So this is not a convenience for tests — it
//! is the ONLY executable implementation of the port that v1 admits, and the
//! vectors it runs are the contract's evidence.
//!
//! # How it stays honest
//!
//! * it consumes an EXPLICIT transcript. Nothing is inferred, and a vector that
//!   wants a disconnect mid-page has to say so;
//! * identities and ticks are INJECTED. There is no clock and no randomness in
//!   this crate at all, so a vector's result cannot drift between runs;
//! * every physical port call is RECORDED. A vector can therefore assert what
//!   did NOT happen — "no second provider call", "zero acknowledgements" — which
//!   is the half most contract tests forget;
//! * it shares no control flow with a real adapter, which is trivially true
//!   today because no real adapter exists and none may be written under v1. When
//!   one arrives, R3b-2I runs these same immutable vectors against both.
//!
//! # What it deliberately does not do
//!
//! It does not model the store, the runtime owner, the coalesced wake bit or
//! reconnect scheduling. Those belong to R3b-2B and R3b-2C. A fake that grew
//! them would let this lane accidentally prove laws it does not own.

use crate::ack::{DeliveryAckOutcome, DeliveryAdmissionAck};
use crate::binding::{
    MailBinding, MailBindingReceipt, MailBindingReceiptParts, OpenRequest, TakeoverReceipt,
};
use crate::cursor::{ArmedMailboxSubscription, MailSnapshot, RemoteMailPage};
use crate::failure::{
    DeliveryAckFailure, LifecycleOutcome, OpenFailure, SubscriptionClosed, TransportFailure,
};
use crate::identity::{
    AdapterVersion, ExclusiveSourceCursor, ProjectPair, RecipientMappingVersion, SourceBindingId,
    SourcePrincipalId,
};
use crate::limits::ProviderLimits;
use crate::operation::OperationFailure;
use crate::port::{
    PageLimits, RemoteSeatMailTransportV1, ResumeRequest, ShutdownBudget, ShutdownReceipt,
};
use crate::signal::{BulkDismissOutcome, BulkDismissRequest, ChangeSignal};
use core::future::{ready, Future};
use grapt_types::mailbox::SeatRecipientId;
use grapt_types::protocol::ProtocolVersion;

/// One physical call the port actually received.
///
/// Recorded so a vector can assert absence as well as presence. `Open` carries
/// no request copy because the request is not `Copy` and the vectors that care
/// assert on the returned receipt instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortCall {
    Open,
    SubscribeAndSnapshot { cold: bool },
    FetchSnapshotPage { after: Option<String> },
    AcknowledgeDelivery { ack: Box<DeliveryAdmissionAck> },
    Dismiss { count: usize },
    NextChange,
    Close,
}

/// What the fake will pretend it verified on a scripted open.
///
/// This is INERT DESCRIPTION, not authority. A vector writes one down; only this
/// crate turns it into a [`MailBindingReceipt`], and only for the fake. That is
/// what lets binding-receipt construction stay `pub(crate)` while vectors — which
/// are external consumers of this crate — still script every open they need.
///
/// `protocol` is scriptable so a vector can stage a mismatch against its own
/// [`OpenRequest`]. The settled limit profile deliberately is NOT: see
/// [`DeterministicTransport::open`].
#[derive(Debug, Clone)]
pub struct ScriptedBinding {
    pub binding_id: SourceBindingId,
    pub protocol: ProtocolVersion,
    pub adapter_version: AdapterVersion,
    pub projects: ProjectPair,
    pub verified_principal: SourcePrincipalId,
    pub local_recipient: SeatRecipientId,
    pub mapping_version: RecipientMappingVersion,
    pub takeover: TakeoverReceipt,
}

/// What the fake should do for the next call of each kind.
///
/// Every field is a queue consumed front-to-back. An exhausted queue is an
/// explicit script error rather than a silent default, because a vector that
/// ran off the end of its own transcript is not proving what it claims.
#[derive(Debug, Default)]
pub struct TransportScript {
    pub open: Vec<Result<ScriptedBinding, OpenFailure>>,
    pub subscribe: Vec<Result<ArmedMailboxSubscription, TransportFailure>>,
    pub pages: Vec<Result<RemoteMailPage, TransportFailure>>,
    pub acks: Vec<Result<DeliveryAckOutcome, DeliveryAckFailure>>,
    pub dismissals: Vec<Result<BulkDismissOutcome, TransportFailure>>,
    pub changes: Vec<Result<ChangeSignal, SubscriptionClosed>>,
    pub closes: Vec<Result<ShutdownReceipt, LifecycleOutcome>>,
}

/// A deterministic implementation of [`RemoteSeatMailTransportV1`].
#[derive(Debug)]
pub struct DeterministicTransport {
    script: TransportScript,
    calls: Vec<PortCall>,
    lose_ack_response: bool,
}

impl DeterministicTransport {
    pub fn new(script: TransportScript) -> Self {
        DeterministicTransport {
            script,
            calls: Vec::new(),
            lose_ack_response: false,
        }
    }

    /// Explicit fixture cut: the next scripted successful ACK happened at the
    /// source, but its reply is lost. Local consumers must retain exact debt.
    pub fn lose_next_ack_response(&mut self) {
        self.lose_ack_response = true;
    }

    /// Every physical call, in order.
    pub fn calls(&self) -> &[PortCall] {
        &self.calls
    }

    /// How many calls of one shape were made.
    ///
    /// The workhorse for "and nothing else happened" assertions: a vector
    /// proving no retry occurred counts provider calls rather than trusting a
    /// transcript to be silent.
    pub fn count(&self, matching: impl Fn(&PortCall) -> bool) -> usize {
        self.calls.iter().filter(|call| matching(call)).count()
    }

    /// Panics with the exact exhausted queue rather than inventing a result.
    fn pop<T>(queue: &mut Vec<T>, what: &str) -> T {
        if queue.is_empty() {
            panic!(
                "deterministic transport script exhausted at {what}: the vector asked for a \
                 call its own transcript does not describe"
            );
        }
        queue.remove(0)
    }
}

fn operation_ready<T, E: Into<OperationFailure>>(
    value: Result<T, E>,
) -> core::future::Ready<Result<T, OperationFailure>> {
    ready(value.map_err(Into::into))
}

impl crate::port::Sealed for DeterministicTransport {}

impl RemoteSeatMailTransportV1 for DeterministicTransport {
    /// Settles the CALLER's profile rather than echoing a scripted one.
    ///
    /// An earlier revision took the whole receipt from the script and ignored
    /// its `limits` argument entirely, so the settled profile a vector observed
    /// had never been through [`ProviderLimits::settle`] at all. Settling here
    /// means the negotiated bounds every later call reads are the ones this open
    /// actually produced — which is also what puts the settled diagnostic bound
    /// on the vectors' path.
    fn open(
        &mut self,
        _request: OpenRequest,
        limits: ProviderLimits,
    ) -> impl Future<Output = Result<MailBinding, OperationFailure>> + Send {
        self.calls.push(PortCall::Open);
        let outcome = Self::pop(&mut self.script.open, "open");
        operation_ready(outcome.map(|scripted| {
            MailBinding::new(MailBindingReceipt::new(MailBindingReceiptParts {
                binding_id: scripted.binding_id,
                protocol: scripted.protocol,
                adapter_version: scripted.adapter_version,
                projects: scripted.projects,
                verified_principal: scripted.verified_principal,
                local_recipient: scripted.local_recipient,
                mapping_version: scripted.mapping_version,
                takeover: scripted.takeover,
                limits: limits.settle(),
            }))
        }))
    }

    fn subscribe_and_snapshot(
        &mut self,
        _binding: &MailBinding,
        resume: ResumeRequest,
    ) -> impl Future<Output = Result<ArmedMailboxSubscription, OperationFailure>> + Send {
        self.calls.push(PortCall::SubscribeAndSnapshot {
            cold: resume.is_cold(),
        });
        operation_ready(Self::pop(
            &mut self.script.subscribe,
            "subscribe_and_snapshot",
        ))
    }

    fn fetch_snapshot_page(
        &mut self,
        _binding: &MailBinding,
        _snapshot: &MailSnapshot,
        after: Option<&ExclusiveSourceCursor>,
        _page: PageLimits,
    ) -> impl Future<Output = Result<RemoteMailPage, OperationFailure>> + Send {
        self.calls.push(PortCall::FetchSnapshotPage {
            after: after.map(|cursor| cursor.as_str().to_owned()),
        });
        operation_ready(Self::pop(&mut self.script.pages, "fetch_snapshot_page"))
    }

    fn acknowledge_delivery(
        &mut self,
        _binding: &MailBinding,
        ack: DeliveryAdmissionAck,
    ) -> impl Future<Output = Result<DeliveryAckOutcome, OperationFailure>> + Send {
        self.calls
            .push(PortCall::AcknowledgeDelivery { ack: Box::new(ack) });
        let reply = Self::pop(&mut self.script.acks, "acknowledge_delivery");
        if reply.is_ok() && core::mem::take(&mut self.lose_ack_response) {
            return ready(Err(OperationFailure::new(
                crate::operation::OperationCause::Connection(
                    crate::operation::ConnectionFailure::Unavailable,
                ),
                crate::operation::WriteEffect::UnknownRetryExact,
            )));
        }
        operation_ready(reply)
    }

    fn dismiss(
        &mut self,
        _binding: &MailBinding,
        request: BulkDismissRequest,
    ) -> impl Future<Output = Result<BulkDismissOutcome, OperationFailure>> + Send {
        self.calls.push(PortCall::Dismiss {
            count: request.messages().len(),
        });
        operation_ready(Self::pop(&mut self.script.dismissals, "dismiss"))
    }

    fn next_change(
        &mut self,
        _subscription: &mut ArmedMailboxSubscription,
    ) -> impl Future<Output = Result<ChangeSignal, OperationFailure>> + Send + use<> {
        self.calls.push(PortCall::NextChange);
        operation_ready(Self::pop(&mut self.script.changes, "next_change"))
    }

    fn close(
        &mut self,
        _binding: MailBinding,
        _budget: ShutdownBudget,
    ) -> impl Future<Output = Result<ShutdownReceipt, OperationFailure>> + Send {
        self.calls.push(PortCall::Close);
        operation_ready(Self::pop(&mut self.script.closes, "close"))
    }
}

/// Drives an immediately-ready future to completion.
///
/// The fake never pends — it has no I/O and no timers — so this needs no
/// executor, no reactor and no dependency. A future that DID pend would spin
/// here forever, which is the correct outcome: it would mean the deterministic
/// fake had acquired non-determinism, and the vector should fail loudly rather
/// than quietly wait.
pub fn block_on<F: Future>(future: F) -> F::Output {
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_returns_a_ready_value() {
        assert_eq!(block_on(ready(7_u8)), 7);
    }

    #[test]
    fn every_call_is_recorded_in_order() -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = DeterministicTransport::new(TransportScript {
            open: vec![Err(OpenFailure::Unauthenticated)],
            ..TransportScript::default()
        });
        let request = OpenRequest::new(
            crate::identity::TargetProjectId::new("secode").ok_or("target")?,
            grapt_types::mailbox::SeatRecipientId::new("seat-delta").ok_or("recipient")?,
            grapt_types::protocol::ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1)
                .ok_or("protocol")?,
            crate::identity::MailTransportClientInstanceId::new("pid-1").ok_or("client")?,
        );
        let outcome = block_on(transport.open(request, crate::fake::tests::limits()));
        assert_eq!(outcome.err(), Some(OpenFailure::Unauthenticated.into()));
        assert_eq!(transport.calls(), [PortCall::Open]);
        assert_eq!(transport.count(|call| matches!(call, PortCall::Open)), 1);
        Ok(())
    }

    /// A `#[should_panic]` test must return `()`, so it cannot use `?` — and the
    /// workspace denies `expect`/`unwrap`, so it cannot use those either. The
    /// `let ... else` binding is the remaining honest route: a broken fixture
    /// panics with its OWN message, which does not match the `expected` string,
    /// so this test still fails loudly rather than passing for the wrong reason.
    #[test]
    #[should_panic(expected = "script exhausted at open")]
    fn an_exhausted_script_panics_rather_than_inventing_a_result() {
        let mut transport = DeterministicTransport::new(TransportScript::default());
        let (Some(project), Some(recipient), Some(protocol), Some(client)) = (
            crate::identity::TargetProjectId::new("secode"),
            grapt_types::mailbox::SeatRecipientId::new("seat-delta"),
            grapt_types::protocol::ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1),
            crate::identity::MailTransportClientInstanceId::new("pid-1"),
        ) else {
            panic!("fixture identities are valid by construction")
        };
        let request = OpenRequest::new(project, recipient, protocol, client);
        let _ = block_on(transport.open(request, limits()));
    }

    pub(crate) fn limits() -> ProviderLimits {
        crate::test_support::provider_at_ceilings()
    }

    /// A scripted open whose settled diagnostic bound comes from the provider
    /// side rather than the Grapt ceiling.
    fn scripted() -> Option<ScriptedBinding> {
        Some(ScriptedBinding {
            binding_id: SourceBindingId::new("bind-1")?,
            protocol: ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1)?,
            adapter_version: AdapterVersion::new("deterministic-fake-1")?,
            projects: ProjectPair::new(
                crate::identity::SourceProjectId::new("stateengine")?,
                crate::identity::TargetProjectId::new("secode")?,
            ),
            verified_principal: SourcePrincipalId::new("agt-xztg")?,
            local_recipient: SeatRecipientId::new("seat-delta")?,
            mapping_version: RecipientMappingVersion::new("map-1")?,
            takeover: TakeoverReceipt::first_open(crate::binding::BindingGeneration::FIRST),
        })
    }

    #[test]
    fn an_open_settles_the_callers_profile_rather_than_a_scripted_one(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The fake used to ignore its `limits` argument entirely. A vector could
        // therefore observe a "settled" profile that had never been settled — so
        // a defect in `settle` was invisible to every vector that opened.
        let scripted = scripted().ok_or("scripted binding")?;
        let mut transport = DeterministicTransport::new(TransportScript {
            open: vec![Ok(scripted)],
            ..TransportScript::default()
        });
        let request = OpenRequest::new(
            crate::identity::TargetProjectId::new("secode").ok_or("target")?,
            SeatRecipientId::new("seat-delta").ok_or("recipient")?,
            ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1).ok_or("protocol")?,
            crate::identity::MailTransportClientInstanceId::new("pid-1").ok_or("client")?,
        );
        // A provider that can only carry 64 diagnostic bytes.
        let binding = block_on(transport.open(
            request,
            crate::test_support::provider_with(65_536, 64, 262_144, 64),
        ))
        .map_err(|e| format!("the script opens cleanly: {e:?}"))?;

        let settled = binding.receipt().limits();
        assert_eq!(
            settled.diagnostic_bytes(),
            64,
            "the settled profile must come from the limits this open was GIVEN"
        );
        Ok(())
    }
}

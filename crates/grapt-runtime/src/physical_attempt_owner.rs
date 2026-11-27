//! Private physical-attempt receipt owner.
//!
//! It is below unavailable campaign, permission, registry, and credential
//! authority. Ordinary application composition remains an admission refusal.

use std::num::NonZeroU32;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
use grapt_provider::{
    AttemptFragment, AttemptStream, DeterministicAdapter, FinishReason, ScriptBuilder,
};
use grapt_store::{
    AggregateId, AttemptRecordCeiling, AttemptSettlement, EventStore, Projector, ReserveInput,
    TerminalCoordinates, WriteError,
};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, IdAuthority,
    LostEffect, PhysicalAttemptId, ReceiptDisposition, RequestSentMarker, ReservedMarker,
    SessionId, SettledMarker, StepNumber,
};

use crate::owner::AttemptOwner;
use crate::residency::Residency;
use crate::supervisor::{
    AttemptReport, AttemptSupervisor, InterruptHandle, ProviderReader, ToolRunner,
};
#[cfg(test)]
use crate::{BoxFuture, DrainResidency, RecordingOwner, StepOutcome, ToolOutcome};

/// An opener that only this owner may consume after the durable send permit.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "ordinary application composition is intentionally fail closed pending the later authenticated join"
    )
)]
pub(crate) struct PrivateOpener<R>(Box<dyn FnOnce(PhysicalAttemptId) -> R + Send>);

pub(crate) struct PhysicalAttemptOpenInput<'a, R> {
    pub(crate) ids: &'a dyn IdAuthority,
    pub(crate) ceiling: AttemptRecordCeiling,
    pub(crate) session: &'a SessionId,
    pub(crate) step: StepNumber,
    pub(crate) ordinal: AttemptOrdinal,
    pub(crate) reserved: ReservedMarker,
    pub(crate) send_permit: RequestSentMarker,
    pub(crate) terminal: &'a TerminalCoordinates,
    pub(crate) opener: PrivateOpener<R>,
}

/// A sealed reserve-and-permit result that can enter only the owned supervisor.
#[cfg_attr(not(test), expect(dead_code, reason = "witness only"))]
struct PermittedAttempt<R> {
    attempt: PhysicalAttemptId,
    opener: PrivateOpener<R>,
}
impl<R> PrivateOpener<R> {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only the crate-private deterministic witness may construct an opener at this stage"
        )
    )]
    pub(crate) fn new(open: impl FnOnce(PhysicalAttemptId) -> R + Send + 'static) -> Self {
        Self(Box::new(open))
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the owner consumes this only from its private deterministic witness before the later authenticated join"
        )
    )]
    fn open(self, attempt: PhysicalAttemptId) -> R {
        (self.0)(attempt)
    }
}
#[derive(Debug, thiserror::Error)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the fail-closed production seam has no caller until the later authenticated join"
    )
)]
pub(crate) enum PhysicalAttemptOwnerError {
    #[error("IdAuthority returned an empty token")]
    EmptyIdToken,
    #[error("AttemptRecordCeiling was unexpectedly zero")]
    ZeroCeiling,
    #[error("attempt identity refuses the injected ceiling: {0:?}")]
    AttemptBound(grapt_types::IdTooLong),
    #[error("session identity refuses the injected ceiling: {0:?}")]
    SessionBound(grapt_types::IdTooLong),
    #[error("no durable receipt exists for recovery of {attempt}")]
    AttemptNotReserved { attempt: String },
    #[error("durable attempt receipt operation refused: {0}")]
    Store(#[from] WriteError),
}

/// The only recovery classifications visible below the sealed owner boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the later authenticated recovery driver is intentionally unavailable in this narrow prerequisite"
    )
)]
pub(crate) enum PhysicalAttemptRecovery {
    NoneInitiated,
    AmbiguousProcessLoss,
    AlreadySettled,
}

/// Mint, reserve, and durably permit before handing the one-shot opener to the supervisor.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the positive path is deliberately limited to crate-private deterministic evidence until its later authenticated join"
    )
)]
fn reserve_permit_handoff<R>(
    store: &mut EventStore,
    input: PhysicalAttemptOpenInput<'_, R>,
) -> Result<PermittedAttempt<R>, PhysicalAttemptOwnerError> {
    let PhysicalAttemptOpenInput {
        ids,
        ceiling,
        session,
        step,
        ordinal,
        reserved,
        send_permit,
        terminal,
        opener,
    } = input;
    let maximum = match NonZeroU32::new(ceiling.get()) {
        Some(value) => value,
        None => return Err(PhysicalAttemptOwnerError::ZeroCeiling),
    };
    let token = ids.mint();
    let attempt = match PhysicalAttemptId::new(token.as_str()) {
        Some(value) => value,
        None => return Err(PhysicalAttemptOwnerError::EmptyIdToken),
    };
    let bounded_attempt = BoundedAttemptId::new(attempt.clone(), maximum)
        .map_err(PhysicalAttemptOwnerError::AttemptBound)?;
    let bounded_session = BoundedSessionId::new(session.clone(), maximum)
        .map_err(PhysicalAttemptOwnerError::SessionBound)?;
    let durable_ceiling = ceiling;
    let receipt =
        DurableAttemptReceipt::reserved(bounded_attempt, bounded_session, step, ordinal, reserved);
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    store.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt,
        writer,
        terminal,
        ceiling: durable_ceiling,
    })?;
    let writer = store.fencing_token(&aggregate)?;
    let transaction = store.governed_transaction().map_err(WriteError::from)?;
    EventStore::permit_attempt_send_within(
        &transaction,
        &attempt,
        writer,
        durable_ceiling,
        send_permit,
    )?;
    transaction.commit().map_err(WriteError::from)?.discard();
    Ok(PermittedAttempt { attempt, opener })
}

#[cfg_attr(not(test), expect(dead_code, reason = "witness only"))]
impl<R> PermittedAttempt<R> {
    fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    fn prepare_supervisor<T, O>(
        self,
        tools: Arc<T>,
        owner: O,
        residency: Residency,
        step: tokio::sync::watch::Receiver<Option<grapt_types::InterruptionSource>>,
    ) -> (
        InterruptHandle,
        impl core::future::Future<Output = (O, AttemptReport)>,
    )
    where
        R: ProviderReader,
        T: ToolRunner,
        O: AttemptOwner,
    {
        let attempt = self.attempt;
        let opener = self.opener;
        let opened_attempt = attempt.clone();
        AttemptSupervisor::prepare(
            attempt,
            move || opener.open(opened_attempt),
            tools,
            owner,
            residency,
            step,
        )
    }
}

#[cfg(test)]
struct TestAdapterReader(AttemptStream);

#[cfg(test)]
impl ProviderReader for TestAdapterReader {
    fn next(&mut self) -> impl core::future::Future<Output = Option<AttemptFragment>> + Send {
        let next = self
            .0
            .find(|fragment| !matches!(fragment, AttemptFragment::AttemptOpened));
        async move { next }
    }
}

#[cfg(test)]
struct TestNoTools;

#[cfg(test)]
impl ToolRunner for TestNoTools {
    fn run(
        &self,
        _call: grapt_types::ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        Box::pin(async { ToolOutcome::Completed { output: None } })
    }
}

#[cfg(test)]
fn test_opener(
    adapter: Arc<Mutex<DeterministicAdapter>>,
    opens: Arc<AtomicUsize>,
) -> PrivateOpener<TestAdapterReader> {
    PrivateOpener::new(move |attempt| {
        opens.fetch_add(1, Ordering::SeqCst);
        let stream = match adapter.lock() {
            Ok(mut adapter) => match adapter.open(attempt) {
                Ok(stream) => stream,
                Err(error) => panic!("deterministic adapter must open once: {error}"),
            },
            Err(error) => panic!("deterministic adapter lock must remain usable: {error}"),
        };
        TestAdapterReader(stream)
    })
}

#[cfg(test)]
fn test_stopped_adapter() -> Arc<Mutex<DeterministicAdapter>> {
    let script = ScriptBuilder::new().finish(FinishReason::Stop, None);
    Arc::new(Mutex::new(DeterministicAdapter::new([script])))
}

#[cfg(test)]
async fn drive_test_attempt(permitted: PermittedAttempt<TestAdapterReader>) -> RecordingOwner {
    // A Step watch that never publishes: this witness drives a healthy
    // attempt to its own terminal, so no interrupt source is exercised.
    let (step_tx, step_rx) = tokio::sync::watch::channel(None);
    let (_, run) = permitted.prepare_supervisor(
        Arc::new(TestNoTools),
        RecordingOwner::new(),
        DrainResidency::new().token(),
        step_rx,
    );
    drop(step_tx);
    let (owner, report) = run.await;
    assert!(matches!(
        report.outcome,
        StepOutcome::Ended {
            finish: FinishReason::Stop
        }
    ));
    owner
}

/// Classify durable recovery without opening, replaying, or minting anything.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the later authenticated recovery driver is intentionally unavailable in this narrow prerequisite"
    )
)]
pub(crate) fn classify_recovery(
    store: &EventStore,
    attempt: &PhysicalAttemptId,
    ceiling: AttemptRecordCeiling,
) -> Result<PhysicalAttemptRecovery, PhysicalAttemptOwnerError> {
    let record = store.attempt_receipt(attempt, ceiling)?.ok_or_else(|| {
        PhysicalAttemptOwnerError::AttemptNotReserved {
            attempt: attempt.as_str().to_owned(),
        }
    })?;
    Ok(match record.receipt().disposition() {
        ReceiptDisposition::AlreadySettled => PhysicalAttemptRecovery::AlreadySettled,
        ReceiptDisposition::Lost(LostEffect::NoneInitiated) => {
            PhysicalAttemptRecovery::NoneInitiated
        }
        ReceiptDisposition::Lost(LostEffect::Ambiguous { .. }) => {
            PhysicalAttemptRecovery::AmbiguousProcessLoss
        }
    })
}

/// Invoke the existing one-transaction process-loss terminal path, never a marker-only write.
pub(crate) fn settle_process_loss_recovery(
    store: &mut EventStore,
    attempt: &PhysicalAttemptId,
    ceiling: AttemptRecordCeiling,
    settled_at: SettledMarker,
    projectors: &mut [&mut dyn Projector],
) -> Result<AttemptSettlement, PhysicalAttemptOwnerError> {
    let record = store.attempt_receipt(attempt, ceiling)?.ok_or_else(|| {
        PhysicalAttemptOwnerError::AttemptNotReserved {
            attempt: attempt.as_str().to_owned(),
        }
    })?;
    let aggregate = AggregateId::Session(record.receipt().session().clone());
    let writer = store.fencing_token(&aggregate)?;
    let transaction = store.governed_transaction().map_err(WriteError::from)?;
    let settlement = EventStore::settle_attempt_process_loss_within(
        &transaction,
        attempt,
        writer,
        ceiling,
        settled_at,
        projectors,
    )?;
    transaction.commit().map_err(WriteError::from)?.discard();
    Ok(settlement)
}

#[cfg(test)]
mod tests;

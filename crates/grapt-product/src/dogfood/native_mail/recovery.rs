//! Bounded retry timing; the existing runtime retains the exact binding and debt.
//! Copyright 2025 OpenAI.
//! Apache-2.0. Modified by Grapt: closed mail causes and a retained timer,
//! without ambient deadlines, a new connection or a detached retry task.
use super::*;
use grapt_mail_transport::{
    operation::{ConnectionFailure, OperationCause, OperationFailure, WriteEffect},
    DeliveryAckFailure, LifecycleOutcome, SubscriptionClosed, TransportFailure,
};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

/// A refusal is not permission to replace a binding, cursor, capability or ACK.
/// In particular, authentication and takeover remain terminal for this owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Exact,
    /// An uncertain write: it must be repeated with its exact coordinates,
    /// on the same binding, however many times that takes.
    ExactWrite,
    Reconnect,
}

pub(super) fn action(error: &RemoteMailboxError) -> Option<Action> {
    match error {
        RemoteMailboxError::Deadline => Some(Action::Exact),
        RemoteMailboxError::Port(failure) => port_action(failure).map(|action| {
            if action == Action::Exact && failure.effect == WriteEffect::UnknownRetryExact {
                Action::ExactWrite
            } else {
                action
            }
        }),
        _ => None,
    }
}

fn port_action(failure: &OperationFailure) -> Option<Action> {
    {
        match failure.cause {
            OperationCause::Connection(
                ConnectionFailure::Unavailable | ConnectionFailure::Deadline,
            )
            | OperationCause::Transport(
                TransportFailure::TransportUnavailable | TransportFailure::SubscriptionRefused,
            )
            | OperationCause::Lifecycle(
                LifecycleOutcome::ConnectBudgetExhausted
                | LifecycleOutcome::ReconnectBudgetExhausted
                | LifecycleOutcome::SliceBudgetExhausted { .. },
            ) => Some(Action::Exact),
            OperationCause::Transport(
                TransportFailure::Disconnected | TransportFailure::SnapshotExpired,
            )
            | OperationCause::Subscription(SubscriptionClosed::Disconnected) => {
                Some(if failure.effect == WriteEffect::UnknownRetryExact {
                    Action::Exact
                } else {
                    Action::Reconnect
                })
            }
            // Transient by the source's own reason; the credential budget caps it.
            OperationCause::Open(grapt_mail_transport::OpenFailure::UnauthenticatedRetry) => {
                Some(Action::Reconnect)
            }
            OperationCause::Delivery(DeliveryAckFailure::MessageNeverEnumerated) => {
                (failure.effect == WriteEffect::UnknownRetryExact).then_some(Action::Exact)
            }
            _ => None,
        }
    }
}

/// The producer also closes advisory waits when its server shuts down, and an
/// intermediary can answer a long poll with an unreadable reply. Neither can
/// become mail, so reopen only while this consumer still owns the current
/// authenticated subscription; its own close/cancel, typed authentication and
/// takeover refusals and uncertain writes keep the ordinary classifier.
pub(super) fn advisory_action(
    context: &NativeMailContext,
    failure: &OperationFailure,
) -> Option<Action> {
    if matches!(
        failure.cause,
        OperationCause::Subscription(SubscriptionClosed::LocalShutdown)
            | OperationCause::Connection(ConnectionFailure::InvalidEnvelope)
            | OperationCause::Transport(TransportFailure::MalformedEnvelope)
    ) && failure.effect == WriteEffect::None
        && context.owns_runtime()
        && context.binding.is_some()
        && context.blocked_revision.is_none()
        && context.credential.refusal().is_none()
        && context.require_current().is_ok()
    {
        Some(Action::Reconnect)
    } else {
        action(&RemoteMailboxError::Port(failure.clone()))
    }
}

/// A delivery-contract refusal or an unreadable reply can clear from the source
/// side (for example a producer rule change or a restart), so it gets a slow
/// retry rather than a terminal park. Typed authentication and takeover are
/// neither and stay terminal.
pub(super) fn slow(error: &RemoteMailboxError) -> bool {
    matches!(error, RemoteMailboxError::Port(failure)
    if matches!(failure.cause,
        OperationCause::Delivery(_)
            | OperationCause::Connection(ConnectionFailure::InvalidEnvelope)
            | OperationCause::Transport(
                TransportFailure::MalformedEnvelope | TransportFailure::MessageBodyExceeded
            )))
}

/// Exact retries on one binding before the next failure reopens it.
pub(super) const EXACT_RETRIES: usize = 5;

/// Ten minutes between slow retries; tests use a short interval.
pub(super) const SLOW_RETRY_MS: u64 = if cfg!(test) { 300 } else { 600_000 };

/// Project disagreement is terminal identity failure, not transient availability.
pub(super) fn project_mismatch(error: &RemoteMailboxError) -> bool {
    matches!(error, RemoteMailboxError::ForeignBinding)
        || matches!(error, RemoteMailboxError::Port(failure)
            if matches!(failure.cause, OperationCause::Open(grapt_mail_transport::OpenFailure::ForeignProjectMismatch)))
}

/// The credential fact an outcome carries, from closed causes only, never
/// from diagnostic text. Everything else is a transport fact.
pub(super) fn credential_event(error: &RemoteMailboxError) -> CredentialEvent {
    use grapt_mail_transport::OpenFailure;
    if project_mismatch(error) {
        return CredentialEvent::ProjectMismatch;
    }
    match error {
        RemoteMailboxError::Port(failure) => match failure.cause {
            OperationCause::Open(OpenFailure::UnauthenticatedRetry) => {
                CredentialEvent::FloorChanged
            }
            OperationCause::Open(OpenFailure::Unauthenticated) => CredentialEvent::Unauthenticated,
            _ => CredentialEvent::Transport,
        },
        _ => CredentialEvent::Transport,
    }
}

/// One owned timer survives cancellation of the service select. Each drive
/// makes one bounded attempt; slow recovery continues for the Session lifetime.
#[derive(Default)]
pub(super) struct Recovery {
    attempt: usize,
    /// Consecutive exact retries on one binding, since the last reopen or
    /// healthy snapshot.
    exact: usize,
    sequence: u64,
    wait: Option<Pin<Box<tokio::time::Sleep>>>,
    healthy: Option<Pin<Box<tokio::time::Sleep>>>,
}

/// A cancelled service select must leave an explicit outcome for its attempt.
pub(super) struct RetryAttempt {
    session: SessionId,
    number: u64,
    outcome: &'static str,
}

impl RetryAttempt {
    pub(super) fn finish(mut self, outcome: &'static str) {
        self.outcome = outcome;
    }
}

impl Drop for RetryAttempt {
    fn drop(&mut self) {
        crate::diagnostics::report(format!(
            "native mail retry session={} lifetime_sequence={} outcome={}",
            self.session, self.number, self.outcome
        ));
    }
}

impl Recovery {
    #[cfg(test)]
    pub(super) fn attempts(&self) -> u64 {
        self.sequence
    }

    pub(super) fn begin(&mut self, session: &SessionId) -> Option<RetryAttempt> {
        if !self.recovering() {
            return None;
        }
        self.sequence = self.sequence.saturating_add(1);
        crate::diagnostics::report(format!(
            "native mail retry session={session} lifetime_sequence={} outcome=started",
            self.sequence
        ));
        Some(RetryAttempt {
            session: session.clone(),
            number: self.sequence,
            outcome: "cancelled",
        })
    }

    fn delay_ms(&self, session: &SessionId) -> u64 {
        let delays = [250, 1_000, 5_000, 10_000, 20_000, 40_000, 60_000];
        let base = delays[self.attempt.min(delays.len() - 1)];
        if self.attempt < 3 {
            return base;
        }
        // Session IDs already come from the injected identity authority. Hash
        // only to stagger reconnects; this is neither entropy nor authority.
        let seed = session.as_str().bytes().fold(self.sequence, |value, byte| {
            value.wrapping_mul(16777619) ^ u64::from(byte)
        });
        base - seed % (base / 10 + 1)
    }

    pub(super) fn defer(&mut self, session: &SessionId) {
        if self.wait.is_none() {
            // This timer never drives work itself. Check its injected Tokio
            // deadline only when another failure needs a retry decision.
            if self.healthy.as_mut().is_some_and(|timer| {
                timer
                    .as_mut()
                    .poll(&mut Context::from_waker(std::task::Waker::noop()))
                    .is_ready()
            }) {
                self.attempt = 0;
            }
            self.healthy = None;
            let delay = self.delay_ms(session);
            self.attempt = self.attempt.saturating_add(1).min(6);
            crate::diagnostics::report(format!(
                "native mail retry session={session} delay_ms={delay}"
            ));
            self.wait = Some(Box::pin(tokio::time::sleep(Duration::from_millis(delay))));
        }
    }

    /// One fixed, slow wait on the same owned timer. It neither consumes nor
    /// replenishes the fast-retry budget.
    pub(super) fn defer_slow(&mut self, session: &SessionId) {
        if self.wait.is_none() {
            crate::diagnostics::report(format!(
                "native mail slow retry session={session} delay_ms={SLOW_RETRY_MS}"
            ));
            self.wait = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                SLOW_RETRY_MS,
            ))));
        }
    }

    pub(super) fn waiting(&self) -> bool {
        self.wait.is_some()
    }

    pub(super) fn recovering(&self) -> bool {
        self.attempt > 0 && self.healthy.is_none()
    }

    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(wait) = &mut self.wait {
            if wait.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.wait = None;
        }
        Poll::Ready(())
    }

    /// An exact read retry keeps the binding, but not forever: the
    /// EXACT_RETRIES-th in a row reopens instead, so a binding the door has
    /// silently lost cannot keep the Session deaf. An uncertain write, or any
    /// retry while delivery work is pending, keeps its exact coordinates and
    /// is never escalated.
    pub(super) fn escalate(&mut self, action: Action, write_pending: bool) -> Action {
        if action == Action::ExactWrite || (action == Action::Exact && write_pending) {
            return action;
        }
        if action == Action::Exact {
            self.exact += 1;
            if self.exact < EXACT_RETRIES {
                return Action::Exact;
            }
        }
        self.exact = 0;
        Action::Reconnect
    }

    pub(super) fn restored(&mut self) {
        self.exact = 0;
        self.wait = None;
        // A snapshot can be live for only a few milliseconds while a door
        // flaps. Replenish fast retries only after a full healthy minute.
        if self.attempt > 0 && self.healthy.is_none() {
            self.healthy = Some(Box::pin(tokio::time::sleep(Duration::from_secs(60))));
        }
    }

    pub(super) fn reset(&mut self) {
        self.exact = 0;
        self.wait = None;
        self.healthy = None;
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_read_retries_reopen_after_their_cap_but_writes_never_do() {
        let mut recovery = Recovery::default();
        for _ in 1..EXACT_RETRIES {
            assert_eq!(recovery.escalate(Action::Exact, false), Action::Exact);
        }
        assert_eq!(
            recovery.escalate(Action::Exact, false),
            Action::Reconnect,
            "a silently lost binding kept being retried exactly"
        );
        assert_eq!(
            recovery.escalate(Action::Exact, false),
            Action::Exact,
            "a reopen restarts the count"
        );
        recovery.restored();
        for _ in 0..3 * EXACT_RETRIES {
            assert_eq!(
                recovery.escalate(Action::ExactWrite, false),
                Action::ExactWrite,
                "an uncertain write lost its exact coordinates"
            );
            assert_eq!(
                recovery.escalate(Action::Exact, true),
                Action::Exact,
                "a retry with delivery pending lost its binding"
            );
        }
        assert_eq!(
            recovery.escalate(Action::Reconnect, false),
            Action::Reconnect
        );
        assert_eq!(recovery.escalate(Action::Exact, false), Action::Exact);
    }

    #[test]
    fn only_known_failures_retry_and_authority_or_corruption_never_does() {
        for effect in [WriteEffect::None, WriteEffect::UnknownRetryExact] {
            let error = RemoteMailboxError::Port(OperationFailure {
                cause: OperationCause::Delivery(DeliveryAckFailure::MessageNeverEnumerated),
                effect,
                source_state: None,
                partial_dismiss: None,
            });
            assert_eq!(
                action(&error),
                (effect == WriteEffect::UnknownRetryExact).then_some(Action::ExactWrite)
            );
        }
        for cause in [
            OperationCause::Connection(ConnectionFailure::Deadline),
            OperationCause::Transport(TransportFailure::TransportUnavailable),
            OperationCause::Transport(TransportFailure::SubscriptionRefused),
            OperationCause::Lifecycle(LifecycleOutcome::ConnectBudgetExhausted),
            OperationCause::Lifecycle(LifecycleOutcome::ReconnectBudgetExhausted),
            OperationCause::Lifecycle(LifecycleOutcome::SliceBudgetExhausted {
                catch_up_owed: true,
            }),
        ] {
            for effect in [WriteEffect::None, WriteEffect::UnknownRetryExact] {
                let error = RemoteMailboxError::Port(OperationFailure {
                    cause: cause.clone(),
                    effect,
                    source_state: None,
                    partial_dismiss: None,
                });
                assert_eq!(
                    action(&error),
                    Some(if effect == WriteEffect::None {
                        Action::Exact
                    } else {
                        Action::ExactWrite
                    })
                );
            }
        }
        for cause in [
            OperationCause::Open(grapt_mail_transport::OpenFailure::Unauthenticated),
            OperationCause::Open(grapt_mail_transport::OpenFailure::ExactPrincipalMismatch),
            OperationCause::Subscription(SubscriptionClosed::LocalShutdown),
            OperationCause::Connection(ConnectionFailure::InvalidEnvelope),
            OperationCause::Delivery(DeliveryAckFailure::AcknowledgementDigestMismatch),
            OperationCause::Lifecycle(LifecycleOutcome::Cancelled),
            OperationCause::Lifecycle(LifecycleOutcome::RecoveryUnavailable),
            OperationCause::Lifecycle(LifecycleOutcome::ShutdownBudgetExhausted {
                tasks_remaining: 1,
            }),
        ] {
            let error = RemoteMailboxError::Port(OperationFailure {
                cause,
                effect: WriteEffect::UnknownRetryExact,
                source_state: None,
                partial_dismiss: None,
            });
            assert_eq!(action(&error), None);
        }
        assert_eq!(action(&RemoteMailboxError::Cancelled), None);
        assert_eq!(action(&RemoteMailboxError::ForeignBinding), None);
        assert_eq!(action(&RemoteMailboxError::ForeignReceipt), None);
    }

    #[test]
    fn expired_observation_reconnects_but_an_uncertain_operation_keeps_its_coordinates() {
        for cause in [
            OperationCause::Transport(TransportFailure::SnapshotExpired),
            OperationCause::Transport(TransportFailure::Disconnected),
            OperationCause::Subscription(SubscriptionClosed::Disconnected),
        ] {
            for effect in [WriteEffect::None, WriteEffect::UnknownRetryExact] {
                let error = RemoteMailboxError::Port(OperationFailure {
                    cause: cause.clone(),
                    effect,
                    source_state: None,
                    partial_dismiss: None,
                });
                assert_eq!(
                    action(&error),
                    Some(if effect == WriteEffect::None {
                        Action::Reconnect
                    } else {
                        Action::ExactWrite
                    })
                );
            }
        }
    }

    #[test]
    fn takeover_never_retries_into_a_new_owner() -> Result<(), Box<dyn std::error::Error>> {
        let error = RemoteMailboxError::Port(OperationFailure {
            cause: OperationCause::Subscription(SubscriptionClosed::DisplacedByTakeover {
                by_principal: SourcePrincipalId::try_new("other-seat")?,
                by_client: MailTransportClientInstanceId::try_new("other-client")?,
            }),
            effect: WriteEffect::UnknownRetryExact,
            source_state: None,
            partial_dismiss: None,
        });
        assert_eq!(action(&error), None);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn brief_live_snapshots_do_not_restart_the_fast_retry_budget(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let session: SessionId = "ses_flapping_timer".parse()?;
        let mut retry = Recovery::default();
        for ceiling in [250, 1_000, 5_000, 10_000, 20_000, 40_000, 60_000] {
            retry.defer(&session);
            tokio::time::advance(Duration::from_millis(ceiling)).await;
            std::future::poll_fn(|cx| retry.poll(cx)).await;
            retry.restored();
            // The door opens but drops again before it has stayed healthy.
            tokio::time::advance(Duration::from_secs(9)).await;
            assert!(
                retry.delay_ms(&session) >= (ceiling * 9 / 10).min(54_000),
                "brief recovery replenished the fast retry budget"
            );
        }
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_service_slices_retain_the_timer_and_persistent_failures_are_rate_limited(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let session: SessionId = "ses_retry_timer".parse()?;
        let mut retry = Recovery::default();
        for ceiling in [
            250, 1_000, 5_000, 10_000, 20_000, 40_000, 60_000, 60_000, 60_000,
        ] {
            let delay = retry.delay_ms(&session);
            assert!((ceiling * 9 / 10..=ceiling).contains(&delay));
            retry.defer(&session);
            // A cancelled select must neither consume nor restart the timer.
            std::future::poll_fn(|cx| {
                assert!(retry.poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            tokio::time::advance(Duration::from_millis(delay - 1)).await;
            retry.defer(&session);
            std::future::poll_fn(|cx| {
                assert!(retry.poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            tokio::time::advance(Duration::from_millis(1)).await;
            std::future::poll_fn(|cx| retry.poll(cx)).await;
            assert!(!retry.waiting());
        }
        retry.restored();
        assert!(!retry.recovering());
        tokio::time::advance(Duration::from_secs(60)).await;
        retry.defer(&session);
        assert_eq!(
            retry.attempt, 1,
            "stable recovery did not replenish fast retries"
        );
        retry.reset();
        assert!(!retry.waiting() && !retry.recovering());
        Ok(())
    }
}

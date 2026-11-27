//! Await only after store guards have been released. Millisecond ticks are the
//! released loopback adapter's deadline unit; no ambient clock is read here.
use super::*;
use grapt_mail_transport::operation::{ConnectionFailure, OperationCause};
use grapt_mail_transport::{SubscriptionClosed, TransportFailure};
use std::future::Future;
use std::time::Duration;

pub(super) fn check_cancel(
    cancel: &watch::Receiver<Option<InterruptionSource>>,
) -> Result<(), RemoteMailboxError> {
    if cancel.borrow().is_some() {
        Err(RemoteMailboxError::Cancelled)
    } else {
        Ok(())
    }
}
async fn cancelled(mut signal: watch::Receiver<Option<InterruptionSource>>) {
    loop {
        if signal.borrow().is_some() {
            return;
        }
        if signal.changed().await.is_err() {
            return;
        }
    }
}
pub(super) async fn operation<T>(
    cancel: &watch::Receiver<Option<InterruptionSource>>,
    ticks: u32,
    call: impl Future<Output = Result<T, OperationFailure>>,
) -> Result<T, RemoteMailboxError> {
    check_cancel(cancel)?;
    tokio::select! {
        biased;
        () = cancelled(cancel.clone()) => Err(RemoteMailboxError::Cancelled),
        reply = tokio::time::timeout(Duration::from_millis(u64::from(ticks)), call) => {
            reply.map_err(|_| RemoteMailboxError::Deadline)?.map_err(RemoteMailboxError::Port)
        }
    }
}
pub(super) async fn pause(
    cancel: &watch::Receiver<Option<InterruptionSource>>,
    ticks: u32,
) -> Result<(), RemoteMailboxError> {
    check_cancel(cancel)?;
    tokio::select! {
        biased;
        () = cancelled(cancel.clone()) => Err(RemoteMailboxError::Cancelled),
        () = tokio::time::sleep(Duration::from_millis(u64::from(ticks))) => Ok(()),
    }
}
pub(super) fn retryable(error: &RemoteMailboxError) -> bool {
    matches!(
        error,
        RemoteMailboxError::Deadline
            | RemoteMailboxError::Port(OperationFailure {
                cause: OperationCause::Connection(
                    ConnectionFailure::Unavailable | ConnectionFailure::Deadline
                ) | OperationCause::Transport(
                    TransportFailure::TransportUnavailable
                        | TransportFailure::Disconnected
                        | TransportFailure::SnapshotExpired
                ) | OperationCause::Subscription(SubscriptionClosed::Disconnected),
                ..
            })
    )
}
impl<P: RemoteSeatMailTransportV1Port> RemoteMailboxRuntime<P> {
    pub(super) async fn ensure_open(
        &mut self,
        cancel: &watch::Receiver<Option<InterruptionSource>>,
    ) -> Result<(), RemoteMailboxError> {
        if let Some(binding) = &self.binding {
            let receipt = binding.receipt();
            if receipt.projects() != &self.expected.projects
                || receipt.verified_principal() != &self.expected.principal
                || receipt.local_recipient() != &self.expected.recipient
            {
                return Err(RemoteMailboxError::ForeignBinding);
            }
            binding
                .verify_expectations(&self.request)
                .map_err(|failure| RemoteMailboxError::Port(failure.into()))?;
            return Ok(());
        }
        let limits = self.requested_limits;
        let mut last = None;
        for _ in 0..limits.max_connect_attempts_per_activation().get() {
            match operation(
                cancel,
                limits.max_connect_duration_ticks().get(),
                self.port.open(self.request.clone(), limits),
            )
            .await
            {
                Ok(binding) => {
                    let validation = binding.verify_expectations(&self.request);
                    let receipt = binding.receipt();
                    let matches = receipt.projects() == &self.expected.projects
                        && receipt.verified_principal() == &self.expected.principal
                        && receipt.local_recipient() == &self.expected.recipient;
                    self.binding = Some(binding);
                    validation.map_err(|failure| RemoteMailboxError::Port(failure.into()))?;
                    if !matches {
                        // Keep the handle available for explicit bounded close;
                        // it never reaches a store call, snapshot or ACK.
                        return Err(RemoteMailboxError::ForeignBinding);
                    }
                    return Ok(());
                }
                Err(error) if retryable(&error) => last = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last.ok_or(RemoteMailboxError::NotLive)?)
    }
}

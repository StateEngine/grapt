//! Proof that a drain's spawned work is *gone*, not merely told to stop.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Cancellation ownership", whose shutdown sequence ends "join every task
//! with a bounded supervised shutdown policy" and "release handles only after
//! terminal commit" — and which then says plainly: *"If the process dies, Rust
//! does not pretend `Drop` completed this work."*
//!
//! # The gap this closes
//!
//! An owning wake can be aborted outright. That drops the drain's futures, and
//! every frame unwinds synchronously — but a `tokio` task is not a frame.
//! `JoinHandle::abort` is a *request*; the task's future is destroyed later, by
//! the runtime, at a moment no `Drop` can await. So at the instant the
//! coordinator's ownership guard runs, the drain's provider reader may still be
//! live.
//!
//! A coordinator that frees the Session there admits a replacement drain
//! alongside the old attempt's reader. Ordering it by hand in a witness — wait
//! for the reader to die, *then* start the replacement — proves nothing, since
//! the witness supplied the ordering the code was supposed to guarantee.
//!
//! # How it closes it
//!
//! [`Residency`] is a token cloned into every task an attempt owns: the
//! provider reader, and each local tool. A clone dies exactly when its task's
//! future is destroyed. The coordinator holds the other end, so "every token is
//! gone" is evidence of *destruction*, not of a request having been issued, and
//! the Session stays unclaimable until then.
//!
//! Tokens ride with the cancellation signal in one [`Cancellation`] value
//! rather than as a second parameter: a seam that can cancel a task is exactly
//! the seam that owes proof the task ended.

use std::sync::Arc;

use grapt_types::InterruptionSource;
use tokio::sync::watch;

/// A live claim on one drain's Session ownership.
///
/// Held by the spawned work itself. Cheap to clone, and carries no data — the
/// only fact it encodes is "the task holding me still exists". Never store one
/// outside the task whose life it is meant to measure; a token parked in a
/// long-lived structure would report that work as forever unfinished.
#[derive(Clone, Debug)]
pub struct Residency(#[allow(dead_code)] watch::Receiver<()>);

/// The coordinator's end of one drain's residency.
///
/// A `watch` sender is used for its receiver count, not to send: the channel
/// carries `()` and nothing is ever published on it. What matters is that
/// [`watch::Sender::closed`] resolves precisely when the last
/// [`Residency`] is dropped.
#[derive(Debug)]
pub struct DrainResidency(watch::Sender<()>);

impl DrainResidency {
    #[must_use]
    pub fn new() -> Arc<Self> {
        let (sender, _) = watch::channel(());
        Arc::new(Self(sender))
    }

    /// Mint a token for a task about to be spawned.
    #[must_use]
    pub fn token(&self) -> Residency {
        Residency(self.0.subscribe())
    }

    /// Whether every token is gone. Synchronous, for a decision taken under the
    /// coordinator's slot lock — where nothing may await.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.receiver_count() == 0
    }

    /// Wait until every token is gone.
    ///
    /// Deliberately unbounded, and not a new hang risk. A token is held only by
    /// a task the supervisor owns and has already aborted, and the one way such
    /// a task can outlive its abort is by never yielding — which would equally
    /// wedge the supervisor's ordinary join loop, a pre-existing property of
    /// that path rather than something introduced here. A bound belongs with
    /// the supervised shutdown policy the contract names, as a ruled duration,
    /// not as a number invented at this seam.
    pub async fn closed(&self) {
        self.0.closed().await;
    }
}

/// One Step's cancellation signal, together with the residency every task that
/// Step spawns must hold.
///
/// The two travel as one value on purpose. They were previously a lone
/// `watch::Receiver<bool>`, and the seam that can *ask* a task to stop is
/// exactly the seam that owes evidence it did — keeping them together means a
/// future spawn site cannot take the power without inheriting the obligation.
///
/// R2A widened the payload from `bool` to `Option<InterruptionSource>` so the
/// signal carries WHO asked. That is a change to the signal's type, not to the
/// pairing rule above, which predates it and is why the sentence describing the
/// lone receiver is written in the past tense about `bool`.
#[derive(Clone, Debug)]
pub struct Cancellation {
    cancel: watch::Receiver<Option<InterruptionSource>>,
    residency: Residency,
}

impl Cancellation {
    #[must_use]
    pub fn new(cancel: watch::Receiver<Option<InterruptionSource>>, residency: Residency) -> Self {
        Self { cancel, residency }
    }

    /// The interrupt signal, cloned for one watcher.
    #[must_use]
    pub fn signal(&self) -> watch::Receiver<Option<InterruptionSource>> {
        self.cancel.clone()
    }

    /// A token for one task about to be spawned.
    #[must_use]
    pub fn residency(&self) -> Residency {
        self.residency.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn residency_closes_only_when_every_token_is_gone() {
        let residency = DrainResidency::new();
        assert!(residency.is_closed(), "no tokens minted yet");

        let first = residency.token();
        let second = residency.token();
        assert!(!residency.is_closed());

        drop(first);
        assert!(!residency.is_closed(), "one token still outstanding");

        drop(second);
        assert!(residency.is_closed());
        // Already closed: this must not hang.
        residency.closed().await;
    }

    /// The property the coordinator depends on: a token held by a SPAWNED task
    /// keeps residency open until that task's future is actually destroyed —
    /// which an abort request alone does not achieve.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spawned_holder_keeps_residency_open_until_it_is_destroyed() {
        let residency = DrainResidency::new();
        let token = residency.token();
        let task = tokio::spawn(async move {
            let _token = token;
            std::future::pending::<()>().await;
        });
        assert!(!residency.is_closed());

        task.abort();
        // `closed()` is what waits out the gap between the abort REQUEST and
        // the destruction that honours it.
        tokio::time::timeout(std::time::Duration::from_secs(5), residency.closed())
            .await
            .unwrap_or_else(|_| panic!("residency never closed after the holder was aborted"));
        assert!(residency.is_closed());
    }
}

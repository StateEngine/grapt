//! An optional StateEngine seat: when the user's own configuration names one,
//! committed session events become the seat's busy and idle reports and its
//! published turns. Nothing here starts, and no connection is attempted, when
//! no seat is configured.
//!
//! Turn execution never waits on StateEngine. One thread reads the event hub
//! and another owns the IPC connection, reconnecting after a refusal, a
//! disconnect or a host restart. The seat's busy or idle state is shared
//! between them, so it is never lost; turns travel on a bounded queue, and a
//! full queue drops a turn and says so, never blocking the hub. Dropping the
//! [`Seat`] stops both threads after a last idle report, waiting at most
//! [`STOP_WAIT`] for it.
use grapt_api::{DogfoodEventHub, DogfoodEventSubscription, SseError};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{
    channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError,
};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

mod config;
mod ipc;
mod publisher;
mod turns;

use config::SeatConfig;
use publisher::{Command, Desired, Publisher};
use turns::{Report, Tracker};

/// Commands waiting for the publisher; more are dropped and reported.
const QUEUE: usize = 64;
/// The longest a stopping service waits for the seat's last report.
const STOP_WAIT: Duration = Duration::from_secs(1);
/// Waits before subscribing again while the hub has no subscriber room.
const FIRST_RESUBSCRIBE: Duration = Duration::from_millis(100);
const LAST_RESUBSCRIBE: Duration = Duration::from_secs(30);

/// The running seat. Dropping it stops reporting.
pub(crate) struct Seat {
    stop: Arc<Stop>,
    threads: Vec<JoinHandle<()>>,
    /// Disconnects once every thread has ended: each holds a sender.
    ended: Receiver<()>,
}

/// Start the seat when the user's configuration under `service_root` names one.
pub(crate) fn install(hub: &Arc<DogfoodEventHub>, service_root: &Path) -> Option<Seat> {
    let config = match config::read(service_root) {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(reason) => {
            warn("config-unusable", &reason);
            return None;
        }
    };
    let desired = Arc::new(Desired::default());
    let (commands, received) = sync_channel(QUEUE);
    let (ending, ended) = channel();
    let publisher = spawn_publisher(config, Arc::clone(&desired), received, ending.clone());
    let publisher = match publisher {
        Ok(publisher) => publisher,
        Err(error) => {
            warn("publisher-unavailable", &error.to_string());
            return None;
        }
    };
    let stop = Arc::new(Stop::default());
    let (hub, observer_stop) = (Arc::clone(hub), Arc::clone(&stop));
    let observer = std::thread::Builder::new()
        .name("grapt-stateengine-seat-events".into())
        .spawn(move || {
            let _ending = ending;
            observe(&hub, &observer_stop, &desired, &commands);
        });
    let mut threads = vec![publisher];
    match observer {
        Ok(observer) => threads.push(observer),
        Err(error) => warn("observer-unavailable", &error.to_string()),
    }
    Some(Seat {
        stop,
        threads,
        ended,
    })
}

impl Drop for Seat {
    fn drop(&mut self) {
        self.stop.stop();
        match self.ended.recv_timeout(STOP_WAIT) {
            Err(RecvTimeoutError::Disconnected) => {
                for thread in self.threads.drain(..) {
                    if thread.join().is_err() {
                        warn("thread-panicked", "a seat thread ended abnormally");
                    }
                }
            }
            // A host that does not answer cannot hold the service: the
            // threads are left to end on their own.
            Ok(()) | Err(RecvTimeoutError::Timeout) => {
                warn("stop-timeout", "the seat's last report did not finish");
            }
        }
    }
}

/// Whether the seat is stopping, and the subscription to end when it does.
/// No lock is held while the hub subscribes or hydrates, so stopping never
/// waits on the hub.
#[derive(Default)]
struct Stop {
    stopped: AtomicBool,
    current: Mutex<Option<Arc<DogfoodEventSubscription>>>,
    /// Held only while waiting for subscriber room.
    pause: Mutex<()>,
    changed: Condvar,
}

impl Stop {
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(subscription) = self.current().take() {
            subscription.cancel();
        }
        let _paused = self.pause.lock().unwrap_or_else(PoisonError::into_inner);
        self.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn current(&self) -> MutexGuard<'_, Option<Arc<DogfoodEventSubscription>>> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A new subscription, or `None` once the seat is stopping. A hub with
    /// no subscriber room is asked again, later each time.
    fn subscribe(&self, hub: &DogfoodEventHub) -> Option<Arc<DogfoodEventSubscription>> {
        let mut wait = FIRST_RESUBSCRIBE;
        while !self.stopped() {
            match hub.subscribe_host() {
                Ok(subscription) => {
                    let subscription = Arc::new(subscription);
                    let mut current = self.current();
                    // Checked under the lock `stop` takes after setting it.
                    if self.stopped() {
                        subscription.cancel();
                        return None;
                    }
                    *current = Some(Arc::clone(&subscription));
                    return Some(subscription);
                }
                Err(SseError::SubscriberBound) => {
                    if wait == FIRST_RESUBSCRIBE {
                        warn(
                            "subscription-full",
                            "the event hub has no subscriber room yet",
                        );
                    }
                    self.pause(wait);
                    wait = (wait * 2).min(LAST_RESUBSCRIBE);
                }
                Err(error) => {
                    warn("subscription-unavailable", &error.to_string());
                    return None;
                }
            }
        }
        None
    }

    /// Wait up to `wait`, or until the seat stops.
    fn pause(&self, wait: Duration) {
        let paused = self.pause.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.stopped() {
            let _woken = self.changed.wait_timeout(paused, wait);
        }
    }
}

fn spawn_publisher(
    config: SeatConfig,
    desired: Arc<Desired>,
    received: Receiver<Command>,
    ending: Sender<()>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("grapt-stateengine-seat".into())
        .spawn(move || {
            let _ending = ending;
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => return warn("runtime-unavailable", &error.to_string()),
            };
            let mut publisher = Publisher::new(ipc::IpcSeatHost::new(config), desired);
            loop {
                let command = match publisher.retry_in() {
                    None => received.recv().map_err(|_| RecvTimeoutError::Disconnected),
                    Some(wait) => received.recv_timeout(wait),
                };
                match command {
                    Ok(command) => runtime.block_on(publisher.apply(command)),
                    Err(RecvTimeoutError::Timeout) => {
                        runtime.block_on(publisher.apply(Command::Reconcile));
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            runtime.block_on(publisher.finish());
            // A stalled endpoint read must not hold this thread's exit.
            runtime.shutdown_background();
        })
}

/// Feed committed events to the publisher until the seat stops. A hub
/// reconciliation ends a subscription in the ordinary course, and so does an
/// overflow; the next subscription restates which sessions are busy before
/// its first live frame, and only then is the seat's state decided again.
fn observe(hub: &DogfoodEventHub, stop: &Stop, desired: &Desired, commands: &SyncSender<Command>) {
    let mut dropped = false;
    while let Some(subscription) = stop.subscribe(hub) {
        let mut tracker = Tracker::default();
        // `server.connected`, then the frames that restate current state.
        let mut restating = subscription.hydration_frames() + 1;
        loop {
            let frame = match subscription.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    warn("subscription-lost", &error.to_string());
                    break;
                }
            };
            let report = tracker.observe(&frame);
            if restating > 0 {
                restating -= 1;
                if restating == 0 {
                    desired.set(tracker.activity());
                    let _ = commands.try_send(Command::Reconcile);
                }
                continue;
            }
            let command = match report {
                Some(Report::Activity(state)) => {
                    desired.set(state);
                    Command::Reconcile
                }
                Some(Report::Turn(turn)) => Command::Turn(turn),
                None => continue,
            };
            match commands.try_send(command) {
                Ok(()) => dropped = false,
                // A full queue already holds work, so the publisher still
                // wakes and reconciles; only a turn is lost.
                Err(TrySendError::Full(Command::Turn(_))) if !dropped => {
                    dropped = true;
                    warn("queue-full", "a seat turn was dropped");
                }
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
    // Stopping: the service runs no more turns.
    desired.set(grapt_stateengine_ipc::Activity::Idle);
}

fn warn(reason: &str, detail: &str) {
    eprintln!("stateengine seat warning reason={reason} detail={detail}");
}

#[cfg(test)]
mod tests;

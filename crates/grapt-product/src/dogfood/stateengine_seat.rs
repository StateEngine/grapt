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
//! [`Seat`] stops both threads after a last idle report.
use grapt_api::{DogfoodEventHub, DogfoodEventSubscription};
use std::path::Path;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

mod config;
mod ipc;
mod publisher;
mod turns;

use config::SeatConfig;
use publisher::{Command, Desired, Publisher};
use turns::{Report, Tracker};

/// Commands waiting for the publisher; more are dropped and reported.
const QUEUE: usize = 64;

/// The running seat. Dropping it stops reporting and waits for both threads.
pub(crate) struct Seat {
    stop: Arc<Stop>,
    threads: Vec<JoinHandle<()>>,
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
    let publisher = match spawn_publisher(config, Arc::clone(&desired), received) {
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
        .spawn(move || observe(&hub, &observer_stop, &desired, &commands));
    let mut threads = vec![publisher];
    match observer {
        Ok(observer) => threads.push(observer),
        Err(error) => warn("observer-unavailable", &error.to_string()),
    }
    Some(Seat { stop, threads })
}

impl Drop for Seat {
    fn drop(&mut self) {
        self.stop.stop();
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                warn("thread-panicked", "a seat thread ended abnormally");
            }
        }
    }
}

/// Whether the seat is stopping, and the subscription to end when it does.
#[derive(Default)]
struct Stop(Mutex<StopState>);

#[derive(Default)]
struct StopState {
    stopped: bool,
    current: Option<Arc<DogfoodEventSubscription>>,
}

impl Stop {
    fn stop(&self) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.stopped = true;
        if let Some(subscription) = state.current.take() {
            subscription.cancel();
        }
    }

    /// A new subscription, or `None` once the seat is stopping.
    fn subscribe(&self, hub: &DogfoodEventHub) -> Option<Arc<DogfoodEventSubscription>> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stopped {
            return None;
        }
        match hub.subscribe_host() {
            Ok(subscription) => {
                let subscription = Arc::new(subscription);
                state.current = Some(Arc::clone(&subscription));
                Some(subscription)
            }
            Err(error) => {
                warn("subscription-unavailable", &error.to_string());
                None
            }
        }
    }
}

fn spawn_publisher(
    config: SeatConfig,
    desired: Arc<Desired>,
    received: Receiver<Command>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("grapt-stateengine-seat".into())
        .spawn(move || {
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
        })
}

/// Feed committed events to the publisher until the seat stops. A hub
/// reconciliation ends a subscription in the ordinary course; so does an
/// overflow. Either way the seat counts as idle until the next
/// subscription's hydration restates which sessions are busy.
fn observe(hub: &DogfoodEventHub, stop: &Stop, desired: &Desired, commands: &SyncSender<Command>) {
    let mut dropped = false;
    while let Some(subscription) = stop.subscribe(hub) {
        let mut tracker = Tracker::default();
        loop {
            let frame = match subscription.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    warn("subscription-lost", &error.to_string());
                    break;
                }
            };
            let command = match tracker.observe(&frame) {
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
        if tracker.release() {
            desired.set(grapt_stateengine_ipc::Activity::Idle);
            let _ = commands.try_send(Command::Reconcile);
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

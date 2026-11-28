//! An optional StateEngine seat: when the user's own configuration names one,
//! committed session events become the seat's busy and idle reports and its
//! published turns. Nothing here starts, and no connection is attempted, when
//! no seat is configured.
//!
//! Turn execution never waits on StateEngine. One thread reads the event hub
//! and hands bounded commands to another, which owns the IPC connection and
//! reconnects after a refusal, a disconnect or a host restart. A full queue
//! drops a command and says so; it never blocks the hub.
use grapt_api::DogfoodEventHub;
use std::path::Path;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

mod config;
mod ipc;
mod publisher;
mod turns;

use config::SeatConfig;
use publisher::{Command, Publisher};
use turns::Tracker;

/// Commands waiting for the publisher; more are dropped and reported.
const QUEUE: usize = 64;

/// Start the seat when the user's configuration under `service_root` names one.
pub(crate) fn install(hub: &Arc<DogfoodEventHub>, service_root: &Path) {
    let config = match config::read(service_root) {
        Ok(Some(config)) => config,
        Ok(None) => return,
        Err(reason) => return warn("config-unusable", &reason),
    };
    let (commands, received) = sync_channel(QUEUE);
    if let Err(error) = spawn_publisher(config, received) {
        return warn("publisher-unavailable", &error.to_string());
    }
    let hub = Arc::clone(hub);
    let observer = std::thread::Builder::new()
        .name("grapt-stateengine-seat-events".into())
        .spawn(move || observe(&hub, &commands));
    if let Err(error) = observer {
        warn("observer-unavailable", &error.to_string());
    }
}

fn spawn_publisher(config: SeatConfig, received: Receiver<Command>) -> std::io::Result<()> {
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
            let mut publisher = Publisher::new(ipc::IpcSeatHost::new(config));
            for command in received {
                runtime.block_on(publisher.apply(command));
            }
        })
        .map(drop)
}

/// Feed committed events to the publisher until the hub closes. An overflowed
/// subscription is replaced: the seat reports idle, then the new
/// subscription's hydration restates which sessions are busy.
fn observe(hub: &DogfoodEventHub, commands: &SyncSender<Command>) {
    let mut dropped = false;
    loop {
        let subscription = match hub.subscribe_host() {
            Ok(subscription) => subscription,
            Err(error) => return warn("subscription-unavailable", &error.to_string()),
        };
        let mut tracker = Tracker::default();
        loop {
            match subscription.next_frame() {
                Ok(Some(frame)) => {
                    for command in tracker.observe(&frame) {
                        match commands.try_send(command) {
                            Ok(()) => dropped = false,
                            Err(TrySendError::Full(_)) if !dropped => {
                                dropped = true;
                                warn("queue-full", "a seat report was dropped");
                            }
                            Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Disconnected(_)) => return,
                        }
                    }
                }
                Ok(None) => {
                    if let Some(idle) = tracker.release() {
                        let _ = commands.try_send(idle);
                    }
                    return;
                }
                Err(error) => {
                    warn("subscription-lost", &error.to_string());
                    if let Some(idle) = tracker.release() {
                        let _ = commands.try_send(idle);
                    }
                    break;
                }
            }
        }
    }
}

fn warn(reason: &str, detail: &str) {
    eprintln!("stateengine seat warning reason={reason} detail={detail}");
}

#[cfg(test)]
mod tests;

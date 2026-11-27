//! The periodic reads (permissions, the session catalog and forms) run on
//! their own worker and connection, so a slow answer never holds the protocol
//! loop: the loop prepares each read, keeps pumping frames and publishing
//! views while it runs, and applies the answer when it arrives.
//!
//! Admission is bounded by construction: one request slot and one answer slot
//! per kind, so at most one read of each kind is in flight. A hint that
//! arrives meanwhile is held by its poll and starts exactly one more read
//! after this one settles.
use super::{catalog::CatalogRefresh, forms};
use crate::bridge::{Bridge, ReadPeer};
use grapt_client::{
    Client, ClientError, PeriodicRead, PreparedRead, TransportError, WireRequest, WireResponse,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::SyncSender,
    Arc, Condvar, Mutex, MutexGuard, PoisonError,
};
use std::time::Duration;
use tokio::time::Instant;

const KINDS: [PeriodicRead; 4] = [
    PeriodicRead::Permissions,
    PeriodicRead::Sessions,
    PeriodicRead::SessionForms,
    PeriodicRead::LocationForms,
];

fn slot(read: PeriodicRead) -> usize {
    match read {
        PeriodicRead::Permissions => 0,
        PeriodicRead::Sessions => 1,
        PeriodicRead::SessionForms => 2,
        PeriodicRead::LocationForms => 3,
    }
}

/// The worker's side: a request and an answer slot per kind.
#[derive(Default)]
struct Slots {
    requests: [Option<(ReadPeer, WireRequest)>; 4],
    answers: [Option<Result<WireResponse, TransportError>>; 4],
    closed: bool,
}

type Shared = Arc<(Mutex<Slots>, Condvar)>;

/// What a read was issued against: its prepared request and scope, and the
/// attachment it was sent on. An answer is applied only while both hold.
struct Ticket {
    prepared: PreparedRead,
    attachment: Arc<AtomicBool>,
}

pub(super) struct Reads {
    shared: Shared,
    in_flight: [Option<Ticket>; 4],
}

impl std::fmt::Debug for Reads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let in_flight: Vec<_> = KINDS.iter().filter(|read| self.busy(**read)).collect();
        f.debug_struct("Reads")
            .field("in_flight", &in_flight)
            .finish()
    }
}

fn lock(shared: &Shared) -> MutexGuard<'_, Slots> {
    shared.0.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Reads {
    pub(super) fn start(wake: SyncSender<()>) -> Result<Self, String> {
        let shared = Shared::default();
        let worker = Arc::clone(&shared);
        let _worker = std::thread::Builder::new()
            .name("tui-r-reads".into())
            .spawn(move || work(&worker, &wake))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            shared,
            in_flight: Default::default(),
        })
    }

    pub(super) fn busy(&self, read: PeriodicRead) -> bool {
        self.in_flight[slot(read)].is_some()
    }

    /// Start `read` on the worker. `Ok(true)` when there was nothing to ask
    /// and the client already applied that.
    pub(super) fn begin(
        &mut self,
        client: &mut Client<Bridge>,
        read: PeriodicRead,
    ) -> Result<bool, ClientError> {
        if self.busy(read) {
            return Ok(false);
        }
        let Some(prepared) = client.prepare_read(read)? else {
            return Ok(true);
        };
        let peer = client.peer_mut().read_peer()?;
        let attachment = peer.attachment();
        let mut slots = lock(&self.shared);
        if slots.closed {
            return Err(TransportError::Unavailable {
                reason: "the read worker stopped".into(),
            }
            .into());
        }
        slots.requests[slot(read)] = Some((peer, prepared.request.clone()));
        drop(slots);
        self.shared.1.notify_one();
        self.in_flight[slot(read)] = Some(Ticket {
            prepared,
            attachment,
        });
        Ok(false)
    }

    /// Start the form reads that are due. A read with nothing to ask settles
    /// at once.
    pub(super) fn begin_forms(
        &mut self,
        client: &mut Client<Bridge>,
        poll: &mut forms::Poll,
        now: Instant,
    ) -> Result<bool, ClientError> {
        let (mut changed, mut failure) = (false, None);
        for read in poll.due(client, now, |read| self.busy(read)) {
            let started = self.begin(client, read);
            if !matches!(started, Ok(false)) {
                match poll.settled(read, started, now) {
                    Ok(settled) => changed |= settled,
                    Err(error) => failure = failure.or(Some(error)),
                }
            }
        }
        failure.map_or(Ok(changed), Err)
    }

    /// Apply every answer that has arrived and hand each outcome to the poll
    /// that owns its retries. An answer from an attachment that has since been
    /// replaced is discarded and asked again. Returns whether to publish, and
    /// a status line.
    pub(super) fn settle(
        &mut self,
        client: &mut Client<Bridge>,
        polls: (&mut PermissionPoll, &mut CatalogRefresh, &mut forms::Poll),
        now: Instant,
        wake: &SyncSender<()>,
    ) -> (bool, Option<String>) {
        let (mut publish, mut status) = (false, None);
        for read in KINDS {
            let Some(response) = lock(&self.shared).answers[slot(read)].take() else {
                continue;
            };
            let Some(ticket) = self.in_flight[slot(read)].take() else {
                continue;
            };
            let bridge = client.peer_mut();
            bridge.read_health.call(&ticket.prepared.request, &response);
            let applied = if bridge.attachment_current(&ticket.attachment) {
                client.accept_read(ticket.prepared, response)
            } else {
                Ok(false)
            };
            let outcome = match read {
                PeriodicRead::Permissions => polls.0.settled(client, applied, now),
                PeriodicRead::Sessions => polls.1.settled(client, applied, now, wake),
                PeriodicRead::SessionForms | PeriodicRead::LocationForms => {
                    polls.2.settled(read, applied, now)
                }
            };
            match outcome {
                Ok(changed) => publish |= changed,
                Err(error) => {
                    publish = true;
                    if !client.peer_mut().read_health.transient(&error) {
                        status = Some(match read {
                            PeriodicRead::SessionForms | PeriodicRead::LocationForms => {
                                format!("Questions unavailable; last drafts kept: {error}")
                            }
                            _ => error.to_string(),
                        });
                    }
                }
            }
        }
        (publish, status)
    }
}

impl Drop for Reads {
    fn drop(&mut self) {
        lock(&self.shared).closed = true;
        self.shared.1.notify_all();
    }
}

/// The worker: take the next filled request slot, send it, and leave the
/// answer in that kind's answer slot.
fn work(shared: &Shared, wake: &SyncSender<()>) {
    loop {
        let (index, (peer, request)) = {
            let mut slots = lock(shared);
            loop {
                if slots.closed {
                    return;
                }
                let next = slots
                    .requests
                    .iter_mut()
                    .enumerate()
                    .find_map(|(index, held)| held.take().map(|job| (index, job)));
                if let Some(next) = next {
                    break next;
                }
                slots = shared.1.wait(slots).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let answer = peer.call(&request);
        lock(shared).answers[index] = Some(answer);
        let _ = wake.try_send(());
    }
}

/// The open permission asks, re-read when the stream hints at a change or a
/// transient failure's retry falls due.
#[derive(Debug, Default)]
pub(super) struct PermissionPoll {
    retry: Option<Instant>,
}

impl PermissionPoll {
    pub(super) fn wait(&self, now: Instant) -> Option<Duration> {
        self.retry.map(|at| at.saturating_duration_since(now))
    }

    pub(super) fn refresh(
        &mut self,
        client: &mut Client<Bridge>,
        reads: &mut Reads,
        now: Instant,
    ) -> Result<bool, ClientError> {
        // A pending resume, or a read already running, leaves the hint raised.
        if client.resume_pending()
            || reads.busy(PeriodicRead::Permissions)
            || !(client
                .peer_mut()
                .authority_dirty
                .swap(false, Ordering::AcqRel)
                || self.retry.is_some_and(|at| at <= now))
        {
            return Ok(false);
        }
        self.retry = None;
        let started = reads.begin(client, PeriodicRead::Permissions);
        self.settled(client, started.map(|_| true), now)
    }

    fn settled(
        &mut self,
        client: &mut Client<Bridge>,
        result: Result<bool, ClientError>,
        now: Instant,
    ) -> Result<bool, ClientError> {
        match result {
            Ok(true) => Ok(true),
            // Discarded for its scope: ask again at once.
            Ok(false) => {
                self.retry = Some(now);
                Ok(false)
            }
            Err(error) if client.peer_mut().read_health.transient(&error) => {
                self.retry = Some(now + Duration::from_secs(1));
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }
}

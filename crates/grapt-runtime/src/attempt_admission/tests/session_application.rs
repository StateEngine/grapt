use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Clock, EventIds};
use grapt_inbox::{AdmitRequest, Inbox};
use grapt_store::{AggregateId, AppendInput};
use grapt_types::{DeliveryMode, EventId, EventVersion, MessageId, SessionId};
use serde_json::json;
use tokio::sync::Semaphore;

const DEADLINE: Duration = Duration::from_secs(10);
const CREATED_MS: i64 = 1_786_060_800_000;

// ---------------------------------------------------------------------------
// Identities and small fixtures
// ---------------------------------------------------------------------------

fn session(id: &str) -> SessionId {
    match format!("ses_{id}").parse::<SessionId>() {
        Ok(session) => session,
        Err(error) => panic!("session id must parse: {error}"),
    }
}

fn message(id: &str) -> MessageId {
    match format!("msg_{id}").parse::<MessageId>() {
        Ok(message) => message,
        Err(error) => panic!("message id must parse: {error}"),
    }
}

fn version_one() -> EventVersion {
    match EventVersion::new(1) {
        Some(version) => version,
        None => panic!("event version 1 must exist"),
    }
}

/// Monotonic, deterministic event ids. Infallible, as the promotion boundary
/// requires.
#[derive(Debug)]
struct TestIds {
    next: AtomicU64,
    prefix: &'static str,
}

impl Default for TestIds {
    fn default() -> Self {
        Self {
            next: AtomicU64::new(0),
            prefix: "mint",
        }
    }
}

impl EventIds for TestIds {
    fn mint(&self) -> EventId {
        let ordinal = self.next.fetch_add(1, Ordering::SeqCst);
        match format!("evt_{}{ordinal}", self.prefix).parse() {
            Ok(id) => id,
            Err(error) => panic!("minted id must parse: {error}"),
        }
    }
}

impl TestIds {
    fn named(prefix: &'static str) -> Self {
        Self {
            next: AtomicU64::new(0),
            prefix,
        }
    }

    fn minted(&self) -> u64 {
        self.next.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        CREATED_MS
    }
}

#[derive(Debug, Default)]
struct CountingClock {
    calls: AtomicUsize,
}

impl Clock for CountingClock {
    fn now_ms(&self) -> i64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        CREATED_MS
    }
}

impl CountingClock {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// A one-directional rendezvous that cannot lose a signal.
#[derive(Debug, Clone)]
struct Gate {
    reached: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

impl Gate {
    fn new() -> Self {
        Self {
            reached: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }

    async fn arrive(&self) {
        self.reached.add_permits(1);
        match self.release.acquire().await {
            Ok(permit) => permit.forget(),
            Err(_) => panic!("gate release closed"),
        }
    }

    async fn wait(&self) {
        match tokio::time::timeout(DEADLINE, self.reached.acquire()).await {
            Ok(Ok(permit)) => permit.forget(),
            Ok(Err(_)) => panic!("gate closed"),
            Err(_) => panic!("gate was never reached within the deadline"),
        }
    }

    fn open(&self) {
        self.release.add_permits(1);
    }
}

async fn within<F: std::future::Future>(what: &str, future: F) -> F::Output {
    match tokio::time::timeout(DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not complete within the deadline"),
    }
}

/// Wait for a condition that a task in ANOTHER runtime slot will make true.
///
/// A dropped task's future is dropped by the runtime, not by whoever aborted
/// it, so "the reader is gone" is only ever observable a moment later. This is
/// the one place the witnesses cannot use a rendezvous: the very thing being
/// proved is the ABSENCE of a live participant, and something absent cannot
/// reach a gate. It is still a bounded assertion — the deadline fails the test
/// rather than hanging it.
async fn settles(what: &str, done: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} did not settle within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Holds a reader's destruction open.
///
/// The abandoned-drain race is real but narrow: an abort lands in microseconds
/// while a replacement drain spends far longer on its own store work, so a
/// witness that merely starts the replacement quickly observes the safe
/// interleaving no matter what the coordinator does — which is exactly what
/// made an earlier version of that witness pass with the defect in place.
///
/// This removes the timing from the question. Destruction is stopped partway
/// and held there, and the witness asks what the coordinator permits WHILE
/// cleanup is in flight. The invariant under test has to hold for arbitrary
/// cleanup latency, so making that latency controlled is not stacking the deck
/// — it is the only way to state the invariant at all.
///
/// Synchronous by necessity: `Drop` cannot await, which is the whole reason the
/// coordinator needs residency in the first place.
/// One-shot: it holds the FIRST destruction only. Later readers — the
/// replacement's, and whatever the witness tears down at the end — are not the
/// subject of the question and must not be made to wait on a release nobody is
/// going to send.
#[derive(Debug)]
struct CloseGate {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    used: std::sync::atomic::AtomicBool,
}

/// The witness's end of a [`CloseGate`].
struct CloseControl {
    entered: std::sync::mpsc::Receiver<()>,
    release: std::sync::mpsc::SyncSender<()>,
}

fn close_gate() -> (Arc<CloseGate>, CloseControl) {
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(8);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(8);
    (
        Arc::new(CloseGate {
            entered: entered_tx,
            release: Mutex::new(release_rx),
            used: std::sync::atomic::AtomicBool::new(false),
        }),
        CloseControl {
            entered: entered_rx,
            release: release_tx,
        },
    )
}

impl CloseGate {
    /// Called from `Drop`. Announces that destruction has begun and blocks
    /// until the witness lets it finish.
    fn hold(&self) {
        if self.used.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.entered.send(()).is_err() {
            return;
        }
        let release = locked(&self.release);
        let _ = release.recv_timeout(DEADLINE);
    }
}

impl CloseControl {
    fn wait_entered(&self) {
        match self.entered.recv_timeout(DEADLINE) {
            Ok(()) => {}
            Err(error) => panic!("a reader never began being destroyed: {error}"),
        }
    }

    fn release(&self) {
        let _ = self.release.send(());
    }
}

/// A census of provider readers: how many exist now, how many ever did, and
/// the most that ever existed at once.
///
/// The reader is the object whose lifetime IS the attempt's, so counting its
/// constructions and drops lets a witness assert "no reader is live" and
/// "never two at once" from the outside — without asking the supervisor, which
/// is the thing under test.
#[derive(Debug, Default)]
struct ReaderCensus {
    live: AtomicUsize,
    peak: AtomicUsize,
    created: AtomicUsize,
    order: Mutex<Vec<String>>,
}

impl ReaderCensus {
    fn opened(&self) {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        let ordinal = self.created.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        locked(&self.order).push(format!("open({ordinal})"));
    }

    fn closed(&self) {
        let live = self.live.fetch_sub(1, Ordering::SeqCst);
        locked(&self.order).push(format!("close({live})"));
    }

    /// The open/close sequence, in the order it actually happened. `peak` says
    /// two readers never coexisted; this says which ended before which began,
    /// which is the claim a replacement drain has to satisfy.
    fn order(&self) -> Vec<String> {
        locked(&self.order).clone()
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn created(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }
}

fn locked<T>(guard: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match guard.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// ---------------------------------------------------------------------------
// The real store: seeding, admitting, and reading the transcript back
// ---------------------------------------------------------------------------

/// Seed a Session durably at a directory, exactly as `session.create` does.
fn seed(inbox: &Arc<Mutex<Inbox>>, ses: &SessionId, directory: &str) {
    let mut guard = locked(inbox);
    let event: EventId = match format!("evt_seed{}", ses.as_str()).parse() {
        Ok(id) => id,
        Err(error) => panic!("seed event id must parse: {error}"),
    };
    let data = json!({
        "sessionID": ses.as_str(),
        "info": { "id": ses.as_str(), "directory": directory },
    });
    // Test setup elects explicitly; the append path never does.
    let aggregate = AggregateId::Session(ses.clone());
    let writer = match guard
        .store_mut()
        .elect_writer(&aggregate)
        .and_then(|_| guard.store().fencing_token(&aggregate))
    {
        Ok(writer) => writer,
        Err(error) => panic!("seeding must be able to elect a writer: {error}"),
    };
    let appended = guard.store_mut().append(
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.created",
            version: version_one(),
            created_ms: CREATED_MS,
            data: &data,
        },
        &mut [],
    );
    if let Err(error) = appended {
        panic!("seeding must succeed: {error}");
    }
}

/// Durable admission through the real inbox.
fn admit(inbox: &Arc<Mutex<Inbox>>, ses: &SessionId, id: &str, delivery: DeliveryMode) {
    let mut guard = locked(inbox);
    let event: EventId = match format!("evt_adm{id}").parse() {
        Ok(id) => id,
        Err(error) => panic!("admission event id must parse: {error}"),
    };
    let input = message(id);
    let outcome = guard.admit(AdmitRequest {
        event_id: &event,
        session: ses,
        input: &input,
        text: id,
        delivery,
        resume: true,
        created_ms: CREATED_MS,
    });
    if let Err(error) = outcome {
        panic!("admission must succeed: {error}");
    }
}

/// The projected transcript for a Session, in durable order.
fn transcript(inbox: &Arc<Mutex<Inbox>>, ses: &SessionId) -> Vec<String> {
    let guard = locked(inbox);
    let conn = guard.store().connection();
    let mut stmt = match conn.prepare(
        "SELECT id FROM session_message WHERE session_id = ?1 AND type = 'user' ORDER BY seq",
    ) {
        Ok(stmt) => stmt,
        Err(error) => panic!("transcript query must prepare: {error}"),
    };
    let rows = stmt.query_map([ses.as_str()], |row| row.get::<_, String>(0));
    match rows {
        Ok(rows) => rows
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| panic!("transcript rows must read: {error}")),
        Err(error) => panic!("transcript query must run: {error}"),
    }
}

/// How many durable events of one kind exist for a Session.
fn event_count(inbox: &Arc<Mutex<Inbox>>, ses: &SessionId, kind: &str) -> i64 {
    let guard = locked(inbox);
    match guard.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = ?2",
        rusqlite::params![ses.as_str(), kind],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => panic!("event count must read: {error}"),
    }
}

fn pending_count(inbox: &Arc<Mutex<Inbox>>, ses: &SessionId) -> i64 {
    let guard = locked(inbox);
    match guard.store().connection().query_row(
        "SELECT COUNT(*) FROM session_pending WHERE session_id = ?1",
        [ses.as_str()],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => panic!("pending count must read: {error}"),
    }
}

mod runtime_and_harness;

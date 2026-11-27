//! The DOGFOOD-0 vertical, end to end, through the ports an API would call.
//!
//! These are not unit tests of the port functions. Each one drives the whole
//! capability against a real on-disk store — create, prompt admission, a Step's
//! durable content, then the reads — and asserts what a caller sees. The
//! on-disk store matters: `restart` here means CLOSING the store and reopening
//! the file, which is the only form of the word that proves anything.

use super::*;
use crate::durable_owner::{DurableAttemptOwner, NoLiveSubscribers};
use crate::owner::{AttemptOwner, DurableFact};
use grapt_artifact::FilesystemArtifactStore;
use grapt_provider::{AdapterState, AttemptFragment, BlockOrdinal, FinishReason};
use grapt_session::{
    ParentFilter, SessionListCriteria, SessionListDirection, SessionListLimits, SessionListOrder,
    SessionListScope, StepTerminal,
};
use grapt_store::EventStore;
use grapt_types::{
    ClockAuthority, ContentKind, ContentOrdinal, IdAuthority, PhysicalAttemptId, ProjectId,
    StepNumber, Timestamp,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
struct FrozenClock;

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_784_090_756_114)
    }
}

#[derive(Debug, Default)]
struct CountingIds {
    next: AtomicU64,
}

impl IdAuthority for CountingIds {
    fn mint(&self) -> grapt_types::IdToken {
        let value = self.next.fetch_add(1, Ordering::SeqCst);
        match grapt_types::IdToken::new(format!("{value:012x}")) {
            Some(token) => token,
            None => panic!("a hex counter renders non-empty"),
        }
    }
}

/// A scratch root holding both the database FILE and the artifact store, so a
/// "restart" can reopen the same bytes.
#[derive(Debug)]
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "grapt-df0-session-ports-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        if let Err(error) = std::fs::create_dir_all(&path) {
            panic!("scratch root must exist: {error}");
        }
        Self(path)
    }

    fn db(&self) -> PathBuf {
        self.0.join("events.sqlite")
    }

    fn artifacts(&self) -> FilesystemArtifactStore {
        match FilesystemArtifactStore::open(
            self.0.join("artifacts"),
            b"df0-ports-key".to_vec(),
            Box::new(FrozenClock),
        ) {
            Ok(store) => store,
            Err(error) => panic!("scratch artifact store must open: {error}"),
        }
    }

    /// Opens the store on disk and migrates it, so the session-list projection
    /// table exists. Called again after a "restart" to reopen the same file.
    fn inbox(&self) -> Arc<Mutex<Inbox>> {
        let backups = self.0.join("backups");
        if let Err(error) = std::fs::create_dir_all(&backups) {
            panic!("the backup directory must exist: {error}");
        }
        let database = self.db();
        // Platform first-contact makes absence an explicit initialization
        // decision; migration therefore opens only an already-existing file.
        // This test seam owns that initialization without treating any other
        // filesystem failure as absence, and restart reuses the same bytes.
        match std::fs::File::create_new(&database) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("the database file must initialize: {error}"),
        }
        if let Err(error) = EventStore::migrate(&database, &backups, "df0-test") {
            panic!("the store must migrate: {error}");
        }
        let store = match EventStore::open(&database, "df0-test") {
            Ok(store) => store,
            Err(error) => panic!("the store must open: {error}"),
        };
        match Inbox::new(store) {
            Ok(inbox) => Arc::new(Mutex::new(inbox)),
            Err(error) => panic!("the inbox must open: {error}"),
        }
    }

    /// The list criteria these witnesses use: everything, newest first, one
    /// bounded page. Spelled out rather than defaulted because
    /// `SessionListCriteria` deliberately has NO `Default` — a page size and a
    /// budget are decisions, not values a caller can forget to make.
    fn criteria(&self) -> SessionListCriteria {
        // The PRODUCTION limits, not numbers invented here: a witness that
        // paged under bounds no shipped caller uses would prove the query works
        // under conditions the product never creates.
        let limits = SessionListLimits::production();
        match SessionListCriteria::new(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            SessionListOrder::Descending,
            SessionListDirection::Next,
            None,
            25,
            limits,
        ) {
            Ok(criteria) => criteria,
            Err(error) => panic!("the witness criteria must validate: {error}"),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn project() -> ProjectId {
    match ProjectId::new("global".to_owned()) {
        Some(project) => project,
        None => panic!("global is a non-empty project identity"),
    }
}

fn seed() -> SessionSeed {
    match SessionSeed::new(project(), "/work/df0", "dogfood zero") {
        Ok(seed) => seed,
        Err(error) => panic!("the seed must validate: {error}"),
    }
}

fn state() -> AdapterState {
    AdapterState::none()
}

fn fragment(fragment: AttemptFragment) -> DurableFact {
    DurableFact::Fragment {
        fragment: Box::new(fragment),
    }
}

/// Runs one Step's worth of durable content against the shared store.
fn run_step(
    inbox: &Arc<Mutex<Inbox>>,
    scratch: &Scratch,
    clock: &FrozenClock,
    ids: &CountingIds,
    session: &SessionId,
    text: &str,
) {
    let store = {
        // The owner needs the STORE, and the inbox owns it. Rather than open a
        // second connection — which would be a second view of somebody else's
        // transaction boundary — the witness reaches the same store through a
        // dedicated handle for the duration of the Step.
        let guard = match inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let path = guard.store().connection().path().map(PathBuf::from);
        drop(guard);
        match path {
            Some(path) => path,
            None => panic!("the witness store is on disk"),
        }
    };
    let opened = match EventStore::open(&store, "df0-test") {
        Ok(store) => Arc::new(Mutex::new(store)),
        Err(error) => panic!("the owner's store handle must open: {error}"),
    };
    let attempt = match PhysicalAttemptId::new("pat_1") {
        Some(attempt) => attempt,
        None => panic!("pat_1 is a non-empty attempt identity"),
    };
    let step = match StepNumber::new(1) {
        Some(step) => step,
        None => panic!("1 is a positive step number"),
    };
    let mut owner = match DurableAttemptOwner::elect(
        opened,
        scratch.artifacts(),
        clock,
        ids,
        NoLiveSubscribers,
        session.clone(),
        step,
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    let facts = vec![
        DurableFact::AttemptStarted { attempt },
        fragment(AttemptFragment::TextOpened {
            block: BlockOrdinal::new(0),
            state: state(),
        }),
        fragment(AttemptFragment::TextDelta {
            block: BlockOrdinal::new(0),
            text: text.to_owned(),
            state: state(),
        }),
        fragment(AttemptFragment::TextClosed {
            block: BlockOrdinal::new(0),
            state: state(),
        }),
        DurableFact::StepEnded {
            finish: FinishReason::Stop,
        },
    ];
    for fact in facts {
        if let Err(error) = owner.commit(fact) {
            panic!("the owner must accept this fact: {error}");
        }
    }
}

/// THE SUCCESS CUT for the whole vertical: create, prompt, one Step, and all
/// four reads agree.
#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
fn the_vertical_creates_admits_settles_and_reads_back() {
    let scratch = Scratch::new("vertical");
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let inbox = scratch.inbox();
    let ports = SessionPorts::new(Arc::clone(&inbox), &authorities);

    let created = match ports.create(&seed()) {
        Ok(created) => created,
        Err(error) => panic!("create must succeed: {error}"),
    };
    // The seed is the aggregate's FIRST fact.
    assert_eq!(created.seq, 0);

    // GET reads the durable record, not a cache.
    let record = match ports.get(&created.session) {
        Ok(record) => record,
        Err(error) => panic!("get must succeed: {error}"),
    };
    assert_eq!(record.session, created.session);
    assert_eq!(record.title, "dogfood zero");
    assert_eq!(record.placement.directory(), "/work/df0");
    assert_eq!(record.created_ms, created.created_ms);
    assert_eq!(
        record.agent, None,
        "no agent was selected, so none is reported"
    );

    // LIST sees it too, because create projected in the same transaction.
    let page = match ports.list(&scratch.criteria()) {
        Ok(page) => page,
        Err(error) => panic!("list must succeed: {error}"),
    };
    assert_eq!(page.items().len(), 1);
    assert_eq!(page.items()[0].key.session_id(), &created.session);

    // PROMPT admits durably and advises a wake; it starts no drain.
    let admitted = match ports.prompt(&created.session, "do the thing", DeliveryMode::Steer, true) {
        Ok(outcome) => outcome,
        Err(error) => panic!("prompt must succeed: {error}"),
    };
    assert!(matches!(admitted, AdmitOutcome::Admitted { .. }));

    // HISTORY is empty until a Step produces content — an admitted prompt is
    // not an assistant message.
    match ports.history(&created.session) {
        Ok(messages) => assert!(messages.is_empty()),
        Err(error) => panic!("history must succeed: {error}"),
    }

    run_step(&inbox, &scratch, &clock, &ids, &created.session, "answered");

    let messages = match ports.history(&created.session) {
        Ok(messages) => messages,
        Err(error) => panic!("history must succeed: {error}"),
    };
    assert_eq!(messages.len(), 1);
    let content = match &messages[0].content {
        Some(content) => content,
        None => panic!("the Step produced content"),
    };
    assert_eq!(content.len(), 1);
    assert_eq!(content.items()[0].item.ordinal(), ContentOrdinal::FIRST);
    assert_eq!(content.items()[0].item.kind(), &ContentKind::Text);
    assert_eq!(content.items()[0].item.payload().length(), 8);
    assert!(matches!(
        messages[0].terminal,
        Some(StepTerminal::Ended { .. })
    ));
}

/// RESTART / RELIST EQUALITY. The store is closed and the FILE reopened. Every
/// read returns exactly what it returned before, and the reopened process
/// opens no provider stream to produce it.
#[test]
fn every_read_survives_closing_and_reopening_the_store_file() {
    let scratch = Scratch::new("restart");
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);

    let (session, before_record, before_page, before_history) = {
        let inbox = scratch.inbox();
        let ports = SessionPorts::new(Arc::clone(&inbox), &authorities);
        let created = match ports.create(&seed()) {
            Ok(created) => created,
            Err(error) => panic!("create must succeed: {error}"),
        };
        if let Err(error) = ports.prompt(&created.session, "before", DeliveryMode::Queue, false) {
            panic!("prompt must succeed: {error}");
        }
        run_step(&inbox, &scratch, &clock, &ids, &created.session, "durable");
        let record = match ports.get(&created.session) {
            Ok(record) => record,
            Err(error) => panic!("get must succeed: {error}"),
        };
        let page = match ports.list(&scratch.criteria()) {
            Ok(page) => page,
            Err(error) => panic!("list must succeed: {error}"),
        };
        let history = match ports.history(&created.session) {
            Ok(history) => history,
            Err(error) => panic!("history must succeed: {error}"),
        };
        (created.session, record, page, history)
    };
    // The inbox — and with it the store — is dropped here. The next line opens
    // the same file from scratch.

    let reopened = scratch.inbox();
    let ports = SessionPorts::new(Arc::clone(&reopened), &authorities);

    let after_record = match ports.get(&session) {
        Ok(record) => record,
        Err(error) => panic!("get must succeed after restart: {error}"),
    };
    assert_eq!(after_record, before_record);

    let after_page = match ports.list(&scratch.criteria()) {
        Ok(page) => page,
        Err(error) => panic!("list must succeed after restart: {error}"),
    };
    assert_eq!(after_page.items().len(), before_page.items().len());
    assert_eq!(
        after_page.items()[0].key.session_id(),
        before_page.items()[0].key.session_id()
    );
    assert_eq!(after_page.items()[0].title, before_page.items()[0].title);

    let after_history = match ports.history(&session) {
        Ok(history) => history,
        Err(error) => panic!("history must succeed after restart: {error}"),
    };
    assert_eq!(after_history, before_history);

    // ZERO TERMINAL RESURRECTION: reading the history again after a restart
    // did not add a second terminal, and the one that is there is still the
    // one the Step wrote.
    assert_eq!(after_history.len(), 1);
    assert!(matches!(
        after_history[0].terminal,
        Some(StepTerminal::Ended { .. })
    ));
}

/// The refusal cut for `get`: an unknown Session is UNKNOWN, never an empty
/// record. A caller must be able to tell "no such Session" from "a Session
/// with nothing in it".
#[test]
fn getting_an_unknown_session_refuses_rather_than_returning_an_empty_record() {
    let scratch = Scratch::new("unknown");
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let inbox = scratch.inbox();
    let ports = SessionPorts::new(inbox, &authorities);
    let absent: SessionId = match "ses_absent".parse() {
        Ok(session) => session,
        Err(error) => panic!("ses_absent parses: {error}"),
    };
    match ports.get(&absent) {
        Err(SessionPortError::Read {
            source: grapt_session::SessionStoreError::Unknown { .. },
        }) => {}
        other => panic!("expected an unknown-session refusal, got {other:?}"),
    }
}

/// The permit/refusal cut for `create`: a seed the list projector would reject
/// is refused BEFORE the transaction, naming the field — not as an opaque
/// projector string, and not after a partial write.
#[test]
fn a_seed_the_projection_would_reject_is_refused_by_field_before_any_write() {
    let refusal = SessionSeed::new(project(), "", "titled");
    match refusal {
        Err(SessionCreateError::Seed { field, .. }) => assert_eq!(field, "directory"),
        other => panic!("expected a typed directory refusal, got {other:?}"),
    }
    // And an empty title is refused on its own field rather than defaulted.
    match SessionSeed::new(project(), "/work/df0", "") {
        Err(SessionCreateError::Seed { field, .. }) => assert_eq!(field, "title"),
        other => panic!("expected a typed title refusal, got {other:?}"),
    }
}

/// An exact prompt retry RECONCILES rather than admitting a second copy — but
/// only when the caller replays the same identity. These ports mint a fresh
/// identity per call, so two calls with the same text are two admissions, and
/// this test pins that so a future change to identity handling is visible.
#[test]
fn two_prompts_with_the_same_text_are_two_admissions() {
    let scratch = Scratch::new("prompt-twice");
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let inbox = scratch.inbox();
    let ports = SessionPorts::new(inbox, &authorities);
    let created = match ports.create(&seed()) {
        Ok(created) => created,
        Err(error) => panic!("create must succeed: {error}"),
    };
    let first = ports.prompt(&created.session, "same", DeliveryMode::Steer, false);
    let second = ports.prompt(&created.session, "same", DeliveryMode::Steer, false);
    match (first, second) {
        (Ok(AdmitOutcome::Admitted { seq: a, .. }), Ok(AdmitOutcome::Admitted { seq: b, .. })) => {
            assert_ne!(a, b, "two admissions occupy two sequences")
        }
        other => panic!("both prompts must admit, got {other:?}"),
    }
}

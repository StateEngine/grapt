//! R2C5A1: the attempt receipt from a consumer, cut at every point the process
//! can actually die.
//!
//! These are DURABILITY witnesses, so every one of them uses a file-backed
//! store that is CLOSED and REOPENED between the write and the observation. An
//! in-memory store would prove the write path returned what it said it did; it
//! would prove nothing about what a new process finds, which is the only
//! question this lane exists to answer.
//!
//! The six cuts named in the preflight:
//!
//! 1. after `reserve`, before the request was sent — a replayable loss;
//! 2. after the request was sent, before any settlement — the ambiguity window;
//! 3. after the terminal fact commits, before the atomic delete — a leaked
//!    post-terminal row, which reclaim must remove idempotently;
//! 4. mid-reclaim, after the read-back and before the delete — covered by the
//!    rollback-consistency witness in the module's own suite, because the cut is
//!    a transaction boundary rather than a process boundary;
//! 5. two processes, one holding a stale epoch — the stale one refuses;
//! 6. a wrong-shaped `attempt_receipt` namesake at open — refused by the column
//!    check, in `write.rs`'s own suite where the other namesake refusals live.
//!
//! Every identity and instant is supplied explicitly. Nothing here reads an
//! ambient clock or mints an id.

use grapt_store::{
    AggregateId, AttemptReceiptReclaimed, AttemptReceiptReserved, AttemptRecordCeiling,
    AttemptSettlement, EventStore, ReserveInput, TerminalCoordinates, WriteError,
};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, LostEffect,
    MarkerOutcome, PhysicalAttemptId, ReceiptDisposition, RequestSentMarker, ReservedMarker,
    SessionId, SettledMarker, StepNumber, Timestamp,
};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SESSION: &str = "ses_033602c50001";
const ATTEMPT: &str = "att_033602c50001";
const ENGINE: &str = "0.0.1";
const RESERVED_MS: i64 = 1_786_147_200_000;
const SENT_MS: i64 = 1_786_147_200_120;
const SETTLED_MS: i64 = 1_786_147_200_900;
const TERMINAL: &str = "evt_033602c5t001";
const TERMINAL_MS: i64 = 1_786_147_200_800;

/// The exact Step terminal this attempt settles into, captured at reserve.
fn terminal() -> TerminalCoordinates {
    match TERMINAL.parse() {
        Ok(event) => TerminalCoordinates::new(event, TERMINAL_MS),
        Err(_) => panic!("the fixture event identity parses"),
    }
}

/// A FIXTURE ceiling, wide enough that no witness here refuses on size. It is
/// not a production value and names none.
fn ceiling() -> AttemptRecordCeiling {
    match NonZeroU32::new(4_096) {
        Some(value) => AttemptRecordCeiling::new(value),
        None => panic!("a non-zero fixture ceiling"),
    }
}

fn session() -> SessionId {
    match SESSION.parse() {
        Ok(session) => session,
        Err(_) => panic!("the fixture session identity parses"),
    }
}

fn attempt_id() -> PhysicalAttemptId {
    match PhysicalAttemptId::new(ATTEMPT) {
        Some(id) => id,
        None => panic!("a non-empty fixture attempt id"),
    }
}

fn receipt() -> DurableAttemptReceipt {
    let bound = match NonZeroU32::new(4_096) {
        Some(bound) => bound,
        None => panic!("a non-zero fixture bound"),
    };
    let attempt = match BoundedAttemptId::new(attempt_id(), bound) {
        Ok(attempt) => attempt,
        Err(error) => panic!("the fixture attempt id fits: {error:?}"),
    };
    let bounded_session = match BoundedSessionId::new(session(), bound) {
        Ok(session) => session,
        Err(error) => panic!("the fixture session id fits: {error:?}"),
    };
    match (StepNumber::new(3), AttemptOrdinal::new(1)) {
        (Some(step), Some(ordinal)) => DurableAttemptReceipt::reserved(
            attempt,
            bounded_session,
            step,
            ordinal,
            ReservedMarker::new(Timestamp::from_millis(RESERVED_MS)),
        ),
        _ => panic!("the fixture step coordinates are valid"),
    }
}

/// A private directory per witness, so two tests cannot share a database file.
struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!(
            "grapt-attempt-receipt-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Workspace { dir })
    }

    fn db(&self) -> PathBuf {
        self.dir.join("store.db")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Opens the store a NEW process would open, and elects this instance.
fn reopen_elected(db: &Path) -> Result<EventStore, Box<dyn std::error::Error>> {
    let mut store = EventStore::open(db, ENGINE)?;
    store.elect_writer(&AggregateId::Session(session()))?;
    Ok(store)
}

fn reserve(store: &mut EventStore) -> Result<AttemptReceiptReserved, Box<dyn std::error::Error>> {
    let receipt = receipt();
    let writer = store.fencing_token(&AggregateId::Session(session()))?;
    let terminal = terminal();
    Ok(store.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt,
        writer,
        terminal: &terminal,
        ceiling: ceiling(),
    })?)
}

fn mark_sent(store: &mut EventStore) -> Result<MarkerOutcome, Box<dyn std::error::Error>> {
    let writer = store.fencing_token(&AggregateId::Session(session()))?;
    let tx = store.governed_transaction()?;
    let outcome = EventStore::permit_attempt_send_within(
        &tx,
        &attempt_id(),
        writer,
        ceiling(),
        RequestSentMarker::new(Timestamp::from_millis(SENT_MS)),
    )?;
    tx.commit()?.discard();
    Ok(outcome)
}

/// Settles the attempt the ONLY way production can: one transaction that
/// appends the exact terminal AND fills the settled marker.
fn settle(store: &mut EventStore) -> Result<AttemptSettlement, Box<dyn std::error::Error>> {
    let writer = store.fencing_token(&AggregateId::Session(session()))?;
    let tx = store.governed_transaction()?;
    let outcome = EventStore::settle_attempt_process_loss_within(
        &tx,
        &attempt_id(),
        writer,
        ceiling(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok(outcome)
}

fn disposition(store: &EventStore) -> Result<ReceiptDisposition, Box<dyn std::error::Error>> {
    match store.attempt_receipt(&attempt_id(), ceiling())? {
        Some(record) => Ok(record.receipt().disposition()),
        None => panic!("the receipt must survive the process that wrote it"),
    }
}

/// Crash cut 1: the process died after `reserve` and before the request left.
/// A new process finds a receipt that says nothing was initiated, which is the
/// only state from which a replay is safe.
#[test]
fn a_cut_after_reserve_is_a_replayable_loss() -> TestResult {
    let workspace = Workspace::new("cut1")?;
    {
        let mut store = reopen_elected(&workspace.db())?;
        assert_eq!(reserve(&mut store)?, AttemptReceiptReserved::Reserved);
        // The process ends here. Nothing else ran.
    }
    let store = reopen_elected(&workspace.db())?;
    let effect = match disposition(&store)? {
        ReceiptDisposition::Lost(effect) => effect,
        other => panic!("an unsettled receipt must read as lost, got {other:?}"),
    };
    assert_eq!(effect, LostEffect::NoneInitiated);
    assert!(
        effect.safe_to_replay(),
        "nothing left the process, so the work may be re-run"
    );
    Ok(())
}

/// Crash cut 2, the reason this lane exists: the process died after the send
/// permit was recorded and before any settlement. This test performs no
/// provider or send boundary, so it cannot witness that bytes left — and it no
/// longer says it does. What it witnesses is the ambiguity that permit creates:
/// a new process cannot tell a request that crossed from one that did not, and
/// must never replay it automatically.
#[test]
fn a_cut_after_the_request_was_sent_is_ambiguous_and_never_replayable() -> TestResult {
    let workspace = Workspace::new("cut2")?;
    {
        let mut store = reopen_elected(&workspace.db())?;
        reserve(&mut store)?;
        assert_eq!(mark_sent(&mut store)?, MarkerOutcome::Recorded);
        // The process ends inside the ambiguity window.
    }
    let store = reopen_elected(&workspace.db())?;
    let effect = match disposition(&store)? {
        ReceiptDisposition::Lost(effect) => effect,
        other => panic!("an unsettled receipt must read as lost, got {other:?}"),
    };
    assert_eq!(
        effect,
        LostEffect::Ambiguous {
            sent_at: Timestamp::from_millis(SENT_MS)
        },
        "the surviving marker IS the evidence — the classification is derived \
         from which markers the crash left behind, not from a status somebody \
         had to remember to write"
    );
    assert!(
        !effect.safe_to_replay(),
        "the request MAY have crossed, and an unresolvable maybe is exactly what \
         may never be replayed automatically"
    );
    Ok(())
}

/// Crash cut 3: the terminal fact committed and the process died before the
/// atomic delete, leaking a post-terminal row. A later reclaim removes it, and
/// removing it TWICE is idempotent rather than an error — the second pass is
/// exactly what a crash between the delete and its own bookkeeping produces.
#[test]
fn a_leaked_post_terminal_row_is_reclaimed_idempotently() -> TestResult {
    let workspace = Workspace::new("cut3")?;
    {
        let mut store = reopen_elected(&workspace.db())?;
        reserve(&mut store)?;
        mark_sent(&mut store)?;
        // ONE call appends the exact terminal and fills the settled marker.
        assert_eq!(settle(&mut store)?, AttemptSettlement::Settled);
        // The process ends here: the terminal is durable and the receipt leaked.
    }
    {
        let store = reopen_elected(&workspace.db())?;
        assert_eq!(
            disposition(&store)?,
            ReceiptDisposition::AlreadySettled,
            "the leaked row is settled, so a reconciler must not write a second terminal"
        );
    }

    let mut store = reopen_elected(&workspace.db())?;
    let writer = store.fencing_token(&AggregateId::Session(session()))?;
    let tx = store.governed_transaction()?;
    let first = EventStore::reclaim_attempt_receipt_within(&tx, &attempt_id(), writer, ceiling())?;
    tx.commit()?.discard();
    assert_eq!(first, AttemptReceiptReclaimed::Reclaimed);

    let writer = store.fencing_token(&AggregateId::Session(session()))?;
    let tx = store.governed_transaction()?;
    let second = EventStore::reclaim_attempt_receipt_within(&tx, &attempt_id(), writer, ceiling())?;
    tx.commit()?.discard();
    assert_eq!(
        second,
        AttemptReceiptReclaimed::AlreadyAbsent,
        "an already-absent row reconciles; a pass that crashed after its delete \
         committed must be able to re-run"
    );
    Ok(())
}

/// Crash cut 5, across two real owners: a marker write is fenced TWICE, against
/// two different authorities, and the two refusals are distinct because the
/// situations are.
///
/// * against the ELECTION — an owner that lost the aggregate is refused by the
///   store's own `StaleWriter`, EVEN THOUGH the row it wants to mark agrees with
///   it: the same process wrote both, so the row alone can never catch this;
/// * against the ROW — the CURRENT owner is refused for an attempt reserved
///   under an earlier epoch, because it did not make that call and may not
///   describe it.
///
/// Dropping either check would leave a real writer able to describe work it does
/// not own. Two `EventStore` values on one file, each with its own connection,
/// its own writer identity and its own held epochs — the shape the store's own
/// election witnesses already use, and the only one that expresses a genuine
/// ownership TRANSFER: re-electing the SAME owner deliberately retains its
/// epoch rather than advancing it.
#[test]
fn a_marker_is_fenced_against_the_election_and_against_the_row() -> TestResult {
    let workspace = Workspace::new("cut5")?;
    let db = workspace.db();

    let mut first = reopen_elected(&db)?;
    reserve(&mut first)?;
    let lost = first.fencing_token(&AggregateId::Session(session()))?;

    // A second owner takes the aggregate. Election advances the epoch.
    let mut second = reopen_elected(&db)?;
    let current = second.fencing_token(&AggregateId::Session(session()))?;
    assert_ne!(
        current.epoch(),
        lost.epoch(),
        "a second OWNER's election must advance the epoch, or this witness proves nothing"
    );

    // 1. The lost owner. Its own row still records its own epoch, so only the
    //    election can refuse it — and it does, before anything is written.
    let tx = first.governed_transaction()?;
    match EventStore::permit_attempt_send_within(
        &tx,
        &attempt_id(),
        lost,
        ceiling(),
        RequestSentMarker::new(Timestamp::from_millis(SENT_MS)),
    ) {
        Err(WriteError::Election(grapt_store::ElectionError::StaleWriter {
            submitted,
            elected,
            ..
        })) => {
            assert_eq!(
                submitted,
                lost.epoch().value(),
                "the caller's epoch is stale"
            );
            assert_eq!(
                elected,
                current.epoch().value(),
                "the election has moved on"
            );
        }
        other => panic!("an owner that lost the aggregate must refuse, got {other:?}"),
    }
    drop(tx);

    // 2. The current owner. It passes the election fence and is refused by the
    //    ROW, because this attempt belongs to an earlier epoch's call.
    let tx = second.governed_transaction()?;
    match EventStore::permit_attempt_send_within(
        &tx,
        &attempt_id(),
        current,
        ceiling(),
        RequestSentMarker::new(Timestamp::from_millis(SENT_MS)),
    ) {
        Err(WriteError::ReceiptStaleEpoch { held, offered, .. }) => {
            assert_eq!(
                held,
                lost.epoch().value(),
                "the row keeps the epoch it was reserved under"
            );
            assert_eq!(offered, current.epoch().value());
        }
        other => panic!("the current owner must not mark another epoch's attempt, got {other:?}"),
    }
    drop(tx);

    // Neither refusal wrote anything, so the truth a reconciler reads is intact.
    assert_eq!(
        disposition(&second)?,
        ReceiptDisposition::Lost(LostEffect::NoneInitiated),
        "no refused write left a marker behind"
    );
    Ok(())
}

/// A RESERVE is fenced too, and that is the most consequential of the three.
///
/// Reserve is the pre-I/O admission to make an external call. An owner that has
/// lost the aggregate must not be admitted to make one: if it were, it would
/// write a receipt under a dead epoch and then send bytes that nobody's
/// reconciler is looking for.
#[test]
fn a_lost_owner_may_not_reserve_an_attempt() -> TestResult {
    let workspace = Workspace::new("reserve-fence")?;
    let db = workspace.db();

    let mut first = reopen_elected(&db)?;
    let lost = first.fencing_token(&AggregateId::Session(session()))?;

    // Ownership moves before the first owner ever reserves.
    let second = reopen_elected(&db)?;
    assert_ne!(
        second
            .fencing_token(&AggregateId::Session(session()))?
            .epoch(),
        lost.epoch()
    );

    let receipt = receipt();
    let terminal = terminal();
    match first.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt,
        writer: lost,
        terminal: &terminal,
        ceiling: ceiling(),
    }) {
        Err(WriteError::Election(grapt_store::ElectionError::StaleWriter { .. })) => {}
        other => panic!("a lost owner must not be admitted to reserve, got {other:?}"),
    }
    let rows: i64 =
        first
            .connection()
            .query_row("SELECT COUNT(*) FROM attempt_receipt", [], |r| r.get(0))?;
    assert_eq!(rows, 0, "the refused reserve wrote nothing");
    Ok(())
}

/// The reclaim epoch law is deliberately NOT the marker's, and this is the one
/// place the two differ.
///
/// A reconciler runs in a LATER process and therefore holds a NEWER epoch than
/// the receipt it is cleaning up — that is the entire situation this lane exists
/// for. Demanding equality here would make every crashed attempt permanently
/// unreclaimable, which is the defect rather than the law. What must still
/// refuse is a STALE reclaimer.
#[test]
fn a_newer_owner_may_reclaim_and_a_stale_one_may_not() -> TestResult {
    let workspace = Workspace::new("reclaim-epoch")?;
    let db = workspace.db();
    let lost;
    {
        let mut first = reopen_elected(&db)?;
        reserve(&mut first)?;
        mark_sent(&mut first)?;
        settle(&mut first)?;
        lost = first.fencing_token(&AggregateId::Session(session()))?;

        // The stale owner is refused at the election fence, before it can reach
        // the row it once owned. Ownership has already moved by the time this
        // runs, so the fence is what refuses.
        let mut second = reopen_elected(&db)?;
        let current = second.fencing_token(&AggregateId::Session(session()))?;
        assert_ne!(current.epoch(), lost.epoch());

        let tx = first.governed_transaction()?;
        match EventStore::reclaim_attempt_receipt_within(&tx, &attempt_id(), lost, ceiling()) {
            Err(WriteError::Election(grapt_store::ElectionError::StaleWriter { .. })) => {}
            other => panic!("a stale reclaimer must refuse, got {other:?}"),
        }
        drop(tx);
        assert!(
            second.attempt_receipt(&attempt_id(), ceiling())?.is_some(),
            "the refused reclaim removed nothing"
        );

        // The current owner reclaims the EARLIER epoch's leaked row, which is
        // what reconciliation is.
        let tx = second.governed_transaction()?;
        let outcome =
            EventStore::reclaim_attempt_receipt_within(&tx, &attempt_id(), current, ceiling())?;
        tx.commit()?.discard();
        assert_eq!(outcome, AttemptReceiptReclaimed::Reclaimed);
    }

    let store = EventStore::open(&db, ENGINE)?;
    assert!(
        store.attempt_receipt(&attempt_id(), ceiling())?.is_none(),
        "and the receipt is gone for every later process too"
    );
    Ok(())
}

/// PS35: settlement's OWN election fence, witnessed where it is load-bearing.
///
/// For an unsettled receipt the fence looks redundant: with it removed, a lost
/// owner's settlement still refuses, because the terminal append fences against
/// the election independently. That is defence in depth, not a second law, and
/// a witness built on it stays GREEN under the mutation — which is exactly what
/// the first version of this test did.
///
/// The case where the fence is the ONLY thing standing is an ALREADY-SETTLED
/// receipt: that path proves the existing terminal and returns
/// `AlreadySettled` WITHOUT ever appending, so no other fence is consulted.
/// Without settlement's own fence a lost owner receives a successful-looking
/// answer about work it no longer owns.
#[test]
fn a_lost_owner_may_not_settle_an_attempt() -> TestResult {
    let workspace = Workspace::new("settle-fence")?;
    let db = workspace.db();

    let mut first = reopen_elected(&db)?;
    reserve(&mut first)?;
    mark_sent(&mut first)?;
    assert_eq!(settle(&mut first)?, AttemptSettlement::Settled);
    let lost = first.fencing_token(&AggregateId::Session(session()))?;

    // Ownership moves. The receipt is already settled and its terminal durable.
    let second = reopen_elected(&db)?;
    assert_ne!(
        second
            .fencing_token(&AggregateId::Session(session()))?
            .epoch(),
        lost.epoch()
    );

    let tx = first.governed_transaction()?;
    match EventStore::settle_attempt_process_loss_within(
        &tx,
        &attempt_id(),
        lost,
        ceiling(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
        &mut [],
    ) {
        Err(WriteError::Election(grapt_store::ElectionError::StaleWriter { .. })) => {}
        other => panic!(
            "a lost owner must be refused rather than told the work it no longer              owns is already settled, got {other:?}"
        ),
    }
    drop(tx);

    let terminals: i64 = second.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.step.failed.2'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        terminals, 1,
        "still exactly the one terminal the owner wrote"
    );
    Ok(())
}

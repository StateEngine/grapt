//! F3B2: atomic authoritative projections, the durable watermark, and
//! commit-before-notify.
//!
//! These witnesses run against the PUBLIC surface, from outside the crate, so
//! everything they prove is something a consumer service can actually rely on.
//! The key's three dimensions are witnessed next to the key itself, in
//! `src/projection.rs`, because one of them (the aggregate family) is reachable
//! only through an aggregate the released append path refuses by design.

use grapt_store::{
    AggregateId, AppendInput, Appended, DecodedEvent, DurableNotice, EventStore, Notifier,
    ProjectionId, Projector, WriteError,
};
use grapt_types::{EventId, EventVersion, SessionId};
use rusqlite::Connection;
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ROSTER: ProjectionId = ProjectionId::new("roster");
const AUDIT: ProjectionId = ProjectionId::new("audit");

/// Projects one row per event into its own table, named for its projection.
struct RowProjector {
    projection: ProjectionId,
    table: &'static str,
    fail: bool,
}

impl RowProjector {
    fn roster() -> Self {
        RowProjector {
            projection: ROSTER,
            table: "roster_row",
            fail: false,
        }
    }
    fn audit() -> Self {
        RowProjector {
            projection: AUDIT,
            table: "audit_row",
            fail: false,
        }
    }
}

impl Projector for RowProjector {
    fn projection(&self) -> ProjectionId {
        self.projection
    }

    fn project(
        &mut self,
        tx: &rusqlite::Transaction<'_>,
        event: &DecodedEvent,
    ) -> Result<(), String> {
        if self.fail {
            return Err("refused on purpose".into());
        }
        tx.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {} (id TEXT PRIMARY KEY, seq INTEGER NOT NULL)",
                self.table
            ),
            [],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            &format!("INSERT INTO {} (id, seq) VALUES (?1, ?2)", self.table),
            rusqlite::params![event.id.as_str(), event.seq.value() as i64],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Records what it was told, and — crucially — what the DATABASE said at the
/// moment it was told.
#[derive(Default)]
struct Recorder {
    seen: Vec<(String, u64)>,
    dropped: u32,
}

impl Notifier for Recorder {
    fn notify(&mut self, notice: &DurableNotice) {
        self.seen
            .push((notice.id().as_str().to_owned(), notice.seq()));
    }
    fn dropped(&mut self, count: u32) {
        self.dropped += count;
    }
}

fn session(id: &str) -> Result<AggregateId, Box<dyn std::error::Error>> {
    Ok(AggregateId::Session(id.parse::<SessionId>()?))
}

fn data(marker: &str) -> Value {
    json!({ "sessionID": "ses_wm", "inputID": marker })
}

fn input<'a>(
    store: &EventStore,
    aggregate: &AggregateId,
    id: &'a EventId,
    payload: &'a Value,
) -> Result<AppendInput<'a>, Box<dyn std::error::Error>> {
    Ok(AppendInput {
        id,
        aggregate: aggregate.clone(),
        writer: store.fencing_token(aggregate)?,
        kind_name: "session.input.admitted",
        version: EventVersion::new(1).ok_or("version 1")?,
        created_ms: 1_786_060_800_000,
        data: payload,
    })
}

/// An elected in-memory store.
fn elected(id: &str) -> Result<(EventStore, AggregateId), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let aggregate = session(id)?;
    store.elect_writer(&aggregate)?;
    Ok((store, aggregate))
}

/// A file-backed store, for the witnesses that need a SECOND connection to
/// decide whether something is really durable yet.
fn on_disk(
    name: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-f3b2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("scratch.db");
    Connection::open(&db)?.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY)")?;
    Ok((dir, db))
}

fn watermark(
    store: &EventStore,
    projection: ProjectionId,
    aggregate: &AggregateId,
) -> Result<Option<u64>, WriteError> {
    Ok(store.projection_status(projection, aggregate)?.watermark())
}

#[test]
fn every_projector_advances_its_own_watermark_with_the_fact() -> TestResult {
    let (mut store, aggregate) = elected("ses_wm")?;
    let mut roster = RowProjector::roster();
    let mut audit = RowProjector::audit();

    let id: EventId = "evt_wm1".parse()?;
    let payload = data("m1");
    let (appended, notices) = store.append(
        input(&store, &aggregate, &id, &payload)?,
        &mut [&mut roster, &mut audit],
    )?;
    notices.discard();
    assert_eq!(appended, Appended::Committed { seq: 0 });

    // Both authoritative projections advanced, and to the fact's own sequence —
    // not to a count, and not to a shared position.
    assert_eq!(watermark(&store, ROSTER, &aggregate)?, Some(0));
    assert_eq!(watermark(&store, AUDIT, &aggregate)?, Some(0));

    let id2: EventId = "evt_wm2".parse()?;
    let payload2 = data("m2");
    let (appended, notices) = store.append(
        input(&store, &aggregate, &id2, &payload2)?,
        &mut [&mut roster, &mut audit],
    )?;
    notices.discard();
    assert_eq!(appended, Appended::Committed { seq: 1 });
    assert_eq!(watermark(&store, ROSTER, &aggregate)?, Some(1));

    // Caught up, and the projected rows are really there: a watermark that moved
    // without its rows would be the exact "leads its input" state the law bans.
    let status = store.projection_status(ROSTER, &aggregate)?;
    assert!(status.is_caught_up(), "{status:?}");
    let rows: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM roster_row", [], |r| r.get(0))?;
    assert_eq!(rows, 2);
    Ok(())
}

#[test]
fn a_failed_projector_leaves_no_fact_no_projected_row_and_no_watermark() -> TestResult {
    let (mut store, aggregate) = elected("ses_wm")?;
    // The first append succeeds so the watermark has somewhere to be left
    // behind: a witness starting from an empty table could not tell "rolled
    // back" from "never written".
    let id: EventId = "evt_ok".parse()?;
    let payload = data("ok");
    let mut roster = RowProjector::roster();
    store
        .append(
            input(&store, &aggregate, &id, &payload)?,
            &mut [&mut roster],
        )?
        .1
        .discard();
    assert_eq!(watermark(&store, ROSTER, &aggregate)?, Some(0));

    let id2: EventId = "evt_bad".parse()?;
    let payload2 = data("bad");
    let mut failing = RowProjector {
        projection: ROSTER,
        table: "roster_row",
        fail: true,
    };
    let refused = store.append(
        input(&store, &aggregate, &id2, &payload2)?,
        &mut [&mut failing],
    );
    assert!(
        matches!(refused, Err(WriteError::Projector { .. })),
        "{refused:?}"
    );

    // The fact, the projected row and the watermark all stayed where the last
    // successful append left them.
    let events: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    let projected: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM roster_row", [], |r| r.get(0))?;
    assert_eq!((events, projected), (1, 1));
    assert_eq!(
        watermark(&store, ROSTER, &aggregate)?,
        Some(0),
        "a failed projection must not leave its watermark advanced"
    );
    Ok(())
}

#[test]
fn an_exact_retry_advances_no_watermark_and_names_no_second_notice() -> TestResult {
    let (mut store, aggregate) = elected("ses_wm")?;
    let id: EventId = "evt_once".parse()?;
    let payload = data("once");
    let mut roster = RowProjector::roster();

    let (first, notices) = store.append(
        input(&store, &aggregate, &id, &payload)?,
        &mut [&mut roster],
    )?;
    assert_eq!(first, Appended::Committed { seq: 0 });
    assert_eq!(notices.len(), 1, "the first append names its fact");
    notices.discard();

    // The exact same event id, aggregate, type, created and data.
    let (second, notices) = store.append(
        input(&store, &aggregate, &id, &payload)?,
        &mut [&mut roster],
    )?;
    assert_eq!(second, Appended::Idempotent { seq: 0 });
    assert!(
        notices.is_empty(),
        "an idempotent replay must not name a second time: the fact became \
         observable once, and a duplicate notice would announce a second one"
    );
    assert_eq!(notices.dropped(), 0);
    notices.discard();

    // And the projector never ran again, so nothing double-projected.
    assert_eq!(watermark(&store, ROSTER, &aggregate)?, Some(0));
    let projected: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM roster_row", [], |r| r.get(0))?;
    assert_eq!(projected, 1, "a retry must not project the fact twice");
    Ok(())
}

#[test]
fn a_notice_names_exactly_the_fact_that_was_written() -> TestResult {
    let (mut store, aggregate) = elected("ses_wm")?;
    let id: EventId = "evt_named".parse()?;
    let payload = data("named");
    let epoch = store.fencing_token(&aggregate)?.epoch();
    let (_, notices) = store.append(input(&store, &aggregate, &id, &payload)?, &mut [])?;
    let notice = notices.iter().next().ok_or("one notice")?.clone();
    assert_eq!(notice.id().as_str(), "evt_named");
    assert_eq!(notice.aggregate(), &aggregate);
    assert_eq!(notice.seq(), 0);
    assert_eq!(
        notice.writer(),
        epoch,
        "the notice carries the epoch the fact was fenced under"
    );
    notices.discard();
    Ok(())
}

#[test]
fn a_published_notice_names_a_fact_a_foreign_connection_can_already_read() -> TestResult {
    // The commit-before-notify law, observed from outside the writing
    // connection. A separate `Connection` sees only COMMITTED state, so if a
    // notice were released before the commit, the lookup below would find
    // nothing.
    let (dir, db) = on_disk("ordering")?;
    let mut store = EventStore::open(&db, "test")?;
    let aggregate = session("ses_wm")?;
    store.elect_writer(&aggregate)?;

    struct ReadsThrough {
        path: std::path::PathBuf,
        found: Vec<Option<i64>>,
    }
    impl Notifier for ReadsThrough {
        fn notify(&mut self, notice: &DurableNotice) {
            // A failure to open is recorded as "not found" rather than
            // unwrapped: the assertion below is what reports it, so the witness
            // cannot pass by panicking somewhere the reader never reached.
            let seq = Connection::open(&self.path).ok().and_then(|conn| {
                conn.query_row(
                    "SELECT seq FROM event WHERE id = ?1",
                    [notice.id().as_str()],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            });
            self.found.push(seq);
        }
    }

    let id: EventId = "evt_visible".parse()?;
    let payload = data("visible");
    let (_, notices) = store.append(input(&store, &aggregate, &id, &payload)?, &mut [])?;
    let mut reader = ReadsThrough {
        path: db.clone(),
        found: Vec::new(),
    };
    notices.publish(&mut reader);
    assert_eq!(
        reader.found,
        vec![Some(0)],
        "a notice must never name a fact another connection cannot yet see"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_failed_commit_publishes_nothing_and_leaves_nothing_durable() -> TestResult {
    // A REAL commit failure, not a simulated one: a deferred foreign key is
    // checked at COMMIT, so the append and its projection succeed and then the
    // commit itself fails. That is the only ordering in which "publishes
    // nothing on persistence failure" is distinguishable from "publishes
    // nothing because nothing was appended".
    let (dir, db) = on_disk("commitfail")?;
    let mut store = EventStore::open(&db, "test")?;
    store.connection().execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE parent_row (id TEXT PRIMARY KEY);
         CREATE TABLE child_row (
             id TEXT PRIMARY KEY,
             parent TEXT NOT NULL
                 REFERENCES parent_row (id) DEFERRABLE INITIALLY DEFERRED
         );",
    )?;
    let aggregate = session("ses_wm")?;
    store.elect_writer(&aggregate)?;

    let id: EventId = "evt_doomed".parse()?;
    let payload = data("doomed");
    let held = store.held_writers();
    let mut roster = RowProjector::roster();
    let outcome = {
        let tx = store.governed_transaction()?;
        let appended = EventStore::append_within(
            &tx,
            AppendInput {
                id: &id,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.input.admitted",
                version: EventVersion::new(1).ok_or("version 1")?,
                created_ms: 1_786_060_800_000,
                data: &payload,
            },
            &mut [&mut roster],
        )?;
        assert_eq!(
            appended,
            Appended::Committed { seq: 0 },
            "the append itself must succeed, or this witnesses the wrong failure"
        );
        // Violates the deferred constraint. Nothing fails yet.
        tx.execute(
            "INSERT INTO child_row (id, parent) VALUES ('c1', 'no_such_parent')",
            [],
        )?;
        tx.commit()
    };
    assert!(
        outcome.is_err(),
        "the deferred constraint must fail this COMMIT"
    );
    // `outcome` is `Err`, so there is no `CommittedNotices` value in existence
    // for this transaction — publication is not merely skipped here, it is
    // unreachable. Probe 02 is the compile-fail half of that claim.

    let events: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(events, 0, "a failed commit must leave no fact");
    assert_eq!(
        watermark(&store, ROSTER, &aggregate)?,
        None,
        "a failed commit must leave no projection progress"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn an_abandoned_transaction_publishes_nothing() -> TestResult {
    let (mut store, aggregate) = elected("ses_wm")?;
    let id: EventId = "evt_dropped".parse()?;
    let payload = data("dropped");
    let held = store.held_writers();
    {
        let tx = store.governed_transaction()?;
        EventStore::append_within(
            &tx,
            AppendInput {
                id: &id,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.input.admitted",
                version: EventVersion::new(1).ok_or("version 1")?,
                created_ms: 1_786_060_800_000,
                data: &payload,
            },
            &mut [],
        )?;
        // Dropped without committing: the pending notice goes with it.
    }
    let events: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(events, 0);
    Ok(())
}

#[test]
fn a_projection_may_lag_and_is_rebuilt_from_durable_truth() -> TestResult {
    let (mut store, aggregate) = elected("ses_wm")?;
    // Three facts committed with NO projector: the roster projection does not
    // exist yet, which is the ordinary way a projection comes to lag.
    for marker in ["a", "b", "c"] {
        let id: EventId = format!("evt_{marker}").parse()?;
        let payload = data(marker);
        store
            .append(input(&store, &aggregate, &id, &payload)?, &mut [])?
            .1
            .discard();
    }
    let status = store.projection_status(ROSTER, &aggregate)?;
    assert_eq!(status.watermark(), None);
    assert_eq!(status.head(), Some(2));
    assert!(!status.is_caught_up(), "the projection must be behind");

    let mut roster = RowProjector::roster();
    let caught = store.catch_up(&aggregate, &mut roster)?;
    assert_eq!(caught.projected, 3, "every uncovered event must be folded");
    assert_eq!(caught.watermark, Some(2));
    assert!(
        caught.notices.is_empty(),
        "re-projecting a durable fact makes nothing newly observable, so it \
         must name nothing"
    );
    caught.notices.discard();
    assert!(store.projection_status(ROSTER, &aggregate)?.is_caught_up());
    let projected: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM roster_row", [], |r| r.get(0))?;
    assert_eq!(projected, 3);

    // Running it again is a no-op: the watermark is what stops a rebuild from
    // re-projecting rows it already has. Without it the INSERTs would collide
    // on their primary key and this second pass would error.
    match store.catch_up(&aggregate, &mut roster) {
        Ok(again) => {
            assert_eq!(
                again.projected, 0,
                "a caught-up projection must fold nothing on a second pass"
            );
            assert_eq!(again.watermark, Some(2));
            again.notices.discard();
        }
        Err(error) => panic!("a second pass must re-project nothing, not collide: {error}"),
    }
    Ok(())
}

// The refusal of a watermark that LEADS its durable head is witnessed in
// `src/projection.rs`, not here: staging that state needs the store's own
// governed lift, which is `#[cfg(test)]`-sealed and therefore unreachable from
// an integration test by construction. Reaching it from here would have meant
// publishing the lift, which is the capability the chokepoint exists to
// withhold.

#[test]
fn the_bounded_notice_buffer_never_costs_a_durable_fact() -> TestResult {
    // The advisory buffer is bounded, so a transaction that commits more facts
    // than it can name drops the excess NOTICES. It may never drop, delay or
    // fail the FACTS: an advisory concern cannot reach back into durable
    // progress.
    let (mut store, aggregate) = elected("ses_wm")?;
    let held = store.held_writers();
    let cap = grapt_store::MAX_PENDING_NOTICES;
    let overflow = 5_usize;
    let total = cap + overflow;
    let payloads: Vec<Value> = (0..total).map(|n| data(&format!("bulk{n}"))).collect();
    let ids: Vec<EventId> = (0..total)
        .map(|n| format!("evt_bulk{n}").parse())
        .collect::<Result<_, _>>()?;

    let notices = {
        let tx = store.governed_transaction()?;
        for n in 0..total {
            EventStore::append_within(
                &tx,
                AppendInput {
                    id: &ids[n],
                    aggregate: aggregate.clone(),
                    writer: held.token(&aggregate)?,
                    kind_name: "session.input.admitted",
                    version: EventVersion::new(1).ok_or("version 1")?,
                    created_ms: 1_786_060_800_000,
                    data: &payloads[n],
                },
                &mut [],
            )?;
        }
        tx.commit()?
    };
    assert_eq!(notices.len(), cap);
    assert_eq!(notices.dropped(), overflow as u32);

    let mut recorder = Recorder::default();
    notices.publish(&mut recorder);
    assert_eq!(recorder.seen.len(), cap);
    assert_eq!(
        recorder.dropped, overflow as u32,
        "a lossy publication must tell the subscriber it was lossy, or a \
         consumer would read the gap as continuity"
    );

    // The facts themselves: all of them, every sequence.
    let events: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(
        events, total as i64,
        "every fact must be durable regardless of whether it could be named"
    );
    let head = store.projection_status(ROSTER, &aggregate)?.head();
    assert_eq!(head, Some(total as u64 - 1));
    Ok(())
}

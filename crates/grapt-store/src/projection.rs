//! Authoritative projection watermarks: durable progress that may lag its
//! canonical input but can never lead it.
//!
//! # The law
//!
//! Ratified B4 / §3.2: *a rebuildable projection commits its watermark in the
//! same transaction as its rows and may lag but never lead*. The supersession
//! matrix repeats it as "watermark commits in the same transaction as projected
//! rows; projection may lag but never lead."
//!
//! # The key, and why it has three dimensions
//!
//! `(projection, aggregate_family, aggregate_id)`.
//!
//! There is no global commit sequence in this design and this lane does not
//! introduce one, so "how far has a projection got" is not answerable by a
//! single number. `EventSeq` is aggregate-scoped, which makes progress
//! per-aggregate by construction; and several projections advance independently
//! over the same aggregate, which makes it per-projection too.
//!
//! The family dimension is load-bearing for the same reason it is in the
//! election row and the sequence cursor: rows retained under the `campaign`
//! family key can carry an identity string a [`grapt_types::SessionId`] also
//! accepts, so `identity` alone is not unique across families. Each of the three dimensions
//! has its own witness below, because proving one separates rows proves nothing
//! about the other two.
//!
//! # Never leading, in two parts
//!
//! *Structurally*, on the write side: `advance` is called only from the
//! append path's projector loop and from [`crate::EventStore::catch_up`], both
//! of which pass the sequence of an event row that is inside the SAME
//! transaction. A watermark cannot commit without its event.
//!
//! *Detectably*, on the read side: `status` refuses with
//! [`crate::WriteError::WatermarkLeads`] if it ever observes a watermark past
//! the aggregate's durable head. Nothing in this crate can produce that state,
//! which is exactly why the read fails closed on it rather than trusting the
//! writer — an external mutation of the file is the reachable cause, and
//! `F3B1B_RESIDUALS.md` item 2 records that such mutations remain possible.
//!
//! # Monotonic, not contiguous
//!
//! Advance requires `seq > stored`, never `seq == stored + 1`. Within-aggregate
//! sequences are monotonic-with-gaps by measured law (the crate-graph
//! amendments: "never assume contiguity"), so a catch-up that demanded the next
//! integer would stall forever on an imported history with a gap in it.
//!
//! Requiring strictly greater is also what refuses a DUPLICATE advance: a second
//! projection of an already-projected sequence is a typed refusal rather than a
//! silent no-op, so a retry that re-ran a projector could not quietly double a
//! projected row's effect.

use crate::election::{family_of, identity_of};
use crate::governed::{GovernedTable, GovernedTransaction};
use crate::write::WriteError;
use grapt_event::AggregateId;

/// The durable identity of one authoritative projection.
///
/// It is `&'static str` because a projection is a named piece of code, not
/// runtime data: the set of projections is whatever the workspace compiles.
/// [`crate::Projector::projection`] is the single carrier — a projector has no
/// second `name()` to drift from this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectionId(&'static str);

impl ProjectionId {
    /// Deliberately unvalidated and `const`, so a projector can name itself in a
    /// trait method without ceremony. The refusal lives at the durable boundary
    /// instead: `advance` rejects an empty identity rather than keying a
    /// watermark on one. Validating here as well would put the same law in two
    /// places and leave neither attributable.
    pub const fn new(name: &'static str) -> Self {
        ProjectionId(name)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ProjectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Where one projection stands against one aggregate's durable head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionStatus {
    watermark: Option<u64>,
    head: Option<u64>,
}

impl ProjectionStatus {
    /// The highest sequence this projection has durably projected, or `None`
    /// when it has projected nothing for this aggregate.
    pub fn watermark(self) -> Option<u64> {
        self.watermark
    }

    /// The aggregate's highest durable event sequence, or `None` when it has no
    /// events.
    pub fn head(self) -> Option<u64> {
        self.head
    }

    /// True when the projection has consumed everything durable.
    ///
    /// Lag is expressed as this predicate rather than as `head - watermark`
    /// because sequences are monotonic-with-gaps: the arithmetic difference is
    /// not a count of outstanding events and would read as one.
    pub fn is_caught_up(self) -> bool {
        self.watermark == self.head
    }
}

pub(crate) fn ensure_schema(conn: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS projection_watermark (
            projection TEXT NOT NULL,
            aggregate_family TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            PRIMARY KEY (projection, aggregate_family, aggregate_id)
        );",
    )
}

/// This projection's durable position for this aggregate.
///
/// A negative stored value is corruption, not a position: it refuses rather
/// than coercing, exactly as the sequence cursor does.
pub(crate) fn read(
    conn: &rusqlite::Connection,
    projection: ProjectionId,
    aggregate: &AggregateId,
) -> Result<Option<u64>, WriteError> {
    let stored: Option<i64> = conn
        .query_row(
            "SELECT seq FROM projection_watermark
             WHERE projection = ?1 AND aggregate_family = ?2 AND aggregate_id = ?3",
            rusqlite::params![
                projection.as_str(),
                family_of(aggregate),
                identity_of(aggregate)
            ],
            |r| r.get::<_, i64>(0),
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(e)
            }
        })?;
    match stored {
        None => Ok(None),
        Some(value) => u64::try_from(value)
            .map(Some)
            .map_err(|_| WriteError::CorruptWatermark {
                projection: projection.as_str(),
                aggregate: identity_of(aggregate).to_owned(),
                value,
            }),
    }
}

/// Advances this projection's watermark to `seq` INSIDE the caller's
/// transaction.
///
/// Both callers pass the sequence of an event row written in that same
/// transaction, so the watermark commits with the rows it describes or not at
/// all. There is no path that advances a watermark on its own.
pub(crate) fn advance(
    tx: &GovernedTransaction<'_>,
    projection: ProjectionId,
    aggregate: &AggregateId,
    seq: u64,
) -> Result<(), WriteError> {
    if projection.as_str().trim().is_empty() {
        return Err(WriteError::UnnamedProjection);
    }
    if let Some(stored) = read(tx, projection, aggregate)? {
        if seq <= stored {
            return Err(WriteError::WatermarkNotMonotonic {
                projection: projection.as_str(),
                aggregate: identity_of(aggregate).to_owned(),
                stored,
                attempted: seq,
            });
        }
    }
    // Unreachable from either caller — the append path already converted this
    // sequence, and catch-up read it back out of an INTEGER column — but the
    // conversion is the same failure the cursor names, so it reuses that
    // variant rather than inventing an unwitnessable second one.
    let seq_db = i64::try_from(seq).map_err(|_| WriteError::SequenceExhausted {
        aggregate: identity_of(aggregate).to_owned(),
    })?;
    tx.gate().execute(
        tx,
        GovernedTable::ProjectionWatermark,
        "INSERT INTO projection_watermark (projection, aggregate_family, aggregate_id, seq)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(projection, aggregate_family, aggregate_id) DO UPDATE SET seq = excluded.seq",
        rusqlite::params![
            projection.as_str(),
            family_of(aggregate),
            identity_of(aggregate),
            seq_db
        ],
    )?;
    Ok(())
}

/// The aggregate's durable head: the highest sequence present in `event`.
pub(crate) fn head(
    conn: &rusqlite::Connection,
    aggregate: &AggregateId,
) -> Result<Option<u64>, WriteError> {
    // MAX over an empty set is one row containing NULL, not zero rows.
    let found: Option<i64> = conn.query_row(
        "SELECT MAX(seq) FROM event WHERE aggregate_family = ?1 AND aggregate_id = ?2",
        rusqlite::params![family_of(aggregate), identity_of(aggregate)],
        |r| r.get::<_, Option<i64>>(0),
    )?;
    match found {
        None => Ok(None),
        Some(value) => u64::try_from(value)
            .map(Some)
            .map_err(|_| WriteError::CorruptSequence {
                aggregate: identity_of(aggregate).to_owned(),
                value,
            }),
    }
}

/// Reads the projection's position and the aggregate's head together, and
/// REFUSES a watermark that leads.
///
/// Nothing in this crate can write that state. The check is here because the
/// database file is reachable from outside the chokepoint
/// (`F3B1B_RESIDUALS.md` item 2), and a projection claiming progress its
/// canonical input does not have is precisely the corruption the law forbids —
/// a caller must not be handed it as an ordinary lag.
pub(crate) fn status(
    conn: &rusqlite::Connection,
    projection: ProjectionId,
    aggregate: &AggregateId,
) -> Result<ProjectionStatus, WriteError> {
    let watermark = read(conn, projection, aggregate)?;
    let head = head(conn, aggregate)?;
    if let Some(mark) = watermark {
        // A watermark with no head at all leads just as surely as one past a
        // head: the projection claims progress over an aggregate that has no
        // durable facts.
        let leads = match head {
            None => true,
            Some(head) => mark > head,
        };
        if leads {
            return Err(WriteError::WatermarkLeads {
                projection: projection.as_str(),
                aggregate: identity_of(aggregate).to_owned(),
                watermark: mark,
                head,
            });
        }
    }
    Ok(ProjectionStatus { watermark, head })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::EventStore;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const A: ProjectionId = ProjectionId::new("projection-a");
    const B: ProjectionId = ProjectionId::new("projection-b");

    fn session(id: &str) -> Result<AggregateId, Box<dyn std::error::Error>> {
        Ok(AggregateId::Session(id.parse()?))
    }

    /// A store whose transaction can be opened for direct watermark work.
    ///
    /// These tests exercise [`advance`] against the key itself. The append
    /// path's use of it is witnessed in `write` and in
    /// `tests/projection_watermark.rs`; separating them is deliberate, because
    /// the key's family dimension is what these tests pin.
    fn store() -> Result<EventStore, Box<dyn std::error::Error>> {
        Ok(EventStore::open_in_memory()?)
    }

    #[test]
    fn the_watermark_is_scoped_by_projection() -> TestResult {
        // Two projections over ONE aggregate. Without the projection dimension
        // the second advance would read the first's position and refuse as a
        // duplicate, so this is the dimension's own witness.
        let mut store = store()?;
        let aggregate = session("ses_p1")?;
        let tx = store.governed_transaction()?;
        advance(&tx, A, &aggregate, 7)?;
        if let Err(refused) = advance(&tx, B, &aggregate, 3) {
            panic!("projection B read a position that is not its own: {refused}");
        }
        assert_eq!(read(&tx, A, &aggregate)?, Some(7));
        assert_eq!(
            read(&tx, B, &aggregate)?,
            Some(3),
            "a slower projection keeps its own position"
        );
        // The consequence, not just the reads: B may still advance to 4, which
        // it could not if it were sharing A's row at 7.
        advance(&tx, B, &aggregate, 4)?;
        assert_eq!(read(&tx, B, &aggregate)?, Some(4));
        assert_eq!(read(&tx, A, &aggregate)?, Some(7), "A is unmoved");
        tx.commit()?.discard();
        Ok(())
    }

    #[test]
    fn the_watermark_is_scoped_by_aggregate_identity() -> TestResult {
        let mut store = store()?;
        let one = session("ses_p2")?;
        let two = session("ses_p3")?;
        let tx = store.governed_transaction()?;
        advance(&tx, A, &one, 5)?;
        if let Err(refused) = advance(&tx, A, &two, 1) {
            panic!("a sibling aggregate read another aggregate's position: {refused}");
        }
        assert_eq!(read(&tx, A, &one)?, Some(5));
        assert_eq!(read(&tx, A, &two)?, Some(1));
        advance(&tx, A, &two, 2)?;
        assert_eq!(read(&tx, A, &one)?, Some(5), "the sibling is unmoved");
        tx.commit()?.discard();
        Ok(())
    }

    #[test]
    fn the_watermark_is_scoped_by_aggregate_family() -> TestResult {
        // The election row and the sequence cursor each have this witness
        // (`the_election_row_is_family_scoped`, `the_store_key_is_family_scoped`)
        // and the watermark needs its own: the three reads are separate
        // carriers, so the others stay green when this predicate goes missing.
        let mut store = store()?;
        let ses = session("ses_p4")?;
        let camp = AggregateId::Mail(
            grapt_types::remote_mail::MailStreamId::try_new("ses_p4")
                .ok()
                .ok_or("mail stream id")?,
        );
        assert_eq!(
            identity_of(&camp),
            identity_of(&ses),
            "the two identities must collide or this proves nothing"
        );
        assert_ne!(
            family_of(&camp),
            family_of(&ses),
            "the families must differ or there is no key to scope"
        );

        let tx = store.governed_transaction()?;
        advance(&tx, A, &ses, 9)?;
        // A never-advanced mail stream starts from nothing: it must not read the
        // same-named session's position, which would refuse this as a duplicate.
        if let Err(refused) = advance(&tx, A, &camp, 1) {
            panic!("a mail stream read a same-named session's position: {refused}");
        }
        assert_eq!(read(&tx, A, &camp)?, Some(1));
        assert_eq!(
            read(&tx, A, &ses)?,
            Some(9),
            "the session's watermark must be unmoved by the mail stream's advance"
        );
        tx.commit()?.discard();
        Ok(())
    }

    #[test]
    fn an_advance_must_be_strictly_greater_than_the_stored_position() -> TestResult {
        let mut store = store()?;
        let aggregate = session("ses_p5")?;
        let tx = store.governed_transaction()?;
        advance(&tx, A, &aggregate, 4)?;

        // Equal is the duplicate-projection case: the same sequence projected
        // twice would double a projected row's effect.
        let repeat = advance(&tx, A, &aggregate, 4);
        match repeat {
            Err(WriteError::WatermarkNotMonotonic {
                stored: 4,
                attempted: 4,
                ..
            }) => {}
            other => panic!("re-advancing to the stored position must refuse: {other:?}"),
        }
        // Lower is a projection going backwards.
        match advance(&tx, A, &aggregate, 1) {
            Err(WriteError::WatermarkNotMonotonic {
                stored: 4,
                attempted: 1,
                ..
            }) => {}
            other => panic!("a backwards advance must refuse: {other:?}"),
        }
        // A GAP is legal: sequences are monotonic-with-gaps, so 4 -> 9 must be
        // accepted or an imported history would stall a catch-up forever.
        advance(&tx, A, &aggregate, 9)?;
        assert_eq!(read(&tx, A, &aggregate)?, Some(9));
        // The refusals wrote nothing: the position is the one the last legal
        // advance left, not one the refused calls moved.
        tx.commit()?.discard();
        Ok(())
    }

    #[test]
    fn an_unnamed_projection_cannot_key_a_watermark() -> TestResult {
        let mut store = store()?;
        let aggregate = session("ses_p6")?;
        let tx = store.governed_transaction()?;
        for blank in [ProjectionId::new(""), ProjectionId::new("   ")] {
            match advance(&tx, blank, &aggregate, 1) {
                Err(WriteError::UnnamedProjection) => {}
                other => panic!("a blank projection identity must refuse: {other:?}"),
            }
        }
        let rows: i64 = tx.query_row("SELECT COUNT(*) FROM projection_watermark", [], |r| {
            r.get(0)
        })?;
        assert_eq!(rows, 0, "a refused advance must write no row");
        tx.commit()?.discard();
        Ok(())
    }

    #[test]
    fn a_watermark_past_the_durable_head_refuses_rather_than_reading_as_lag() -> TestResult {
        // Nothing in this crate can write this state, and that is precisely why
        // the READ must refuse it: the database file is reachable from outside
        // the chokepoint (`F3B1B_RESIDUALS.md` item 2), so "the writer cannot
        // produce it" is not a reason for the reader to believe it.
        //
        // It lives here rather than in `tests/projection_watermark.rs` because
        // staging it needs the store's own governed lift, which is
        // `#[cfg(test)]`-sealed and unreachable from an integration test.
        let store = store()?;
        let aggregate = session("ses_p8")?;

        // No head at all, and a watermark claiming progress over it. This is the
        // arm an `Option` comparison silently gets backwards.
        crate::write::hostile_rows::install_watermark(
            store.connection(),
            store.gate(),
            A,
            &aggregate,
            0,
        )?;
        match status(store.connection(), A, &aggregate) {
            Err(WriteError::WatermarkLeads {
                watermark: 0,
                head: None,
                ..
            }) => {}
            other => panic!("a watermark over an aggregate with no facts must refuse: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn a_caught_up_watermark_is_not_mistaken_for_a_leading_one() -> TestResult {
        // The other side of the same comparison. Without it, a refusal written
        // as `mark >= head` would pass the witness above and reject every
        // healthy projection that had caught up.
        let mut store = store()?;
        let aggregate = session("ses_p9")?;
        store.elect_writer(&aggregate)?;
        let id: grapt_types::EventId = "evt_p9".parse()?;
        let data = serde_json::json!({"sessionID":"ses_p9","inputID":"msg_p9"});
        let writer = store.fencing_token(&aggregate)?;
        store
            .append(
                crate::write::AppendInput {
                    id: &id,
                    aggregate: aggregate.clone(),
                    writer,
                    kind_name: "session.input.admitted",
                    version: grapt_types::EventVersion::new(1).ok_or("version")?,
                    created_ms: 1_786_060_800_000,
                    data: &data,
                },
                &mut [],
            )?
            .1
            .discard();
        let tx = store.governed_transaction()?;
        advance(&tx, A, &aggregate, 0)?;
        tx.commit()?.discard();

        let observed = status(store.connection(), A, &aggregate)?;
        assert_eq!(observed.watermark(), Some(0));
        assert_eq!(observed.head(), Some(0));
        assert!(
            observed.is_caught_up(),
            "a projection level with its head is caught up, not leading"
        );
        Ok(())
    }

    #[test]
    fn a_corrupt_stored_position_refuses_rather_than_coercing() -> TestResult {
        let store = store()?;
        let aggregate = session("ses_p7")?;
        crate::write::hostile_rows::install_watermark(
            store.connection(),
            store.gate(),
            A,
            &aggregate,
            -3,
        )?;
        match read(store.connection(), A, &aggregate) {
            Err(WriteError::CorruptWatermark { value: -3, .. }) => Ok(()),
            other => panic!("a negative stored position must refuse: {other:?}"),
        }
    }
}

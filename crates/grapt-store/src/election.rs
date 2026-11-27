//! Aggregate-scoped writer election and the fencing check that runs BEFORE a
//! sequence is allocated.
//!
//! # What this owns
//!
//! The master's writer-epoch law (foundational types §"Writer-epoch and legacy-event
//! law", supersession matrix "Commit identity"): `WriterEpoch` is an
//! aggregate-scoped monotonic fencing token issued ONLY by this authority. The
//! first elected Rust writer uses [`WriterEpoch::FIRST`]; every ownership
//! transfer increments it transactionally. Append compares the submitted epoch
//! with the currently elected epoch before allocating `EventSeq` or writing
//! bytes; a stale or unknown epoch is a typed refusal that publishes nothing.
//!
//! # What this is NOT
//!
//! This is not the SQLite schema `engine_epoch` in [`crate::epoch`]. That is a
//! database-FORMAT stamp: one row per database, describing which engine owns the
//! file. It cannot fence a writer — it does not name an aggregate, it never
//! advances on ownership transfer, and two live processes that both opened the
//! same Rust-stamped file carry the identical value. The two are kept in
//! separate modules and separate tables so a later reader cannot mistake one for
//! the other; `engine_epoch_cannot_satisfy_writer_fencing` pins the distinction
//! as an executable fact rather than a comment.
//!
//! # Keying
//!
//! Election and the sequence cursor are keyed by the accepted
//! [`grapt_event::AggregateId`] — family PLUS identity, not a bare session
//! string. A campaign's epoch fences CAMPAIGN ownership and must never be
//! session-scoped (F2A envelope law), so the family is part of the key rather
//! than an assumption that identity prefixes never collide.
//!
//! The election row and the cursor are two separate carriers of that one law,
//! so each has its own witness: `the_election_row_is_family_scoped` here, and
//! `the_store_key_is_family_scoped` in [`crate::write`] for the cursor. Removing
//! the family predicate from either read leaves the other's witness green.

use crate::governed::{GovernedTable, WriteGate};
use grapt_event::AggregateId;
use grapt_types::{IdAuthority, WriterEpoch};
use rusqlite::Transaction;

/// The identity of one writing party, as stored in `aggregate_writer.owner_id`.
///
/// Opaque and store-owned: a caller cannot author it, so a consumer cannot
/// impersonate another owner by passing a string. It identifies a STORE
/// INSTANCE, which is the granularity fencing needs — two `EventStore` values
/// open on the same file are two independent writers and must be able to fence
/// each other, exactly as two processes would.
///
/// # It must be unique over TIME, not just among live processes
///
/// This is the property `elect` depends on and the one an earlier shape did
/// not have. The identity used to be `store-{process id}-{per-process counter}`.
/// That distinguishes concurrent processes, and distinguishes instances inside
/// one process — but BOTH components reset when a process ends, so after the
/// operating system reused a process id the first store in the new process
/// minted the byte-identical string the dead one had used. `elect` then took
/// its `Retained` arm, which leaves the epoch UNCHANGED, and ownership changed
/// hands with no epoch advance: `Transferred`, whose whole purpose is to make
/// every epoch the previous owner holds stale, never ran.
///
/// The distinguishing component is therefore drawn from OS randomness by the
/// named adapter in [`crate::ids`], and it is the SOLE carrier of that
/// property. The process id is retained for human diagnosis only and is
/// deliberately not load-bearing — no witness here attributes it, because
/// removing it would change no outcome.
///
/// The value is not durable identity beyond the currently elected row and
/// carries no authority by itself; only the elected `(owner, epoch)` pair in the
/// database decides who may write.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WriterOwnerId(String);

impl WriterOwnerId {
    /// Mints an owner identity from an injected [`IdAuthority`].
    ///
    /// The authority is injected rather than ambient because row-11 law confines
    /// ambient time and ID creation to named production adapters; production
    /// callers pass [`crate::ids::OsRandomIds`] and witnesses pass a
    /// deterministic authority that can model a restarted process.
    ///
    /// This reads no clock, and the uniqueness claim does not rest on one. A
    /// clock reading is not a uniqueness argument: two processes can observe the
    /// same millisecond.
    pub(crate) fn mint(ids: &dyn IdAuthority) -> Self {
        WriterOwnerId(format!(
            "store-{}-{}",
            std::process::id(),
            ids.mint().as_str()
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WriterOwnerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why an election or a fencing check refuses.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ElectionError {
    #[error("sqlite: {0}")]
    Sqlite(String),
    /// No writer has ever been elected for this aggregate. An append carrying
    /// any epoch is refused: an unknown writer is not a stale writer, and
    /// treating it as "epoch 1 by default" would let an unelected process
    /// write the aggregate's first event.
    #[error("no writer is elected for {family}:{aggregate}; refusing to append")]
    UnknownWriter {
        family: &'static str,
        aggregate: String,
    },
    /// The submitted epoch is not the currently elected one. Covers both a
    /// lost owner (submitted < elected) and a fabricated future epoch
    /// (submitted > elected): only the exact elected epoch may write.
    #[error(
        "writer epoch {submitted} is not the elected epoch {elected} for {family}:{aggregate}; refusing to append"
    )]
    StaleWriter {
        family: &'static str,
        aggregate: String,
        submitted: u32,
        elected: u32,
    },
    /// The stored epoch is not a valid `WriterEpoch` (zero or negative).
    #[error("corrupt elected epoch {value} for {family}:{aggregate}")]
    CorruptEpoch {
        family: &'static str,
        aggregate: String,
        value: i64,
    },
    /// `WriterEpoch::next` hit the ceiling. Checked, never saturating: an epoch
    /// that silently stopped advancing would let a lost owner's writes pass.
    #[error("writer epoch space exhausted for {family}:{aggregate}")]
    EpochExhausted {
        family: &'static str,
        aggregate: String,
    },
}

impl From<rusqlite::Error> for ElectionError {
    fn from(error: rusqlite::Error) -> Self {
        ElectionError::Sqlite(error.to_string())
    }
}

/// What an [`crate::EventStore::elect_writer`] call did.
///
/// The link names the public entry point rather than this module's `elect`,
/// which F3B1b made crate-private: a public type may not document itself
/// through an item its readers cannot reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Election {
    /// No writer existed; this owner is the first and holds
    /// [`WriterEpoch::FIRST`].
    Elected { epoch: WriterEpoch },
    /// This owner already held the aggregate. The epoch is UNCHANGED — a
    /// re-election by the incumbent is not an ownership transfer, and
    /// advancing here would fence the incumbent's own in-flight writes.
    Retained { epoch: WriterEpoch },
    /// A different owner held the aggregate; ownership transferred and the
    /// epoch advanced. Every epoch the previous owner holds is now stale.
    Transferred {
        epoch: WriterEpoch,
        previous: WriterEpoch,
    },
}

impl Election {
    pub fn epoch(self) -> WriterEpoch {
        match self {
            Election::Elected { epoch }
            | Election::Retained { epoch }
            | Election::Transferred { epoch, .. } => epoch,
        }
    }
}

/// What an inspection of a possibly-lost writer's aggregate found.
///
/// This is DETECTION vocabulary and nothing more. No variant authorizes a
/// re-drive, adopts a lost owner's bytes under a new epoch, or settles an
/// ambiguous external effect. Two recorded findings are why:
///
/// * `F3B4A_RESIDUALS.md` item 2 — election advances the epoch on transfer,
///   which makes the lost owner stale, but nothing inspects or settles whatever
///   that owner had in flight. This vocabulary is that inspection.
/// * `F3B1A_RESIDUALS.md` item 9 — a retry under a different epoch DIVERGES,
///   because the epoch sits inside the envelope's canonical bytes, so the same
///   event id written under a new epoch names a DIFFERENT fact. Reconciling it
///   automatically would let a new owner adopt a lost owner's write as its own,
///   which is precisely the thing fencing exists to prevent.
///
/// So the honest output of this pass is a classification a human or a later
/// lane acts on, not a repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LostWriterState {
    /// No writer has ever been elected for this aggregate.
    ///
    /// Deliberately not folded into `Settled`: an aggregate nobody ever owned
    /// and an aggregate whose owner finished cleanly are different findings,
    /// and collapsing them would report "nothing to do" for a database that
    /// never had a writer at all.
    NoWriterElected,
    /// The allocated cursor and the durable head agree, so the previous owner's
    /// last append COMMITTED. Nothing is owed and nothing is offered.
    Settled {
        epoch: WriterEpoch,
        head: Option<u64>,
    },
    /// The cursor names a sequence the event log does not contain: a number was
    /// allocated whose event is not durable.
    ///
    /// Reachable from an interrupted writer, an imported or migrated history,
    /// or a foreign writer — not from this store's append path, which allocates
    /// the sequence and writes the event in one transaction. Exact inspection
    /// settles it; this call refuses to guess.
    ReconciliationRequired {
        epoch: WriterEpoch,
        cursor: Option<u64>,
        head: Option<u64>,
    },
}

/// Classifies an aggregate from the three durable coordinates, and only those.
///
/// Pure on purpose: it reads nothing and writes nothing, so the reads that feed
/// it can be audited separately from the decision they feed. The two collapse
/// directions are separately attributed — R03 turns the agreement arm into a
/// reconciliation demand, R04 turns the disagreement arm into a settlement —
/// because a red on one says little without a green on the other.
pub(crate) fn classify_lost_writer(
    elected: Option<(WriterOwnerId, WriterEpoch)>,
    cursor: Option<u64>,
    head: Option<u64>,
) -> LostWriterState {
    let Some((_, epoch)) = elected else {
        return LostWriterState::NoWriterElected;
    };
    match (cursor, head) {
        // Nothing allocated and nothing durable: an elected writer that never
        // wrote owes no reconciliation.
        (None, None) => LostWriterState::Settled { epoch, head: None },
        (Some(cursor), Some(head_seq)) if cursor == head_seq => LostWriterState::Settled {
            epoch,
            head: Some(head_seq),
        },
        // Everything else disagrees, including a durable head with no cursor at
        // all, which no append path can produce either.
        _ => LostWriterState::ReconciliationRequired {
            epoch,
            cursor,
            head,
        },
    }
}

/// Creates the election table. Called from the store's schema preparation
/// inside the same transaction that prepares every other governed table.
pub(crate) fn ensure_schema(tx: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS aggregate_writer (
            aggregate_family TEXT NOT NULL,
            aggregate_id     TEXT NOT NULL,
            owner_id         TEXT NOT NULL,
            epoch            INTEGER NOT NULL,
            PRIMARY KEY (aggregate_family, aggregate_id)
        );",
    )
}

/// The currently elected `(owner, epoch)` for an aggregate, or `None` when no
/// writer has ever been elected.
///
/// Crate-private: election is the store's own authority, and a consumer that
/// could read or drive it directly would be reaching past the chokepoint
/// [`crate::GovernedTransaction`] exists to be. The public surface is
/// [`crate::EventStore::elect_writer`], which holds the resulting epoch.
pub(crate) fn current(
    tx: &Transaction<'_>,
    aggregate: &AggregateId,
) -> Result<Option<(WriterOwnerId, WriterEpoch)>, ElectionError> {
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT owner_id, epoch FROM aggregate_writer
             WHERE aggregate_family = ?1 AND aggregate_id = ?2",
            rusqlite::params![family_of(aggregate), identity_of(aggregate)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(e)
            }
        })?;
    match row {
        None => Ok(None),
        Some((owner, epoch)) => Ok(Some((
            WriterOwnerId(owner),
            decode_epoch(aggregate, epoch)?,
        ))),
    }
}

/// Elects `owner` as the writer for `aggregate` inside the caller's
/// transaction.
///
/// First election issues [`WriterEpoch::FIRST`]. A re-election by the
/// incumbent retains the current epoch. A different owner is an ownership
/// TRANSFER: the epoch advances by exactly one and the row is rewritten in the
/// same transaction, so the increment and the ownership change can never be
/// observed apart.
///
/// `aggregate_writer` is a governed table, so the row is written through
/// [`WriteGate::execute`]: the denial is lifted for that one table, for that one
/// statement, and lowered again before this function returns.
pub(crate) fn elect(
    tx: &Transaction<'_>,
    gate: &WriteGate,
    aggregate: &AggregateId,
    owner: &WriterOwnerId,
) -> Result<Election, ElectionError> {
    let outcome = match current(tx, aggregate)? {
        None => Election::Elected {
            epoch: WriterEpoch::FIRST,
        },
        Some((incumbent, epoch)) if &incumbent == owner => Election::Retained { epoch },
        Some((_, previous)) => {
            let epoch = previous
                .next()
                .ok_or_else(|| ElectionError::EpochExhausted {
                    family: family_of(aggregate),
                    aggregate: identity_of(aggregate).to_owned(),
                })?;
            Election::Transferred { epoch, previous }
        }
    };
    if !matches!(outcome, Election::Retained { .. }) {
        gate.execute(
            tx,
            GovernedTable::AggregateWriter,
            "INSERT INTO aggregate_writer (aggregate_family, aggregate_id, owner_id, epoch)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(aggregate_family, aggregate_id)
             DO UPDATE SET owner_id = excluded.owner_id, epoch = excluded.epoch",
            rusqlite::params![
                family_of(aggregate),
                identity_of(aggregate),
                owner.as_str(),
                i64::from(outcome.epoch().value())
            ],
        )?;
    }
    Ok(outcome)
}

/// The fencing check. Refuses a stale or unknown writer.
///
/// Callers run this BEFORE allocating a sequence, encoding bytes, running a
/// projector or publishing anything. Comparison is EXACT equality against the
/// elected epoch: `submitted < elected` is a lost owner and `submitted >
/// elected` is a fabricated token, and neither may write. A `<=` comparison
/// would admit the fabricated case, which is why the mutation set separates
/// "stale comparison widened" from "unknown writer accepted".
///
/// Crate-private for the same reason as [`current`]: the fence is consulted by
/// the store's append path, not by a consumer.
pub(crate) fn fence(
    tx: &Transaction<'_>,
    aggregate: &AggregateId,
    submitted: WriterEpoch,
) -> Result<(), ElectionError> {
    let Some((_, elected)) = current(tx, aggregate)? else {
        return Err(ElectionError::UnknownWriter {
            family: family_of(aggregate),
            aggregate: identity_of(aggregate).to_owned(),
        });
    };
    if submitted != elected {
        return Err(ElectionError::StaleWriter {
            family: family_of(aggregate),
            aggregate: identity_of(aggregate).to_owned(),
            submitted: submitted.value(),
            elected: elected.value(),
        });
    }
    Ok(())
}

fn decode_epoch(aggregate: &AggregateId, stored: i64) -> Result<WriterEpoch, ElectionError> {
    u32::try_from(stored)
        .ok()
        .and_then(WriterEpoch::new)
        .ok_or_else(|| ElectionError::CorruptEpoch {
            family: family_of(aggregate),
            aggregate: identity_of(aggregate).to_owned(),
            value: stored,
        })
}

/// The aggregate family label used as part of the election/sequence key.
///
/// Deliberately re-derived here rather than exposed from `grapt-event`:
/// the envelope's `family()` is private to its canonical byte layout, and a
/// store key that silently tracked a byte-format change would repoint every
/// existing row. These two strings must agree, which
/// `the_store_key_family_matches_the_envelope_family` pins.
pub(crate) fn family_of(aggregate: &AggregateId) -> &'static str {
    match aggregate {
        AggregateId::Session(_) => "session",
        AggregateId::Form(_) => "form",
        AggregateId::Mail(_) => "mail",
    }
}

pub(crate) fn identity_of(aggregate: &AggregateId) -> &str {
    match aggregate {
        AggregateId::Session(id) => id.as_str(),
        AggregateId::Form(id) => id.as_str(),
        AggregateId::Mail(id) => id.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn db() -> Result<Connection, Box<dyn std::error::Error>> {
        let mut conn = Connection::open_in_memory()?;
        let tx = conn.transaction()?;
        ensure_schema(&tx)?;
        tx.commit()?;
        Ok(conn)
    }

    fn session(id: &str) -> Result<AggregateId, Box<dyn std::error::Error>> {
        Ok(AggregateId::Session(id.parse()?))
    }

    /// Named owners for the tests. Production owners are minted, never
    /// authored — this constructor is reachable here only because a child
    /// module sees its parent's private fields.
    fn owner(name: &str) -> WriterOwnerId {
        WriterOwnerId(name.to_owned())
    }

    /// A gate for the election tests' bare connections.
    ///
    /// These tests drive `elect` on a raw [`Connection`] that carries no
    /// installed authorizer, so this gate lifts nothing and denies nothing: it
    /// satisfies the signature without changing a single outcome here. The
    /// governed-write behaviour is witnessed where it lives — in
    /// [`crate::governed`] and against a real [`crate::EventStore`] — not by
    /// these election-law tests.
    fn gate() -> WriteGate {
        WriteGate::new()
    }

    /// W2 — the consequence witness, and the load-bearing one.
    ///
    /// A process dies holding an aggregate; the operating system reuses its
    /// process id; the new process opens a store and elects. That MUST be an
    /// ownership transfer with an epoch advance, because everything the dead
    /// owner might still emit has to become stale.
    ///
    /// Two adapters seeded inside this one test process model the two lifetimes
    /// exactly: `std::process::id()` is identical for both by construction, so
    /// the only thing that can separate the two identities is the seeded draw.
    /// When it could not — the old `{process id}-{per-process counter}` shape —
    /// `elect` took the `Retained` arm and left the epoch UNCHANGED, which is
    /// the fencing bypass this lane closes.
    ///
    /// This asserts the CONSEQUENCE rather than arguing that process-id reuse
    /// is rare: whether the collision is likely is not the question, because the
    /// election arm it selects is wrong whenever it happens.
    #[test]
    fn a_restarted_process_at_a_reused_pid_transfers_ownership_instead_of_retaining_it(
    ) -> TestResult {
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let agg = session("ses_pid_reuse")?;

        let first_life = crate::ids::OsRandomIds::seed()?;
        let second_life = crate::ids::OsRandomIds::seed()?;
        let dead = WriterOwnerId::mint(&first_life);
        let restarted = WriterOwnerId::mint(&second_life);

        assert_ne!(
            dead, restarted,
            "a restarted process minted the dead instance's identity, so the \
             election below cannot tell an ownership transfer from a re-election"
        );

        assert_eq!(
            elect(&tx, &gate(), &agg, &dead)?,
            Election::Elected {
                epoch: WriterEpoch::FIRST
            }
        );

        let second = WriterEpoch::FIRST
            .next()
            .ok_or("epoch space exhausted at the first advance")?;
        assert_eq!(
            elect(&tx, &gate(), &agg, &restarted)?,
            Election::Transferred {
                epoch: second,
                previous: WriterEpoch::FIRST
            },
            "the restarted process RETAINED the dead owner's epoch instead of \
             transferring; every write the lost owner still holds is unfenced"
        );
        Ok(())
    }

    #[test]
    fn the_first_elected_writer_begins_at_epoch_one() -> TestResult {
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let agg = session("ses_e1")?;
        assert_eq!(
            elect(&tx, &gate(), &agg, &owner("owner-a"))?,
            Election::Elected {
                epoch: WriterEpoch::FIRST
            }
        );
        assert_eq!(WriterEpoch::FIRST.value(), 1, "first Rust epoch is one");
        Ok(())
    }

    #[test]
    fn re_election_by_the_incumbent_does_not_advance_the_epoch() -> TestResult {
        // An incumbent that re-elected itself into a NEW epoch would fence its
        // own in-flight writes — the failure this separates from transfer.
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let agg = session("ses_e2")?;
        elect(&tx, &gate(), &agg, &owner("owner-a"))?;
        assert_eq!(
            elect(&tx, &gate(), &agg, &owner("owner-a"))?,
            Election::Retained {
                epoch: WriterEpoch::FIRST
            }
        );
        Ok(())
    }

    #[test]
    fn ownership_transfer_advances_the_epoch_by_exactly_one() -> TestResult {
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let agg = session("ses_e3")?;
        elect(&tx, &gate(), &agg, &owner("owner-a"))?;
        let transferred = elect(&tx, &gate(), &agg, &owner("owner-b"))?;
        let second = match WriterEpoch::new(2) {
            Some(v) => v,
            None => panic!("epoch 2"),
        };
        assert_eq!(
            transferred,
            Election::Transferred {
                epoch: second,
                previous: WriterEpoch::FIRST
            }
        );
        let (owner, epoch) = match current(&tx, &agg)? {
            Some(v) => v,
            None => panic!("an elected writer must be readable after transfer"),
        };
        assert_eq!((owner.as_str(), epoch), ("owner-b", second));
        Ok(())
    }

    #[test]
    fn a_stale_owner_is_fenced_after_transfer() -> TestResult {
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let agg = session("ses_e4")?;
        elect(&tx, &gate(), &agg, &owner("owner-a"))?;
        // Owner A still holds epoch 1 in memory; B takes ownership.
        elect(&tx, &gate(), &agg, &owner("owner-b"))?;
        let refused = fence(&tx, &agg, WriterEpoch::FIRST);
        assert!(
            matches!(
                refused,
                Err(ElectionError::StaleWriter {
                    submitted: 1,
                    elected: 2,
                    ..
                })
            ),
            "the lost owner's epoch must be refused, got {refused:?}"
        );
        Ok(())
    }

    #[test]
    fn a_fabricated_future_epoch_is_refused_too() -> TestResult {
        // Separated from the stale witness on purpose: a `<=` comparison passes
        // the stale case and this one is the only side that catches it.
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let agg = session("ses_e5")?;
        elect(&tx, &gate(), &agg, &owner("owner-a"))?;
        let ahead = match WriterEpoch::new(99) {
            Some(v) => v,
            None => panic!("epoch 99"),
        };
        let refused = fence(&tx, &agg, ahead);
        assert!(
            matches!(
                refused,
                Err(ElectionError::StaleWriter {
                    submitted: 99,
                    elected: 1,
                    ..
                })
            ),
            "an epoch ahead of the election is not authority, got {refused:?}"
        );
        Ok(())
    }

    #[test]
    fn an_unelected_aggregate_refuses_every_epoch() -> TestResult {
        // Distinct from stale: there is no election at all. Defaulting this to
        // "epoch 1 is fine" would let an unelected process write the
        // aggregate's first event.
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let agg = session("ses_e6")?;
        let refused = fence(&tx, &agg, WriterEpoch::FIRST);
        assert!(
            matches!(refused, Err(ElectionError::UnknownWriter { .. })),
            "an unelected aggregate must refuse, got {refused:?}"
        );
        Ok(())
    }

    #[test]
    fn election_is_scoped_per_aggregate() -> TestResult {
        // Electing one aggregate must not fence another: the token is
        // aggregate-scoped, never global.
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let one = session("ses_e7")?;
        let two = session("ses_e8")?;
        elect(&tx, &gate(), &one, &owner("owner-a"))?;
        assert!(
            matches!(
                fence(&tx, &two, WriterEpoch::FIRST),
                Err(ElectionError::UnknownWriter { .. })
            ),
            "a second aggregate is not elected by the first"
        );
        elect(&tx, &gate(), &two, &owner("owner-b"))?;
        assert_eq!(fence(&tx, &one, WriterEpoch::FIRST), Ok(()));
        assert_eq!(fence(&tx, &two, WriterEpoch::FIRST), Ok(()));
        Ok(())
    }

    #[test]
    fn the_election_row_is_family_scoped() -> TestResult {
        // Identity alone is not the election key. A mail stream whose identity
        // string equals a session's is a DIFFERENT aggregate: its first
        // election must issue `WriterEpoch::FIRST` rather than read the
        // session's row and report a transfer off it, and it must leave that
        // session's elected owner and epoch exactly where they were.
        //
        // `write::tests::the_store_key_is_family_scoped` pins the same law for
        // the sequence cursor. This is the election row's half: the two reads
        // are separate carriers, so the cursor's witness stays green when this
        // predicate is the one that goes missing.
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let ses = session("ses_e10")?;
        let camp = AggregateId::Mail(
            grapt_types::remote_mail::MailStreamId::try_new("ses_e10")
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

        elect(&tx, &gate(), &ses, &owner("owner-a"))?;
        assert_eq!(
            elect(&tx, &gate(), &camp, &owner("owner-b"))?,
            Election::Elected {
                epoch: WriterEpoch::FIRST
            },
            "a never-elected mail stream begins at epoch one; it must not inherit a same-named session's row"
        );

        // Symmetry, and the right reason: the mail stream's election wrote its OWN
        // row and moved nothing. Asserting only the variant above would still
        // hold if both aggregates shared one row that happened to read back as
        // a first election.
        assert_eq!(
            current(&tx, &ses)?.map(|(held, epoch)| (held.as_str().to_owned(), epoch)),
            Some(("owner-a".to_owned(), WriterEpoch::FIRST)),
            "the session's elected row must be unmoved by the mail stream's election"
        );
        assert_eq!(
            current(&tx, &camp)?.map(|(held, epoch)| (held.as_str().to_owned(), epoch)),
            Some(("owner-b".to_owned(), WriterEpoch::FIRST)),
            "the mail stream must hold its own elected row"
        );
        // The consequence that survives: the session's incumbent still writes.
        assert_eq!(fence(&tx, &ses, WriterEpoch::FIRST), Ok(()));
        Ok(())
    }

    #[test]
    fn a_corrupt_stored_epoch_refuses_rather_than_coercing() -> TestResult {
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let agg = session("ses_e9")?;
        tx.execute(
            "INSERT INTO aggregate_writer (aggregate_family, aggregate_id, owner_id, epoch)
             VALUES ('session', 'ses_e9', 'owner-a', 0)",
            [],
        )?;
        let refused = fence(&tx, &agg, WriterEpoch::FIRST);
        assert!(
            matches!(refused, Err(ElectionError::CorruptEpoch { value: 0, .. })),
            "epoch zero is not an elected epoch, got {refused:?}"
        );
        Ok(())
    }

    #[test]
    fn the_store_key_family_matches_the_envelope_family() -> TestResult {
        // The store key and the envelope's canonical byte layout must agree on
        // the family label. `AggregateId::family` is private to the envelope, so
        // the agreement is proven through the bytes it produces rather than
        // asserted in prose.
        let agg = session("ses_efam")?;
        let bytes = grapt_event::EventEnvelopeV2::new(
            "evt_efam".parse()?,
            agg.clone(),
            grapt_types::EventSeq::FIRST,
            WriterEpoch::FIRST,
            grapt_event::EnvelopeKind::Legacy(
                match grapt_event::EventKind::parse("session.input.admitted.1") {
                    Ok(kind) => kind,
                    Err(error) => panic!("a manifest kind must parse: {error:?}"),
                },
            ),
            grapt_types::Timestamp::from_millis(0),
            Vec::new(),
        )?
        .canonical_bytes();
        let needle = family_of(&agg).as_bytes();
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "the store's family label must be the one inside the canonical bytes"
        );
        Ok(())
    }

    #[test]
    fn engine_epoch_cannot_satisfy_writer_fencing() -> TestResult {
        // The two epochs are different facts. `engine_epoch` is one row per
        // DATABASE describing the owning engine; it names no aggregate and never
        // advances on ownership transfer. If a later reader wired fencing to it,
        // this asserts what would break: two aggregates under one database stamp
        // hold independent epochs, and a transfer moves one without the other.
        let mut conn = db()?;
        let tx = conn.transaction()?;
        let one = session("ses_ee1")?;
        let two = session("ses_ee2")?;
        elect(&tx, &gate(), &one, &owner("owner-a"))?;
        elect(&tx, &gate(), &two, &owner("owner-a"))?;
        elect(&tx, &gate(), &one, &owner("owner-b"))?;

        let first = match current(&tx, &one)? {
            Some((_, epoch)) => epoch,
            None => panic!("aggregate one must be elected"),
        };
        let second = match current(&tx, &two)? {
            Some((_, epoch)) => epoch,
            None => panic!("aggregate two must be elected"),
        };
        assert_ne!(
            first, second,
            "a per-database stamp could not represent these two values at once"
        );
        assert_eq!(grapt_types::EngineEpoch::RUST.0, 2);
        Ok(())
    }
}

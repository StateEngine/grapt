use super::model::StoredRow;
use super::{
    allocate_sequence, read_sequence_cursor, AppendInput, Appended, CaughtUp, EventStore,
    FencedWriter, HeldWriters, Projector, ProjectorError, WriteError,
};
use crate::election::{Election, LostWriterState};
use crate::form_fact::decode_fact;
use crate::governed::{GovernedTable, GovernedTransaction};
use crate::notify::{CommittedNotices, DurableNotice};
use crate::projection::{ProjectionId, ProjectionStatus};
use grapt_event::AggregateId;

#[cfg(test)]
use crate::governed::WriteGate;

impl EventStore {
    /// Elects this store instance as the writer for `aggregate` and holds the
    /// resulting epoch for later appends.
    ///
    /// EXPLICIT by contract. Nothing else in this crate calls it — in
    /// particular no append path does, because a writer that elected itself on
    /// the way past the fence could never be stale, and the refusal would be
    /// unreachable rather than merely untested.
    pub fn elect_writer(&mut self, aggregate: &AggregateId) -> Result<Election, WriteError> {
        self.elect_writer_inner(aggregate, false)
    }

    /// Claim an unused identity without transferring an existing writer. Even
    /// an interrupted creation's durable claim requires a new identity.
    pub fn claim_new_writer(&mut self, aggregate: &AggregateId) -> Result<Election, WriteError> {
        self.elect_writer_inner(aggregate, true)
    }

    /// Read-only identity availability; an interrupted claim remains visible.
    pub fn aggregate_claimed(&self, aggregate: &AggregateId) -> Result<bool, WriteError> {
        Self::aggregate_claimed_on(self.connection(), aggregate)
    }

    fn aggregate_claimed_on(
        conn: &rusqlite::Connection,
        aggregate: &AggregateId,
    ) -> Result<bool, WriteError> {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family=?1 AND aggregate_id=?2)
             OR EXISTS(SELECT 1 FROM event_sequence WHERE aggregate_family=?1 AND aggregate_id=?2)
             OR EXISTS(SELECT 1 FROM aggregate_writer WHERE aggregate_family=?1 AND aggregate_id=?2)",
            rusqlite::params![crate::election::family_of(aggregate), crate::election::identity_of(aggregate)],
            |row| row.get(0),
        )?)
    }

    fn elect_writer_inner(
        &mut self,
        aggregate: &AggregateId,
        require_fresh: bool,
    ) -> Result<Election, WriteError> {
        let tx = GovernedTransaction::begin_with_projection(
            &mut self.conn,
            self.gate.clone(),
            self.read_projection.clone(),
            self.commit_executor.clone(),
        )?;
        if require_fresh && Self::aggregate_claimed_on(&tx, aggregate)? {
            return Err(WriteError::AggregateClaimed {
                aggregate: aggregate.clone(),
            });
        }
        let outcome = crate::election::elect(&tx, tx.gate(), aggregate, &self.owner)?;
        // Election changes no fact feed. Claims and transfers use the existing
        // writer row; only a committed domain fact publishes an advisory notice.
        tx.commit()?.discard();
        self.held.insert(aggregate.clone(), outcome.epoch());
        Ok(outcome)
    }

    /// The fencing token this instance holds for `aggregate`.
    ///
    /// Returns the epoch recorded by the last successful writer election or claim
    /// on THIS instance, which is exactly what makes a lost owner detectable:
    /// after another instance takes ownership, this one still offers the epoch
    /// it held, and the database refuses it.
    pub fn fencing_token(&self, aggregate: &AggregateId) -> Result<FencedWriter, WriteError> {
        self.held_writers().token(aggregate)
    }

    /// An owned snapshot of every epoch this instance holds. Take it before
    /// opening a transaction on this store; see [`HeldWriters`].
    pub fn held_writers(&self) -> HeldWriters {
        HeldWriters(self.held.clone())
    }

    /// Release this instance's cached epoch after an aggregate becomes terminal.
    /// This does not revoke previously issued tokens or change durable fencing.
    pub fn release_held_writer(&mut self, aggregate: &AggregateId) {
        self.held.remove(aggregate);
    }

    /// This instance's governed-write lift, for the deliberate-injection module.
    ///
    /// `#[cfg(test)]` because production has no caller: the store's own governed
    /// statements take the lift from the transaction they run in, and the only
    /// other writer of governed rows is [`hostile_rows`], which is itself
    /// test-sealed. Compiling this accessor into a release build would publish a
    /// crate-internal capability with no user.
    #[cfg(test)]
    pub(crate) fn gate(&self) -> &WriteGate {
        &self.gate
    }

    /// THE write chokepoint. Opens the one immediate transaction a caller may
    /// make domain decisions and append inside (pair with `append_within`).
    ///
    /// It replaces `connection_mut`, which returned a raw `&mut Connection` and
    /// therefore let every consumer choose its own isolation level and write any
    /// table it could name — including the four this store governs. The returned
    /// [`GovernedTransaction`] still exposes the full read/query surface through
    /// `Deref`; what it cannot do is write `event`, `event_sequence`,
    /// `aggregate_writer` or `engine_epoch`, because the connection carries an
    /// installed authorizer that refuses those at statement preparation.
    pub fn governed_transaction(&mut self) -> rusqlite::Result<GovernedTransaction<'_>> {
        self.transaction_with_policy(true)
    }

    /// Verify that `writer` is still the currently elected writer for
    /// `aggregate` inside `tx`, using the store's authoritative election fence.
    pub fn verify_writer_within(
        tx: &GovernedTransaction<'_>,
        aggregate: &AggregateId,
        writer: FencedWriter,
    ) -> Result<(), WriteError> {
        crate::election::fence(tx, aggregate, writer.epoch())?;
        Ok(())
    }

    /// Fixture-only durable injection, store-owned: the transaction opens and
    /// closes on this store's connection and the rows take the store's own
    /// one-statement lift. It exists for the counterexample states no
    /// admission would accept (see [`crate::fixture_rows`]); the
    /// write-chokepoint gate pins its consumers to the reviewed fixture
    /// files, so it cannot become a production write route.
    pub fn fixture_rows(&self) -> rusqlite::Result<crate::fixture_rows::FixtureRows<'_>> {
        crate::fixture_rows::FixtureRows::begin(&self.conn, &self.gate)
    }

    /// NORMAL commits for streaming content, block openings and usage only.
    /// Intent events and operational authority writes refuse this transaction.
    pub fn stream_transaction(&mut self) -> rusqlite::Result<GovernedTransaction<'_>> {
        self.transaction_with_policy(false)
    }

    fn transaction_with_policy(
        &mut self,
        durable: bool,
    ) -> rusqlite::Result<GovernedTransaction<'_>> {
        let timing = self.timing.clone();
        let start = timing.as_ref().map(|observer| observer.now());
        GovernedTransaction::begin_with_policy(
            &mut self.conn,
            self.gate.clone(),
            self.read_projection.clone(),
            self.commit_executor.clone(),
            durable,
        )
        .map(|mut tx| {
            tx.timing_begin = start.zip(timing.as_ref().map(|observer| observer.now()));
            tx.timing = timing;
            tx
        })
    }

    /// Append one durable event and run every projector in the SAME immediate
    /// transaction. Returns the committed sequence and the notices the commit
    /// released.
    ///
    /// The notices are returned rather than published here because the store
    /// owns no notifier: a bounded lossy fanout with subscriber interests is the
    /// event-feed surface, downstream of this lane. What this path does own is
    /// the ordering, and it cannot hand out a notice for a fact that did not
    /// commit — see [`crate::notify`].
    pub fn append(
        &mut self,
        input: AppendInput<'_>,
        projectors: &mut [&mut dyn Projector],
    ) -> Result<(Appended, CommittedNotices), WriteError> {
        let tx = self.governed_transaction()?;
        let appended = Self::append_within(&tx, input, projectors)?;
        let notices = tx.commit()?;
        Ok((appended, notices))
    }

    /// This projection's durable position against the aggregate's head.
    ///
    /// Refuses [`WriteError::WatermarkLeads`] rather than reporting a lead as
    /// ordinary lag; see [`crate::projection`] for why the read fails closed on
    /// a state this crate cannot write.
    pub fn projection_status(
        &self,
        projection: ProjectionId,
        aggregate: &AggregateId,
    ) -> Result<ProjectionStatus, WriteError> {
        crate::projection::status(&self.conn, projection, aggregate)
    }

    /// Inspects an aggregate for a lost writer's unfinished work.
    ///
    /// DETECTION ONLY, and the boundary is the point. It classifies from three
    /// durable coordinates — the elected `(owner, epoch)`, the allocated
    /// sequence cursor, and the durable head — and returns
    /// [`LostWriterState`]. It re-drives nothing, adopts nothing, quarantines
    /// nothing and writes nothing.
    ///
    /// Automatic re-drive is not merely unimplemented, it is refused. The
    /// writer epoch lives inside the envelope's canonical bytes, so re-writing
    /// a lost owner's event under the new epoch names a DIFFERENT fact and
    /// cannot reconcile as `Idempotent`
    /// (`F3B1A_RESIDUALS.md` item 9). A store that silently re-drove would be
    /// letting the new owner adopt the dead one's write.
    ///
    /// # Why this commits rather than rolls back
    ///
    /// Nothing above the commit writes, so committing persists nothing and a
    /// rollback would be equally correct today. It is a commit anyway, and
    /// deliberately: a rollback would ALSO discard a write if one ever appeared
    /// here, which would hide the exact regression R02 exists to catch. The
    /// "consumes no sequence" law is carried by the absence of a write, so the
    /// witness has to be able to see a write that appears.
    pub fn inspect_lost_writer(
        &mut self,
        aggregate: &AggregateId,
    ) -> Result<LostWriterState, WriteError> {
        let family = crate::election::family_of(aggregate);
        let identity = crate::election::identity_of(aggregate);
        let tx = self.governed_transaction()?;
        let elected = crate::election::current(&tx, aggregate)?;
        let cursor = read_sequence_cursor(&tx, family, identity)?;
        let head = crate::projection::head(&tx, aggregate)?;
        let state = crate::election::classify_lost_writer(elected, cursor, head);
        let notices = tx.commit()?;
        // An inspection names no new fact, so it publishes none.
        notices.discard();
        Ok(state)
    }

    /// Brings one lagging projection forward from durable truth.
    ///
    /// This is the recovery half of "may lag but never lead". A projection that
    /// is behind — a new projector over an existing aggregate, or one whose
    /// process died between two appends — is rebuilt by folding the events its
    /// watermark has not covered, in sequence order, advancing the watermark
    /// with each one inside ONE transaction.
    ///
    /// It appends no fact and therefore NAMES none: the returned notices are
    /// always empty. Re-projecting an already-durable event makes nothing newly
    /// observable, so publishing here would announce facts a subscriber was
    /// already entitled to have seen.
    ///
    /// Events are selected by `seq > watermark`, never `seq == watermark + 1`:
    /// within-aggregate sequences are monotonic-with-gaps, so a contiguity
    /// assumption would stall on an imported history.
    ///
    /// It reads through the same lead check the status read uses, and for the
    /// same reason. A watermark PAST the durable head selects no rows, so a
    /// recovery pass that read the position directly would report a corrupt
    /// projection as caught up and fold nothing — the one place where "may lag
    /// but never lead" would be silently discarded instead of enforced.
    pub fn catch_up(
        &mut self,
        aggregate: &AggregateId,
        projector: &mut dyn Projector,
    ) -> Result<CaughtUp, WriteError> {
        let projection = projector.projection();
        let tx = self.governed_transaction()?;
        let from = crate::projection::status(&tx, projection, aggregate)?.watermark();
        // -1 rather than 0: sequences are zero-based, so a projection that has
        // covered nothing must still see the event at sequence zero.
        let lower: i64 = match from {
            None => -1,
            Some(mark) => i64::try_from(mark).map_err(|_| WriteError::SequenceExhausted {
                aggregate: crate::election::identity_of(aggregate).to_owned(),
            })?,
        };
        // Collected before projecting: the statement borrows the transaction the
        // projector and the watermark advance both need.
        let rows: Vec<(String, i64, i64, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, seq, created, type,
                    CASE WHEN aggregate_family NOT IN ('form','mail') OR length(CAST(data AS BLOB)) <= 262144 THEN data END FROM event
                 WHERE aggregate_family = ?1 AND aggregate_id = ?2 AND seq > ?3
                 ORDER BY seq ASC LIMIT ?4",
            )?;
            let found = stmt.query_map(
                rusqlite::params![
                    crate::election::family_of(aggregate),
                    crate::election::identity_of(aggregate),
                    lower,
                    if matches!(aggregate, AggregateId::Form(_) | AggregateId::Mail(_)) {
                        129_i64
                    } else {
                        i64::MAX
                    }
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )?;
            found.collect::<Result<Vec<_>, _>>()?
        };

        if matches!(aggregate, AggregateId::Form(_) | AggregateId::Mail(_)) && rows.len() > 128 {
            return Err(crate::DecodeError::Form("global form replay row bound").into());
        }

        let mut projected = 0_u64;
        let mut watermark = from;
        for (id, seq, created, versioned_type, data_text) in rows {
            // Every row goes through the SAME closed decoder the replay path
            // uses. A projection rebuilt from rows the replay path refuses would
            // be a second, more permissive reading of durable truth.
            let event = decode_fact(
                &id,
                crate::election::family_of(aggregate),
                crate::election::identity_of(aggregate),
                seq,
                created,
                &versioned_type,
                &data_text,
            )?;
            event.project(projector, &tx).map_err(|error| {
                let name = projection.as_str().to_owned();
                match error {
                    ProjectorError::Refused(message) => WriteError::Projector { name, message },
                    ProjectorError::Storage(source) => {
                        WriteError::ProjectorStorage { name, source }
                    }
                }
            })?;
            let seq = event.seq().value();
            crate::projection::advance(&tx, projection, aggregate, seq)?;
            watermark = Some(seq);
            projected += 1;
        }
        let notices = tx.commit()?;
        Ok(CaughtUp {
            projected,
            watermark,
            notices,
        })
    }

    /// The append+project law inside a CALLER-OWNED transaction: domain
    /// decision reads and this append commit or roll back together. Does NOT
    /// commit — the caller owns the transaction boundary.
    ///
    /// The transaction is a [`GovernedTransaction`] and not a bare
    /// [`rusqlite::Transaction`] BY TYPE, so the one path that writes `event`
    /// and `event_sequence` is structurally reachable only from the chokepoint.
    /// The two governed statements below run through the transaction's own gate,
    /// one statement per lift; everything between them — the fence, the
    /// idempotency read, the cursor read and the whole projector loop — runs with
    /// the denial ON.
    pub fn append_within(
        tx: &GovernedTransaction<'_>,
        input: AppendInput<'_>,
        projectors: &mut [&mut dyn Projector],
    ) -> Result<Appended, WriteError> {
        if matches!(input.aggregate, AggregateId::Mail(_)) {
            return Err(WriteError::RemoteMailbox {
                reason: "mail progress must use the atomic page owner".into(),
            });
        }
        if input.kind_name == "session.mail.presented" {
            return Err(WriteError::RemoteMailbox {
                reason: "mail presentation must use the atomic capture owner".into(),
            });
        }
        if input.kind_name == "session.subagent-mail.presented" {
            return Err(WriteError::OperationMail {
                reason: "mail presentation must use the atomic capture owner".into(),
            });
        }
        Self::append_event_within(tx, input, projectors)
    }

    // The mailbox capture owner is the sole new-kind caller. Public generic
    // appends cannot fabricate a fact without the atomic consumption checks.
    pub(crate) fn append_captured_mail_within(
        tx: &GovernedTransaction<'_>,
        input: AppendInput<'_>,
    ) -> Result<Appended, WriteError> {
        Self::append_event_within(tx, input, &mut [])
    }

    pub(crate) fn append_mail_progress_within(
        tx: &GovernedTransaction<'_>,
        input: AppendInput<'_>,
    ) -> Result<Appended, WriteError> {
        Self::append_event_within(tx, input, &mut [])
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn append_event_within(
        tx: &GovernedTransaction<'_>,
        input: AppendInput<'_>,
        projectors: &mut [&mut dyn Projector],
    ) -> Result<Appended, WriteError> {
        tx.attribute_timing(&input.aggregate, input.kind_name);
        let _append = tx.timing_span(grapt_types::timing::TimingStage::Append);
        let family = crate::election::family_of(&input.aggregate);
        let identity = crate::election::identity_of(&input.aggregate);
        let epoch_db = i64::from(input.writer.epoch.value());

        // FENCING FIRST — before the idempotency read, before the sequence
        // cursor, before any encoded byte, projection row or return value.
        //
        // The law is "compare before allocating `EventSeq` or writing bytes",
        // so this call's POSITION is the law, not merely its presence: a fence
        // that ran after allocation would leave a consumed sequence number
        // behind for a writer that was never allowed to write. The pairing of
        // `a_stale_writer_is_refused_before_any_sequence_is_consumed` with a
        // live append proves the gap is unconsumed rather than just refused.
        crate::election::fence(tx, &input.aggregate, input.writer.epoch)?;
        tx.require_event_policy(input.kind_name)?;

        let versioned_type = format!("{}.{}", input.kind_name, input.version.value());
        if matches!(input.aggregate, AggregateId::Form(_)) {
            crate::form_fact::bound_input(input.data)?;
        }
        if input.kind_name == "session.mail.presented" {
            if input.version.value() != 1 {
                return Err(WriteError::RemoteMailbox {
                    reason: "unsupported mail presentation version".into(),
                });
            }
            grapt_event::mail_presented::MailPresented::decode(input.data)
                .map_err(|reason| WriteError::RemoteMailbox { reason })?;
        }
        if matches!(input.aggregate, AggregateId::Mail(_)) {
            crate::mailbox::remote::progress::bound_input(input.data)?;
        }
        if input.kind_name == "session.subagent-mail.presented" {
            if input.version.value() != 1 {
                return Err(WriteError::OperationMail {
                    reason: "unsupported operation mail version".into(),
                });
            }
            grapt_event::operation_mail_presented::OperationMailPresented::decode(input.data)
                .map_err(|reason| WriteError::OperationMail { reason })?;
        }
        let data_text = grapt_event::canonical_string(input.data);
        let incoming_hash = grapt_event::canonical_hash_hex(input.data);

        // Idempotent replay-conflict check by event id. Oracle law
        // (bus.ts:255-266): an exact retry compares the FULL identity —
        // aggregate, type (name+version), created, and data — not data alone
        // (review round 2, item C). The v2 identity adds two coordinates the
        // oracle had no concept of: the aggregate FAMILY, and the WRITER EPOCH
        // that sits inside the envelope's canonical bytes. An event replayed
        // under a different epoch names a different fact, so it diverges rather
        // than reconciling — reconciling it would let a new owner adopt a lost
        // owner's write as its own.
        let existing: Option<StoredRow> = tx
            .query_row(
                "SELECT aggregate_family, aggregate_id, seq, created, type, data, writer_epoch
                 FROM event WHERE id = ?1",
                [input.id.as_str()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        if let Some((stored_family, agg, seq, created, stored_type, stored_data, stored_epoch)) =
            existing
        {
            // The stored row goes through the SAME closed decoder the replay
            // path uses, BEFORE any field is compared. An unmodelled kind, a
            // known name at an unknown version, a negative sequence, oversized
            // or unparseable data is refused HERE.
            //
            // Deciding first and decoding only on a parse error — the shape
            // this replaced — consulted the kind on no path a well-formed row
            // could take, so a row carrying `session.imaginary.7` settled as
            // `Idempotent` while `decode_for_replay` refused that same row.
            // A pre-check must never settle a shape replay rejects.
            let stored = decode_fact(
                input.id.as_str(),
                &stored_family,
                agg.as_str(),
                seq,
                created,
                &stored_type,
                &stored_data,
            )?;
            // The decoder already refused every negative sequence, so the
            // separate i64 -> u64 guard that stood here could no longer fire.
            // `CorruptSequence` remains live on the `event_sequence` cursor.
            let seq = stored.seq().value();
            if stored_family == family
                && agg == identity
                && stored_type == versioned_type
                && created == input.created_ms
                && stored_epoch == Some(epoch_db)
                && grapt_event::canonical_hash_hex(stored.data()) == incoming_hash
            {
                return Ok(Appended::Idempotent { seq });
            }
            return Err(WriteError::Divergence {
                id: input.id.as_str().to_owned(),
            });
        }

        let next_seq = allocate_sequence(tx, family, identity)?;
        let next_seq_db = i64::try_from(next_seq).map_err(|_| WriteError::SequenceExhausted {
            aggregate: identity.to_owned(),
        })?;

        // Projectors run INSIDE the transaction, before the event insert —
        // the bus.ts ordering law. A projector failure rolls back everything.
        //
        // They run with the governed denial ON. The cursor lift above was
        // released by the time `execute` returned, and the event lift below is
        // not taken until after this loop, so a projector is foreign code
        // holding no governed-write capability: it can project into its own
        // tables and cannot touch the fact log or the election row.
        let event = decode_fact(
            input.id.as_str(),
            family,
            identity,
            next_seq_db,
            input.created_ms,
            &versioned_type,
            &data_text,
        )?;
        for projector in projectors.iter_mut() {
            let projection = projector.projection();
            event.project(*projector, tx).map_err(|error| {
                let name = projection.as_str().to_owned();
                match error {
                    ProjectorError::Refused(message) => WriteError::Projector { name, message },
                    ProjectorError::Storage(source) => {
                        WriteError::ProjectorStorage { name, source }
                    }
                }
            })?;
            // The watermark advances HERE — in the same transaction as the rows
            // that projector just wrote and as the event row below. Every
            // projector's position moves with the fact or with neither, which is
            // what makes "may lag but never lead" structural rather than a
            // convention the projectors are trusted to keep.
            //
            // A failing projector never reaches this line, but that is not what
            // rolls its watermark back: transaction atomicity is. See
            // `F3B2_RESIDUALS.md` — no mutation is claimed for the ordering
            // WITHIN the loop, because moving this line above the `project` call
            // is unobservable when the whole transaction is discarded either way.
            crate::projection::advance(tx, projection, &input.aggregate, next_seq)?;
        }

        tx.gate().execute(
            tx,
            GovernedTable::Event,
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                input.id.as_str(),
                family,
                identity,
                next_seq_db,
                input.created_ms,
                versioned_type,
                data_text,
                epoch_db
            ],
        )?;

        // The fact, every authoritative projection row and every watermark are
        // now written in this transaction. Only NOW is a notice minted, and it
        // is minted into the transaction — nothing is published here, and the
        // carrier that could publish it is released only by a successful commit.
        //
        // The `Idempotent` arm returned long before this point, which is what
        // keeps an exact retry from minting a second notice for one fact.
        tx.push_notice(DurableNotice::new(
            input.id.clone(),
            input.aggregate.clone(),
            next_seq,
            event.kind(),
            input.writer.epoch,
        ));
        Ok(Appended::Committed { seq: next_seq })
    }
}

// ---------------------------------------------------------------------------
// F5 JOIN COMPOSITION WITNESS
// ---------------------------------------------------------------------------
//
// The two F5 parents never met before this join. F3B4c REBUILDS `event` and
// `event_sequence` (DROP + RENAME under `PRAGMA foreign_keys = ON`), while
// F4B1b's compaction cache is a durable table that lives beside them. Nothing
// on either side proved they survive together: of the migration witnesses in
// this file, none touched the compaction cache.
//
// Structurally the rebuild cannot cascade into them, because neither table
// carries a FOREIGN KEY into `event`/`event_sequence` — but "cannot" was an
// argument, not a witness, and this is the test that makes it one.
//
// REACHABILITY, measured rather than assumed. The state cannot be written "in
// place" before the rebuild: `EventStore::open` REFUSES a pre-target database
// (`incompatible preexisting event table: the invalid-session probe row was
// ACCEPTED`), so no public API can reach a database that still needs it. The
// faithful construction is therefore to write the F4-side state through the
// real public APIs first, then put ONLY `event`/`event_sequence` back into the
// pre-target shape, which is exactly the "before the rebuild" state a
// deployment presents. The migration itself is the real one, never copied SQL.
//
// This witness is deliberately UNMUTATED: it composes two accepted mechanisms
// and mints no new law, so it carries no manifest row.

/// Puts `event`/`event_sequence` back into the pre-target shape, preserving
/// every row, so the real migration has something real to rebuild.
fn downgrade_event_tables(db: &std::path::Path) -> TestResult {
    let conn = Connection::open(db)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         BEGIN IMMEDIATE;
         ALTER TABLE event RENAME TO event_target;
         ALTER TABLE event_sequence RENAME TO event_sequence_target;
         DROP INDEX IF EXISTS event_aggregate_seq_idx;
         DROP INDEX IF EXISTS event_aggregate_type_seq_idx;
         DROP INDEX IF EXISTS event_campaign_transition_idx;
         CREATE TABLE event_sequence (
             aggregate_family TEXT NOT NULL,
             aggregate_id TEXT PRIMARY KEY,
             seq INTEGER NOT NULL,
             owner_id TEXT
         );
         CREATE TABLE event (
             id TEXT PRIMARY KEY,
             aggregate_family TEXT NOT NULL,
             aggregate_id TEXT NOT NULL,
             seq INTEGER NOT NULL,
             created INTEGER NOT NULL,
             type TEXT NOT NULL,
             data TEXT,
             writer_epoch INTEGER,
             envelope_bytes BLOB,
             campaign_transition_id TEXT,
             CONSTRAINT fk_event FOREIGN KEY (aggregate_id)
                 REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE
         );
         INSERT INTO event_sequence SELECT aggregate_family, aggregate_id, seq, owner_id
             FROM event_sequence_target;
         INSERT INTO event SELECT id, aggregate_family, aggregate_id, seq, created, type,
             data, writer_epoch, envelope_bytes, campaign_transition_id FROM event_target;
         DROP TABLE event_target;
         DROP TABLE event_sequence_target;
         CREATE UNIQUE INDEX event_aggregate_seq_idx ON event (aggregate_id, seq);
         CREATE INDEX event_aggregate_type_seq_idx ON event (aggregate_id, type, seq);
         COMMIT;",
    )?;
    Ok(())
}

/// The attempt receipt staged by the composition witness below.
const ATTEMPT: &str = "att_0336f5c0mpac1";

/// A FIXTURE bound for the attempt receipt's identities and whole record. It is
/// not a production value and names none.
fn attempt_bound() -> std::num::NonZeroU32 {
    match std::num::NonZeroU32::new(4_096) {
        Some(bound) => bound,
        None => panic!("a non-zero fixture bound"),
    }
}

fn f5_artifact(id: &str) -> Option<ArtifactRef> {
    Some(ArtifactRef::new(
        ArtifactId::new(id)?,
        7,
        ArtifactDigest::new("digest")?,
    ))
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_migrated_database_still_serves_operational_rows_and_compaction_cache() -> TestResult {
    let dir = scratch("f5-composition")?;
    let db = dir.join("scratch.db");
    let session: SessionId = "ses_033601c26ff5".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    const MAIL: &str = "msg-r2b2-migration";
    let recipient = SeatRecipientId::new("seat-r2b2").ok_or("recipient")?;

    // 1. Both F4-side facts, written through the REAL public APIs.
    {
        let mut store = EventStore::open(&db, "0.0.1")?;

        let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
            id: CompactionId::new("cmp-f5").ok_or("compaction id")?,
            source_session: session.clone(),
            source_epoch: ContextEpoch::new(1),
            target_window: ContextWindowId::new("cwn-f5").ok_or("window")?,
            target_epoch: ContextEpoch::new(2),
            trigger: CompactionTrigger::ContextOverflow,
            retention_policy: RetentionPolicyVersion::new("ret-1").ok_or("retention")?,
            summary: CompactionSummary::ClientReadable {
                artifact: f5_artifact("sum-f5").ok_or("summary artifact")?,
            },
            retained: RetainedHistory::References {
                artifacts: vec![f5_artifact("hist-f5").ok_or("history artifact")?],
            },
            baseline: f5_artifact("base-f5").ok_or("baseline artifact")?,
            next_request: NextRequestRef::new(
                StepNumber::new(1).ok_or("step")?,
                f5_artifact("req-f5").ok_or("request artifact")?,
            ),
        })
        .ok()
        .ok_or("descriptor")?;

        store.elect_writer(&aggregate)?;
        let writer = store.fencing_token(&aggregate)?;
        let event: EventId = "evt_0336f5c0mpac".parse()?;
        let tx = store.governed_transaction()?;
        grapt_store::compaction::record(
            &tx,
            &event,
            writer,
            1_786_147_200_000,
            CompactionAttempt::Settled(&descriptor),
            &mut [] as &mut [&mut dyn Projector],
        )?;
        tx.commit()?.discard();

        let tx = store.governed_transaction()?;
        let rebuilt = grapt_store::compaction::rebuild_cache(&tx, &aggregate)?;
        tx.commit()?.discard();
        assert_eq!(
            rebuilt.folded, 1,
            "the descriptor must fold into the cache before the rebuild"
        );

        let message = SeatMailboxMessage::new(
            ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
            MailboxMessageId::new(MAIL).ok_or("mail id")?,
            SeatRecipientId::new("seat-alpha").ok_or("sender")?,
            recipient.clone(),
            SeatMessageKind::TaskPointer,
            MailboxBody::new("survive the event-table rebuild")?,
            None,
            MailboxSourceCursor::new("source-r2b2-migration").ok_or("source cursor")?,
        )?;
        store.admit_mailbox_message(&recipient, &message)?;

        // R2C5A1's operational row, staged through the REAL public API and
        // carried into the ambiguity window, so a surviving row proves both its
        // canonical bytes and its indexed MARKER columns came through.
        let attempt = PhysicalAttemptId::new(ATTEMPT).ok_or("attempt id")?;
        let receipt = DurableAttemptReceipt::reserved(
            BoundedAttemptId::new(attempt.clone(), attempt_bound())
                .ok()
                .ok_or("attempt id fits")?,
            BoundedSessionId::new(session.clone(), attempt_bound())
                .ok()
                .ok_or("session id fits")?,
            StepNumber::new(5).ok_or("step")?,
            AttemptOrdinal::new(1).ok_or("ordinal")?,
            ReservedMarker::new(Timestamp::from_millis(1_786_147_200_002)),
        );
        let terminal = TerminalCoordinates::new("evt_0336f5c0term1".parse()?, 1_786_147_200_004);
        let writer = store.fencing_token(&aggregate)?;
        store.reserve_attempt_receipt(&ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling: AttemptRecordCeiling::new(attempt_bound()),
        })?;
        let writer = store.fencing_token(&aggregate)?;
        let tx = store.governed_transaction()?;
        EventStore::permit_attempt_send_within(
            &tx,
            &attempt,
            writer,
            AttemptRecordCeiling::new(attempt_bound()),
            RequestSentMarker::new(Timestamp::from_millis(1_786_147_200_003)),
        )?;
        tx.commit()?.discard();
    }

    // 2. THE PRECONDITION, asserted numerically: both rows really are present
    //    before the rebuild, so a green result cannot come from an empty table.
    {
        let conn = Connection::open(&db)?;
        let windows: i64 =
            conn.query_row("SELECT COUNT(*) FROM compaction_window_cache", [], |r| {
                r.get(0)
            })?;
        let mailbox_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM mailbox_message", [], |r| r.get(0))?;
        assert_eq!(
            windows, 1,
            "one cached window must exist before the rebuild"
        );
        assert_eq!(
            mailbox_rows, 1,
            "one mailbox row must exist before the event-table rebuild"
        );
        let attempt_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM attempt_receipt", [], |r| r.get(0))?;
        assert_eq!(
            attempt_rows, 1,
            "one attempt receipt must exist before the event-table rebuild"
        );
    }

    // 3. Put the event tables back to the pre-target shape and run the REAL
    //    migration over them.
    downgrade_event_tables(&db)?;
    migrated(&db, &dir);

    // 4. Both surfaces must still answer through their REAL public APIs.
    let store = opened(&db);

    let window = match grapt_store::compaction::cached_window(store.connection(), &aggregate) {
        Ok(Some(window)) => window,
        Ok(None) => panic!(
            "the compaction cache was LOST by the event/event_sequence rebuild: cached_window \
             returned None where one window was cached before it"
        ),
        Err(error) => panic!("the compaction cache became unreadable after the rebuild: {error}"),
    };
    assert_eq!(
        window.window().as_str(),
        "cwn-f5",
        "the cached window must still be the one folded before the rebuild"
    );
    assert_eq!(
        window.epoch(),
        ContextEpoch::new(2),
        "the cached window's epoch must survive the rebuild unchanged"
    );

    let mailbox = match store.read_actionable_mailbox(&recipient, MailboxCursor::START, 16) {
        Ok(page) => page,
        Err(error) => panic!(
            "the mailbox became unreadable after the event-table rebuild: {error}. The mailbox \
             is an independent governed operational table without an event foreign key"
        ),
    };
    assert_eq!(mailbox.entries.len(), 1, "the admitted row survives");
    match &mailbox.entries[..] {
        [MailboxPageEntry::Message(message)] => {
            assert_eq!(message.id().as_str(), MAIL, "exact durable identity");
            assert_eq!(
                message.body().as_str(),
                "survive the event-table rebuild",
                "strict decode"
            );
        }
        rows => panic!("the surviving mailbox must be exactly the admitted message: {rows:?}"),
    }

    // R2C5A1: the attempt receipt carries no FOREIGN KEY into
    // event/event_sequence either, so the forced rebuild must not reach it.
    // "Cannot" was an argument; this is the measurement. Read through the REAL
    // public API, so a surviving row must still pass its own strict decode and
    // every one of its column-versus-bytes checks.
    let attempt = match PhysicalAttemptId::new(ATTEMPT) {
        Some(attempt) => attempt,
        None => panic!("a non-empty fixture attempt id"),
    };
    let surviving =
        match store.attempt_receipt(&attempt, AttemptRecordCeiling::new(attempt_bound())) {
            Ok(Some(record)) => record,
            Ok(None) => panic!(
            "the attempt receipt was LOST by the event/event_sequence rebuild: the table carries \
             no event foreign key and must be outside the rebuild surface entirely"
        ),
            Err(error) => panic!(
                "the attempt receipt became unreadable after the rebuild: {error}. Its canonical \
             bytes and every indexed column must come through untouched"
            ),
        };
    assert_eq!(
        surviving.receipt().step().value(),
        5,
        "the surviving receipt must retain the exact coordinates it was reserved with"
    );
    assert_eq!(
        surviving.terminal().event().as_str(),
        "evt_0336f5c0term1",
        "and the exact terminal identity reserved before the provider opened -          losing it would leave a recovering process unable to prove or reproduce          the terminal at all"
    );
    assert_eq!(surviving.terminal().created_ms(), 1_786_147_200_004);
    assert_eq!(
        surviving.receipt().disposition(),
        ReceiptDisposition::Lost(LostEffect::Ambiguous {
            sent_at: Timestamp::from_millis(1_786_147_200_003)
        }),
        "the marker written before the rebuild must still be there. A lost \
         request-sent marker would silently turn an ambiguous external effect \
         into a replayable one, which is the exact misclassification this lane \
         exists to prevent"
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

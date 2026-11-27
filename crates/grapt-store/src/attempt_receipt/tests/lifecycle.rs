use super::*;

// -----------------------------------------------------------------------
// Positive witness
// -----------------------------------------------------------------------

/// The lane's positive law, end to end: reserve carrying its terminal
/// coordinates, a send PERMIT, then one atomic settlement that appends the
/// exact terminal and fills the settled marker together. Columns and bytes
/// are asserted separately, because a write path that authored only the
/// record would leave the columns stale and the agreement check would then
/// be comparing bytes with themselves.
#[test]
fn a_receipt_round_trips_through_its_columns_and_its_bytes() -> TestResult {
    let mut store = match elected_store() {
        Ok(store) => store,
        Err(error) => panic!(
            "a store must open with the attempt-receipt tables chained into \
             its schema in the same transaction as the fact log: {error}"
        ),
    };
    let receipt = reserved_receipt(ATTEMPT, 3);
    match reserve(&mut store, &receipt) {
        Ok(AttemptReceiptReserved::Reserved) => {}
        other => panic!("reserving on a fresh store must write the row, got {other:?}"),
    }
    let id = attempt_id(ATTEMPT);

    let stored = match read(&store, &id) {
        Ok(Some(stored)) => stored,
        other => panic!("the reserved row must be readable, got {other:?}"),
    };
    assert_eq!(stored.writer_epoch(), WriterEpoch::FIRST);
    assert_eq!(stored.terminal().event().as_str(), TERMINAL);
    assert_eq!(stored.terminal().created_ms(), TERMINAL_MS);
    assert_eq!(
        stored.receipt().disposition(),
        ReceiptDisposition::Lost(LostEffect::NoneInitiated)
    );

    assert_eq!(permit(&mut store, &id, PERMIT_MS)?, MarkerOutcome::Recorded);
    let stored = match read(&store, &id) {
        Ok(Some(stored)) => stored,
        Err(error) => panic!(
            "after a permit the row must still agree with its own bytes - the \
             column and the record are written by ONE statement: {error}"
        ),
        Ok(None) => panic!("the row survives its permit"),
    };
    assert_eq!(
        stored.receipt().disposition(),
        ReceiptDisposition::Lost(LostEffect::Ambiguous {
            sent_at: Timestamp::from_millis(PERMIT_MS)
        }),
        "a committed send permit is ambiguous, never none-initiated"
    );

    assert_eq!(settle(&mut store, &id)?, AttemptSettlement::Settled);
    let stored = match read(&store, &id) {
        Ok(Some(stored)) => stored,
        other => panic!("the row survives settlement, got {other:?}"),
    };
    assert_eq!(
        stored.receipt().disposition(),
        ReceiptDisposition::AlreadySettled
    );

    let row = columns(&store, ATTEMPT)?;
    assert_eq!(row.session_id, SESSION);
    assert_eq!(row.step, 3);
    assert_eq!(row.ordinal, 1);
    assert_eq!(row.writer_epoch, 1);
    assert_eq!(row.terminal_event_id, TERMINAL);
    assert_eq!(row.terminal_created_ms, TERMINAL_MS);
    assert_eq!(row.reserved_ms, RESERVED_MS);
    assert_eq!(row.request_sent_ms, Some(PERMIT_MS));
    assert_eq!(row.settled_ms, Some(SETTLED_MS));
    Ok(())
}

/// The property that makes the reserve-time ceiling structural: adding a
/// marker never grows the record, so the one size decision already budgets
/// every later write.
#[test]
fn encoded_len_is_marker_invariant() {
    let reserved = reserved_receipt(ATTEMPT, 3);
    let at_reserve = encode_record(&AttemptReceiptRecord::new(
        reserved.clone(),
        WriterEpoch::FIRST,
        terminal(),
    ))
    .len();
    let (sent, _) =
        reserved.mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(PERMIT_MS)));
    let after_permit = encode_record(&AttemptReceiptRecord::new(
        sent.clone(),
        WriterEpoch::FIRST,
        terminal(),
    ))
    .len();
    let (settled, _) = sent.mark_settled(SettledMarker::new(Timestamp::from_millis(SETTLED_MS)));
    let after_settled = encode_record(&AttemptReceiptRecord::new(
        settled,
        WriterEpoch::FIRST,
        terminal(),
    ))
    .len();

    assert_eq!(
        at_reserve, after_permit,
        "the permit must not grow the record"
    );
    assert_eq!(
        at_reserve, after_settled,
        "settling must not grow the record"
    );
    assert_eq!(
        at_reserve,
        FIXED_RECORD_BYTES + ATTEMPT.len() + SESSION.len() + TERMINAL.len(),
        "the encoded length is the fixed part plus the three identities"
    );
}

// -----------------------------------------------------------------------
// Reserve
// -----------------------------------------------------------------------

#[test]
fn an_exact_reserve_retry_reconciles_and_a_conflicting_one_refuses() -> TestResult {
    let mut store = elected_store()?;
    let receipt = reserved_receipt(ATTEMPT, 3);
    reserve(&mut store, &receipt)?;
    match reserve(&mut store, &receipt) {
        Ok(AttemptReceiptReserved::Idempotent) => {}
        other => panic!("byte-identical canonical data must reconcile, got {other:?}"),
    }
    match reserve(&mut store, &reserved_receipt(ATTEMPT, 4)) {
        Err(WriteError::ReceiptConflict { attempt }) => assert_eq!(attempt, ATTEMPT),
        other => {
            panic!("a different record under a used attempt id must refuse, got {other:?}")
        }
    }
    assert_eq!(
        columns(&store, ATTEMPT)?.step,
        3,
        "the held row is untouched"
    );
    Ok(())
}

/// A DIFFERENT terminal identity under the same attempt id is a conflict
/// too. The coordinates are part of the record, so they cannot be swapped
/// underneath a reservation.
#[test]
fn a_reserve_that_changes_the_terminal_identity_refuses() -> TestResult {
    let mut store = elected_store()?;
    let receipt = reserved_receipt(ATTEMPT, 3);
    reserve(&mut store, &receipt)?;
    match reserve_with(
        &mut store,
        &receipt,
        &terminal_of("evt_033602b2tfff"),
        wide(),
    ) {
        Err(WriteError::ReceiptConflict { attempt }) => {
            assert_eq!(attempt, ATTEMPT);
            assert_eq!(columns(&store, ATTEMPT)?.terminal_event_id, TERMINAL);
            Ok(())
        }
        other => panic!("a swapped terminal identity must refuse, got {other:?}"),
    }
}

#[test]
fn an_oversize_record_refuses_at_reserve_before_any_row_exists() -> TestResult {
    let mut store = elected_store()?;
    let receipt = reserved_receipt(ATTEMPT, 3);
    let exact = encode_record(&AttemptReceiptRecord::new(
        receipt.clone(),
        WriterEpoch::FIRST,
        terminal(),
    ))
    .len();
    let too_small = match u32::try_from(exact) {
        Ok(exact) if exact > 1 => exact - 1,
        _ => panic!("the fixture record is longer than one byte"),
    };
    match reserve_with(&mut store, &receipt, &terminal(), ceiling(too_small)) {
        Err(WriteError::ReceiptRecordTooLarge {
            attempt,
            bytes,
            ceiling,
        }) => {
            assert_eq!(attempt, ATTEMPT);
            assert_eq!(bytes, exact);
            assert_eq!(ceiling, too_small);
        }
        other => panic!("a record above the ceiling must refuse at reserve, got {other:?}"),
    }
    let rows: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM attempt_receipt", [], |r| r.get(0))?;
    assert_eq!(rows, 0, "the size refusal happens before any I/O");
    Ok(())
}

// -----------------------------------------------------------------------
// The send permit
// -----------------------------------------------------------------------

/// O1, durably: an exact retry is idempotent, a DIFFERENT instant is a typed
/// conflict carrying both values, and the held marker survives in the ROW.
#[test]
fn an_exact_permit_retry_is_idempotent_and_a_different_instant_conflicts() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let id = attempt_id(ATTEMPT);

    assert_eq!(permit(&mut store, &id, PERMIT_MS)?, MarkerOutcome::Recorded);
    assert_eq!(
        permit(&mut store, &id, PERMIT_MS)?,
        MarkerOutcome::ExactRetry,
        "the same instant re-applied after a crashed write is normal"
    );
    match permit(&mut store, &id, PERMIT_MS + 500)? {
        MarkerOutcome::Conflict { held, offered } => {
            assert_eq!(held, Timestamp::from_millis(PERMIT_MS));
            assert_eq!(offered, Timestamp::from_millis(PERMIT_MS + 500));
        }
        other => panic!("a different instant must be a typed conflict, got {other:?}"),
    }
    assert_eq!(
        columns(&store, ATTEMPT)?.request_sent_ms,
        Some(PERMIT_MS),
        "the held value is preserved in the row, not merely in the outcome"
    );
    Ok(())
}

/// A permit for an attempt nobody reserved refuses. Reserve comes FIRST, and
/// this is that ordering: a permit that created its own row would authorise
/// a call for an attempt that was never admitted to make one.
#[test]
fn a_permit_for_an_unreserved_attempt_refuses() -> TestResult {
    let mut store = elected_store()?;
    match permit(&mut store, &attempt_id("att_never"), PERMIT_MS) {
        Err(WriteError::ReceiptNotReserved { attempt }) => {
            assert_eq!(attempt, "att_never");
            Ok(())
        }
        other => panic!("an unreserved attempt must refuse, got {other:?}"),
    }
}

/// A permit carries NO ceiling of its own and therefore has no size refusal
/// to make: it re-encodes a record of exactly the reserved length.
#[test]
fn a_permit_makes_no_size_decision() -> TestResult {
    let mut store = elected_store()?;
    let receipt = reserved_receipt(ATTEMPT, 3);
    let exact = encode_record(&AttemptReceiptRecord::new(
        receipt.clone(),
        WriterEpoch::FIRST,
        terminal(),
    ))
    .len();
    reserve(&mut store, &receipt)?;

    let tight = ceiling(match u32::try_from(exact) {
        Ok(exact) => exact,
        Err(_) => panic!("the fixture record fits a u32"),
    });
    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    let outcome = EventStore::permit_attempt_send_within(
        &tx,
        &attempt_id(ATTEMPT),
        writer,
        tight,
        RequestSentMarker::new(Timestamp::from_millis(PERMIT_MS)),
    )?;
    tx.commit()?.discard();
    assert_eq!(outcome, MarkerOutcome::Recorded);
    assert_eq!(
        columns(&store, ATTEMPT)?.record_bytes.len(),
        exact,
        "the record did not grow, so no later write can breach the ceiling"
    );
    Ok(())
}

// -----------------------------------------------------------------------
// Atomic recovery settlement
// -----------------------------------------------------------------------

/// The corrected composition: ONE call appends the exact terminal AND fills
/// the settled marker. Neither exists without the other, and the terminal
/// carries the coordinates reserved before the provider was ever opened.
#[test]
fn settlement_appends_the_exact_terminal_and_fills_settled_together() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let id = attempt_id(ATTEMPT);
    permit(&mut store, &id, PERMIT_MS)?;

    assert_eq!(terminal_rows(&store)?, 0, "no terminal before settlement");
    assert_eq!(settle(&mut store, &id)?, AttemptSettlement::Settled);
    assert_eq!(terminal_rows(&store)?, 1, "exactly one terminal");

    let (id_col, created): (String, i64) = store.connection().query_row(
        "SELECT id, created FROM event WHERE type = 'session.step.failed.2'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        id_col, TERMINAL,
        "the terminal uses the identity reserved before the provider opened"
    );
    assert_eq!(created, TERMINAL_MS, "and its exact reserved instant");
    assert_eq!(columns(&store, ATTEMPT)?.settled_ms, Some(SETTLED_MS));
    Ok(())
}

/// A permit that was committed makes the loss AMBIGUOUS, and the terminal
/// records exactly that — never `none-initiated`, which would be safe to
/// replay.
#[test]
fn a_permitted_attempt_settles_as_ambiguous_and_never_replayable() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let id = attempt_id(ATTEMPT);
    permit(&mut store, &id, PERMIT_MS)?;
    settle(&mut store, &id)?;

    let data: String = store.connection().query_row(
        "SELECT data FROM event WHERE type = 'session.step.failed.2'",
        [],
        |r| r.get(0),
    )?;
    assert!(
        data.contains("\"kind\":\"ambiguous\""),
        "a permitted attempt is ambiguous, got: {data}"
    );
    assert!(
        !data.contains("none-initiated"),
        "a permitted attempt must never settle as none-initiated: {data}"
    );
    assert!(
        data.contains("\"evidence\""),
        "ambiguity must carry evidence: {data}"
    );
    Ok(())
}

/// A reserved-only attempt settles as `none-initiated` — the only state from
/// which a replay is safe.
#[test]
fn a_reserved_only_attempt_settles_as_none_initiated() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    settle(&mut store, &attempt_id(ATTEMPT))?;
    let data: String = store.connection().query_row(
        "SELECT data FROM event WHERE type = 'session.step.failed.2'",
        [],
        |r| r.get(0),
    )?;
    assert!(
        data.contains("none-initiated"),
        "nothing left the process, got: {data}"
    );
    Ok(())
}

/// The recorded effect is A0's OWN projection, not a second rule. Pinned so
/// the durable encoding and `LostEffect::into_effect_state` cannot drift.
#[test]
fn the_recorded_effect_matches_a0s_own_projection() {
    let receipt = reserved_receipt(ATTEMPT, 3);
    let (permitted, _) =
        receipt.mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(PERMIT_MS)));

    for state in [reserved_receipt(ATTEMPT, 3), permitted] {
        let lost = match state.disposition() {
            ReceiptDisposition::Lost(lost) => lost,
            other => panic!("an unsettled receipt is lost, got {other:?}"),
        };
        let mine = match lost {
            LostEffect::NoneInitiated => ExternalEffectState::NoneInitiated,
            LostEffect::Ambiguous { sent_at } => ExternalEffectState::Ambiguous {
                evidence: match grapt_types::AmbiguityEvidence::new(
                    state.describe_ambiguity(sent_at),
                ) {
                    Some(evidence) => evidence,
                    None => panic!("the description is never empty"),
                },
            },
        };
        let a0 = lost.into_effect_state(|sent_at| {
            match grapt_types::AmbiguityEvidence::new(state.describe_ambiguity(sent_at)) {
                Some(evidence) => evidence,
                None => panic!("the description is never empty"),
            }
        });
        assert_eq!(
            mine, a0,
            "the settlement path must record exactly A0's projection"
        );
        assert_eq!(mine.safe_to_replay(), a0.safe_to_replay());
    }
}

/// Settling twice is idempotent and appends NO second terminal: a recovery
/// pass that crashed after committing must be able to re-run.
#[test]
fn settlement_is_idempotent_and_appends_no_second_terminal() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let id = attempt_id(ATTEMPT);
    assert_eq!(settle(&mut store, &id)?, AttemptSettlement::Settled);
    assert_eq!(
        settle(&mut store, &id)?,
        AttemptSettlement::AlreadySettled,
        "a second pass must reconcile"
    );
    assert_eq!(
        terminal_rows(&store)?,
        1,
        "and must not append a second terminal"
    );
    Ok(())
}

/// The state R2C5I finding 2 named: settled with NO terminal. It can no
/// longer be REACHED through the production API — there is no standalone
/// settled writer — so it is staged hostilely, and the settlement path
/// refuses it loudly instead of reporting a completion nobody can point at.
#[test]
fn a_settled_row_without_its_terminal_refuses() -> TestResult {
    let mut store = elected_store()?;
    let (settled, _) = reserved_receipt(ATTEMPT, 3)
        .mark_settled(SettledMarker::new(Timestamp::from_millis(SETTLED_MS)));
    let bytes = encode_record(&AttemptReceiptRecord::new(
        settled,
        WriterEpoch::FIRST,
        terminal(),
    ));
    stage(
        &store,
        ForeignAttemptReceipt {
            settled_ms: Some(SETTLED_MS),
            ..base_row(&bytes)
        },
    )?;
    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    match settle_process_loss_within(
        &tx,
        &attempt_id(ATTEMPT),
        writer,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
        &mut [],
    ) {
        Err(WriteError::ReceiptTerminalUnproven { detail, .. }) => {
            assert!(
                detail.contains("no such durable event"),
                "a settled row whose terminal is absent must refuse, got: {detail}"
            );
            Ok(())
        }
        other => panic!("settled-without-terminal must refuse, got {other:?}"),
    }
}

// -----------------------------------------------------------------------

#[test]
fn an_ordinary_success_terminal_is_settled_and_reclaimed_atomically() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    append_l4b2_ended_terminal(&mut store, TERMINAL, TERMINAL_MS, 3)?;

    let id = attempt_id(ATTEMPT);
    assert_eq!(
        settle_ordinary(&mut store, &id)?,
        AttemptReceiptReclaimed::Reclaimed
    );
    assert!(read(&store, &id)?.is_none(), "the settled receipt is gone");
    assert_eq!(
        ordinary_terminal_rows(&store)?,
        1,
        "the proof remains exactly once"
    );
    Ok(())
}

/// The same closed proof roster serves ordinary failures. A StepFailed is an
/// ordinary terminal here — explicitly not ProcessLoss manufacture.
#[test]
fn an_ordinary_failure_terminal_is_settled_and_reclaimed_atomically() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    append_failed_terminal(&mut store, TERMINAL, TERMINAL_MS, 3)?;

    let id = attempt_id(ATTEMPT);
    assert_eq!(
        settle_ordinary(&mut store, &id)?,
        AttemptReceiptReclaimed::Reclaimed
    );
    assert!(read(&store, &id)?.is_none());
    assert_eq!(ordinary_terminal_rows(&store)?, 1);
    Ok(())
}

/// Proof precedes every receipt mutation. A missing named terminal refuses,
/// leaves the receipt unsettled, and therefore cannot create settled work
/// with no visible terminal.
#[test]
fn ordinary_settlement_refuses_a_missing_terminal_before_the_marker() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    match EventStore::settle_attempt_ordinary_terminal_within(
        &tx,
        &attempt_id(ATTEMPT),
        writer,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
    ) {
        Err(WriteError::ReceiptTerminalUnproven { detail, .. }) => {
            assert!(detail.contains("no such durable event"), "{detail}");
        }
        other => panic!("missing terminal must refuse settlement, got {other:?}"),
    }
    drop(tx);
    assert_eq!(
        columns(&store, ATTEMPT)?.settled_ms,
        None,
        "a refused settlement writes no trusted marker"
    );
    Ok(())
}

/// A real success terminal for ANOTHER Step cannot settle this receipt, even
/// though its event identity and session happen to fit.
#[test]
fn ordinary_settlement_binds_the_receipts_exact_step() -> TestResult {
    let mut store = elected_store()?;
    append_l4b2_ended_terminal(&mut store, TERMINAL, TERMINAL_MS, 9)?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    match EventStore::settle_attempt_ordinary_terminal_within(
        &tx,
        &attempt_id(ATTEMPT),
        writer,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
    ) {
        Err(WriteError::ReceiptTerminalUnproven { detail, .. }) => {
            assert!(detail.contains("settles step 9"), "{detail}");
        }
        other => panic!("a foreign Step terminal must refuse, got {other:?}"),
    }
    drop(tx);
    assert_eq!(columns(&store, ATTEMPT)?.settled_ms, None);
    Ok(())
}

/// A lost owner still holds a token whose epoch agrees with the receipt it
/// reserved. The row-level lineage comparison therefore cannot detect that a
/// successor has taken the aggregate: the election fence must route the call
/// to `StaleWriter` before the ordinary settlement touches the receipt.
#[test]
fn a_lost_writer_cannot_settle_an_ordinary_terminal() -> TestResult {
    let dir =
        std::env::temp_dir().join(format!("grapt-l4b2-ordinary-fence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("scratch.db");
    rusqlite::Connection::open(&db)?.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY)")?;

    let aggregate = AggregateId::Session(session());
    let mut lost = EventStore::open(&db, "0.0.1")?;
    let mut successor = EventStore::open(&db, "0.0.1")?;
    assert_ne!(lost.owner(), successor.owner(), "two stores are two owners");
    lost.elect_writer(&aggregate)?;
    reserve(&mut lost, &reserved_receipt(ATTEMPT, 3))?;
    append_l4b2_ended_terminal(&mut lost, TERMINAL, TERMINAL_MS, 3)?;
    let stale = token(&lost)?;
    successor.elect_writer(&aggregate)?;

    let tx = lost.governed_transaction()?;
    let refused = EventStore::settle_attempt_ordinary_terminal_within(
        &tx,
        &attempt_id(ATTEMPT),
        stale,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
    );
    assert!(
        matches!(
            refused,
            Err(WriteError::Election(ElectionError::StaleWriter {
                submitted: 1,
                elected: 2,
                ..
            }))
        ),
        "ordinary settlement must route a lost owner to StaleWriter, got {refused:?}"
    );
    drop(tx);
    assert_eq!(
        columns(&successor, ATTEMPT)?.settled_ms,
        None,
        "the refused lost owner leaves the receipt unsettled"
    );
    assert_eq!(ordinary_terminal_rows(&successor)?, 1);

    drop(lost);
    drop(successor);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Commit is the exact-once boundary. A second recovery pass sees absence,
/// does not resurrect the receipt, and leaves the one terminal intact.
#[test]
fn a_second_ordinary_settlement_is_idempotent_and_resurrects_nothing() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    append_l4b2_ended_terminal(&mut store, TERMINAL, TERMINAL_MS, 3)?;
    let id = attempt_id(ATTEMPT);
    assert_eq!(
        settle_ordinary(&mut store, &id)?,
        AttemptReceiptReclaimed::Reclaimed
    );

    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    let outcome = EventStore::settle_attempt_ordinary_terminal_within(
        &tx,
        &id,
        writer,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
    )?;
    tx.commit()?.discard();
    assert_eq!(
        outcome,
        AttemptReceiptReclaimed::AlreadyAbsent,
        "absence is typed idempotence, not a fresh completion"
    );
    assert!(read(&store, &id)?.is_none());
    assert_eq!(ordinary_terminal_rows(&store)?, 1);
    Ok(())
}

// -----------------------------------------------------------------------
// Retention
// -----------------------------------------------------------------------

#[test]
fn an_unsettled_receipt_refuses_deletion() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let id = attempt_id(ATTEMPT);
    permit(&mut store, &id, PERMIT_MS)?;

    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    match reclaim_within(&tx, &id, writer, wide()) {
        Err(WriteError::ReceiptUnsettledDelete { attempt }) => assert_eq!(attempt, ATTEMPT),
        other => panic!("an unsettled receipt must refuse deletion, got {other:?}"),
    }
    drop(tx);
    assert!(read(&store, &id)?.is_some(), "the row survives the refusal");
    Ok(())
}

#[test]
fn reclaiming_an_absent_receipt_is_idempotent() -> TestResult {
    let mut store = elected_store()?;
    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    match reclaim_within(&tx, &attempt_id("att_gone"), writer, wide()) {
        Ok(AttemptReceiptReclaimed::AlreadyAbsent) => Ok(()),
        other => panic!("an absent row must reconcile idempotently, got {other:?}"),
    }
}

/// Settle then reclaim: the whole retention cycle, with the reclaim proving
/// the terminal the RECEIPT names rather than one a caller supplied.
#[test]
fn a_settled_receipt_is_reclaimed_against_its_own_terminal() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let id = attempt_id(ATTEMPT);
    settle(&mut store, &id)?;

    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    let outcome = reclaim_within(&tx, &id, writer, wide())?;
    tx.commit()?.discard();
    assert_eq!(outcome, AttemptReceiptReclaimed::Reclaimed);
    assert!(
        read(&store, &id)?.is_none(),
        "the reclaimed receipt is gone"
    );
    assert_eq!(terminal_rows(&store)?, 1, "the terminal remains authority");
    Ok(())
}

/// Crash cut 4, and the atomicity that makes settlement ONE outcome: a
/// transaction rolled back after a successful settlement leaves NEITHER the
/// terminal nor the settled marker. Structure-carried, so no mutation row.
#[test]
fn a_rolled_back_settlement_leaves_neither_the_terminal_nor_the_marker() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    let id = attempt_id(ATTEMPT);
    {
        let writer = token(&store)?;
        let tx = store.governed_transaction()?;
        let outcome = settle_process_loss_within(
            &tx,
            &id,
            writer,
            wide(),
            SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
            &mut [],
        )?;
        assert_eq!(outcome, AttemptSettlement::Settled);
        // Dropped without committing.
    }
    assert_eq!(terminal_rows(&store)?, 0, "the terminal rolled back");
    assert_eq!(
        columns(&store, ATTEMPT)?.settled_ms,
        None,
        "and the marker rolled back with it, so neither is durable alone"
    );
    Ok(())
}

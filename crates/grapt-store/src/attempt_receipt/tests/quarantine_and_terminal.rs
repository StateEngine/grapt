use super::*;
// -----------------------------------------------------------------------
// Durable quarantine
// -----------------------------------------------------------------------

/// PANICS on refusal rather than returning it. A quarantine that could not
/// verify its own copy reds through here, and an `Err` carried out by `?`
/// would leave the mutation harness no `panicked at` line at all — the row
/// would read as "failed without a captured diagnostic" and could not be
/// told apart from any other such row.
fn quarantine(
    store: &mut EventStore,
    attempt: &PhysicalAttemptId,
    reason: QuarantineReason,
) -> Result<ReceiptQuarantined, WriteError> {
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    let outcome = match EventStore::quarantine_attempt_receipt_within(
        &tx,
        attempt,
        writer,
        reason,
        Timestamp::from_millis(SETTLED_MS),
    ) {
        Ok(outcome) => outcome,
        Err(WriteError::ReceiptQuarantineConflict { .. }) => {
            // The one refusal a caller legitimately inspects.
            drop(tx);
            return EventStore::quarantine_attempt_receipt_within(
                &store.governed_transaction()?,
                attempt,
                writer,
                reason,
                Timestamp::from_millis(SETTLED_MS),
            );
        }
        Err(error) => panic!(
            "the quarantine must move the row and preserve its exact raw                  record; the transfer refused instead: {error}"
        ),
    };
    tx.commit()?.discard();
    Ok(outcome)
}

/// The move preserves the RAW record for audit and removes the live row,
/// and it does so without this process ever holding the blob.
#[test]
fn quarantine_moves_the_row_and_preserves_the_raw_record() -> TestResult {
    let mut store = elected_store()?;
    let corrupt: &[u8] = b"not an attempt receipt";
    stage(
        &store,
        ForeignAttemptReceipt {
            record_bytes: corrupt,
            ..base_row(corrupt)
        },
    )?;
    let id = attempt_id(ATTEMPT);
    assert_eq!(
        quarantine(&mut store, &id, QuarantineReason::Undecodable)?,
        ReceiptQuarantined::Quarantined
    );

    let live: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM attempt_receipt", [], |r| r.get(0))?;
    assert_eq!(live, 0, "the live row is gone");

    let (bytes, reason, declared, epoch, session_id, terminal_id): (
        Vec<u8>,
        String,
        i64,
        i64,
        String,
        String,
    ) = store.connection().query_row(
        "SELECT record_bytes, reason, declared_bytes, detected_epoch, session_id,
                terminal_event_id
           FROM attempt_receipt_quarantine WHERE attempt_id = ?1",
        [ATTEMPT],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    assert_eq!(
        bytes, corrupt,
        "the exact corrupt bytes are preserved for audit"
    );
    assert_eq!(reason, "undecodable");
    assert_eq!(declared, corrupt.len() as i64);
    assert_eq!(epoch, 1, "the detecting epoch is recorded");
    assert_eq!(session_id, SESSION, "identity travels with it");
    assert_eq!(terminal_id, TERMINAL);
    Ok(())
}

/// Quarantining twice is idempotent; a CONFLICTING record refuses loudly
/// rather than overwriting the first durable report of a corruption.
#[test]
fn quarantine_is_idempotent_and_a_conflict_refuses() -> TestResult {
    let mut store = elected_store()?;
    let corrupt: &[u8] = b"not an attempt receipt";
    stage(
        &store,
        ForeignAttemptReceipt {
            record_bytes: corrupt,
            ..base_row(corrupt)
        },
    )?;
    let id = attempt_id(ATTEMPT);
    quarantine(&mut store, &id, QuarantineReason::Undecodable)?;

    // Re-staged live row with the same bytes: the interrupted move finishes.
    stage(
        &store,
        ForeignAttemptReceipt {
            record_bytes: corrupt,
            ..base_row(corrupt)
        },
    )?;
    assert_eq!(
        quarantine(&mut store, &id, QuarantineReason::Undecodable)?,
        ReceiptQuarantined::AlreadyQuarantined
    );

    // A DIFFERENT reason for the same attempt is a conflict, not a rewrite.
    stage(
        &store,
        ForeignAttemptReceipt {
            record_bytes: corrupt,
            ..base_row(corrupt)
        },
    )?;
    match quarantine(&mut store, &id, QuarantineReason::OverCeiling) {
        Err(WriteError::ReceiptQuarantineConflict {
            attempt,
            held,
            offered,
        }) => {
            assert_eq!(attempt, ATTEMPT);
            assert_eq!(held, "undecodable");
            assert_eq!(offered, "over-ceiling");
            Ok(())
        }
        other => panic!("a conflicting quarantine must refuse, got {other:?}"),
    }
}

/// The index must SERVE the recovery scan, not merely exist in the declared
/// shape.
///
/// The settled-inclusive index is chosen only after `EXPLAIN QUERY PLAN` and
/// the exact query shape prove it, so this asks SQLite rather than reasoning
/// about column order in prose. The
/// previous `(settled_ms, session_id, attempt_id)` index passed the open-time
/// shape check while the corrected query never constrained its leading
/// column — a shape check cannot catch that, because the shape was exactly
/// what had been declared.
///
/// Two things are asserted, and the second is the one that matters: the plan
/// names this index, and it contains no `TEMP B-TREE` — SQLite reports a
/// sort that way, and a sort means it walked rows it should never have read.
#[test]
fn the_recovery_scan_uses_its_index_rather_than_scanning() -> TestResult {
    let store = elected_store()?;
    let conn = store.connection();
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT attempt_id, session_id, step, ordinal, writer_epoch,
                    terminal_event_id, terminal_created_ms,
                    reserved_ms, request_sent_ms, settled_ms,
                    length(record_bytes),
                    CASE WHEN length(record_bytes) <= ?3 THEN record_bytes ELSE NULL END
               FROM attempt_receipt
              WHERE session_id = ?1 AND attempt_id > ?2
              ORDER BY attempt_id ASC LIMIT 1",
        )?
        .query_map(rusqlite::params![SESSION, "", 4_096_i64], |r| {
            r.get::<_, String>(3)
        })?
        .collect::<Result<_, _>>()?;
    let plan = plan.join(" | ");

    assert!(
        plan.contains("attempt_receipt_recovery"),
        "the recovery scan must use its own index, plan was: {plan}"
    );
    assert!(
        !plan.to_uppercase().contains("TEMP B-TREE"),
        "the scan must not sort: a sort means rows were read to be discarded. Plan: {plan}"
    );
    Ok(())
}

/// The retry path a COMPLETED move actually takes.
///
/// `quarantine_is_idempotent_and_a_conflict_refuses` re-stages a live row
/// before every retry, so it only ever exercises the interrupted-move
/// branch. The common case is the opposite: the move succeeded, the source
/// row is gone, and the retry finds only the quarantine row. That branch
/// once compared NOTHING and returned AlreadyQuarantined for any offered
/// reason.
#[test]
fn a_retry_after_the_source_is_absent_still_compares_the_reason() -> TestResult {
    let mut store = elected_store()?;
    let corrupt: &[u8] = b"not an attempt receipt";
    stage(
        &store,
        ForeignAttemptReceipt {
            record_bytes: corrupt,
            ..base_row(corrupt)
        },
    )?;
    let id = attempt_id(ATTEMPT);
    quarantine(&mut store, &id, QuarantineReason::Undecodable)?;

    // The move removed the live row. Nothing is re-staged from here on:
    // that absence is the whole point of this witness.
    assert!(
        read(&store, &id)?.is_none(),
        "the move must remove the live row, or this test proves nothing"
    );

    // Same reason, source absent: idempotent.
    assert_eq!(
        quarantine(&mut store, &id, QuarantineReason::Undecodable)?,
        ReceiptQuarantined::AlreadyQuarantined,
        "an exact retry after a completed move is idempotent"
    );

    // DIFFERENT reason, source absent: a loud typed refusal, never a
    // silent success and never an overwrite of the first durable report.
    match quarantine(&mut store, &id, QuarantineReason::OverCeiling) {
        Err(WriteError::ReceiptQuarantineConflict {
            attempt,
            held,
            offered,
        }) => {
            assert_eq!(attempt, ATTEMPT);
            assert_eq!(held, "undecodable");
            assert_eq!(offered, "over-ceiling");
            Ok(())
        }
        other => {
            panic!("a different-reason retry after the source is absent must refuse, got {other:?}")
        }
    }
}

// -----------------------------------------------------------------------
// The terminal read-back's bindings
//
// The coordinates come from the ROW, so each binding is staged by writing a
// SETTLED receipt that names a terminal which is real but wrong — or absent.
// The event log is governed, so a test cannot rewrite a committed terminal;
// corrupting the receipt is both the permitted route and the faithful one,
// since a foreign writer could only ever reach the receipt.
//
// Each binding gets its OWN test so two rows removing two different bindings
// leave different records. The runner's duplicate guard cannot see that
// itself: the pid in every panic location makes any two details distinct.
// -----------------------------------------------------------------------

/// Stages a SETTLED receipt for `ATTEMPT` naming the given terminal.
fn stage_settled_naming(
    store: &EventStore,
    event: &str,
    created_ms: i64,
    step: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let coords = match event.parse() {
        Ok(event) => TerminalCoordinates::new(event, created_ms),
        Err(_) => panic!("the fixture event identity parses"),
    };
    let (settled, _) = reserved_receipt(ATTEMPT, step)
        .mark_settled(SettledMarker::new(Timestamp::from_millis(SETTLED_MS)));
    let bytes = encode_record(&AttemptReceiptRecord::new(
        settled,
        WriterEpoch::FIRST,
        coords,
    ));
    stage(
        store,
        ForeignAttemptReceipt {
            step: i64::from(step),
            terminal_event_id: event,
            terminal_created_ms: created_ms,
            settled_ms: Some(SETTLED_MS),
            record_bytes: &bytes,
            ..base_row(&bytes)
        },
    )
}

fn reclaim_refusal(store: &mut EventStore, expect: &str) -> TestResult {
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    match reclaim_within(&tx, &attempt_id(ATTEMPT), writer, wide()) {
        Err(WriteError::ReceiptTerminalUnproven { detail, .. }) => {
            assert!(
                detail.contains(expect),
                "the refusal must name the {expect:?} binding, got: {detail}"
            );
            Ok(())
        }
        other => panic!("the {expect:?} binding must refuse the reclaim, got {other:?}"),
    }
}

/// The terminal must EXIST.
#[test]
fn a_reclaim_refuses_when_the_named_terminal_is_absent() -> TestResult {
    let mut store = elected_store()?;
    stage_settled_naming(&store, "evt_033602b2tfff", TERMINAL_MS, 3)?;
    reclaim_refusal(&mut store, "no such durable event")?;
    assert!(
        read(&store, &attempt_id(ATTEMPT))?.is_some(),
        "the receipt survives a reclaim it could not prove"
    );
    Ok(())
}

/// The terminal must belong to THIS receipt's session.
#[test]
fn a_terminal_from_another_session_cannot_prove_this_receipt() -> TestResult {
    let mut store = elected_store()?;
    // A genuine terminal, committed on a DIFFERENT session.
    let other: SessionId = "ses_033602b2aaaa".parse()?;
    let other_aggregate = AggregateId::Session(other.clone());
    store.elect_writer(&other_aggregate)?;
    let bound = wide().0;
    let foreign = DurableAttemptReceipt::reserved(
        match BoundedAttemptId::new(attempt_id("att_foreign"), bound) {
            Ok(a) => a,
            Err(e) => panic!("fits: {e:?}"),
        },
        match BoundedSessionId::new(other.clone(), bound) {
            Ok(s) => s,
            Err(e) => panic!("fits: {e:?}"),
        },
        match StepNumber::new(3) {
            Some(s) => s,
            None => panic!("valid"),
        },
        match AttemptOrdinal::new(1) {
            Some(o) => o,
            None => panic!("valid"),
        },
        ReservedMarker::new(Timestamp::from_millis(RESERVED_MS)),
    );
    let coords = terminal();
    let writer = store.fencing_token(&other_aggregate)?;
    store.reserve_attempt_receipt(&ReserveInput {
        receipt: &foreign,
        writer,
        terminal: &coords,
        ceiling: wide(),
    })?;
    let writer = store.fencing_token(&other_aggregate)?;
    let tx = store.governed_transaction()?;
    EventStore::settle_attempt_process_loss_within(
        &tx,
        &attempt_id("att_foreign"),
        writer,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
        &mut [],
    )?;
    tx.commit()?.discard();

    stage_settled_naming(&store, TERMINAL, TERMINAL_MS, 3)?;
    reclaim_refusal(&mut store, "belongs to session")
}

/// The terminal must settle THIS receipt's step.
#[test]
fn a_terminal_for_another_step_cannot_prove_this_receipt() -> TestResult {
    let mut store = elected_store()?;
    // A genuine step-9 terminal on THIS session.
    reserve(&mut store, &reserved_receipt("att_step9", 9))?;
    settle(&mut store, &attempt_id("att_step9"))?;
    // A receipt for step 3 naming it.
    stage_settled_naming(&store, TERMINAL, TERMINAL_MS, 3)?;
    reclaim_refusal(&mut store, "settles step 9")
}

/// The persisted terminal INSTANT is a binding too: the receipt reserved an
/// exact (id, created) pair, and a lone id is not the coordinate.
#[test]
fn a_terminal_at_a_different_instant_cannot_prove_this_receipt() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt("att_real", 3))?;
    settle(&mut store, &attempt_id("att_real"))?;
    stage_settled_naming(&store, TERMINAL, TERMINAL_MS + 1, 3)?;
    reclaim_refusal(&mut store, "not the reserved instant")
}

/// An event of the right session that is NOT a Step terminal cannot prove
/// the receipt. Staged through the hostile event seam, because no public
/// path writes a non-terminal event under a reserved terminal identity.
#[test]
fn an_event_that_is_not_a_step_terminal_cannot_prove_this_receipt() -> TestResult {
    let mut store = elected_store()?;
    let aggregate = AggregateId::Session(session());
    hostile_rows::install_cursor(store.connection(), store.gate(), &aggregate, 0)?;
    hostile_rows::install_event(
        store.connection(),
        store.gate(),
        crate::write::hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: TERMINAL,
            seq: 0,
            created_ms: TERMINAL_MS,
            versioned_type: "session.created.1",
            data_text: "{}",
            writer_epoch: Some(1),
        },
    )?;
    stage_settled_naming(&store, TERMINAL, TERMINAL_MS, 3)?;
    reclaim_refusal(&mut store, "is not a durable Step terminal")
}

/// PS36: a receipt claiming an epoch AHEAD of the current election is
/// corruption, not lineage. Settlement refuses it rather than treating a
/// future epoch as a newer owner.
#[test]
fn a_receipt_reserved_under_a_future_epoch_refuses_settlement() -> TestResult {
    let mut store = elected_store()?;
    let future = match WriterEpoch::new(9) {
        Some(epoch) => epoch,
        None => panic!("9 is a valid epoch"),
    };
    let bytes = encode_record(&AttemptReceiptRecord::new(
        reserved_receipt(ATTEMPT, 3),
        future,
        terminal(),
    ));
    stage(
        &store,
        ForeignAttemptReceipt {
            writer_epoch: 9,
            record_bytes: &bytes,
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
        Err(WriteError::ReceiptStaleEpoch { held, offered, .. }) => {
            assert_eq!(held, 9, "the row's claimed epoch");
            assert_eq!(offered, 1, "the currently elected one");
            Ok(())
        }
        other => panic!("a future-epoch row must refuse settlement, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// L4b1 — the terminal proof is total over BOTH Step terminal contracts
//
// The roster of durable Step terminals and the codec each is read by are one
// decision. Before L4b1 they were two: a name list, and a hard-coded failure
// decoder under it. These witnesses drive the SUCCESS contract, which no
// existing test reaches, and pin each wrong-codec pairing in both directions.
// -----------------------------------------------------------------------

/// The canonical bytes of a valid `session.step.ended.2` payload.
fn ended_payload(step: u32) -> String {
    let step = match StepNumber::new(step) {
        Some(step) => step,
        None => panic!("the fixture step number is positive"),
    };
    let fact = grapt_event::session_step_ended::StepEndedFact::new(
        step,
        grapt_types::ProviderCompletionDisposition::AssertedComplete,
    );
    grapt_event::session_step_ended::encode(&fact).to_string()
}

/// The canonical bytes of a valid `session.step.failed.2` payload.
fn failed_payload(step: u32) -> String {
    let step = match StepNumber::new(step) {
        Some(step) => step,
        None => panic!("the fixture step number is positive"),
    };
    let fact = match grapt_event::session_step_failed::StepFailedFact::new(
        step,
        grapt_types::StepTerminalReason::ProcessLoss {
            effect: ExternalEffectState::NoneInitiated,
        },
    ) {
        Ok(fact) => fact,
        Err(error) => panic!("the fixture failure terminal constructs: {error}"),
    };
    grapt_event::session_step_failed::encode(&fact)
        .unwrap_or_else(|error| panic!("the fixture fact must encode: {error}"))
        .to_string()
}

/// Appends a REAL `session.step.ended.2` through the governed seam, at the
/// exact coordinates a receipt reserved. Not a hostile row: the positive
/// witness must prove the contract a production success terminal really writes.
fn append_ended_terminal(
    store: &mut EventStore,
    event: &str,
    created_ms: i64,
    step: u32,
) -> TestResult {
    let id = match event.parse() {
        Ok(id) => id,
        Err(_) => panic!("the fixture event identity parses"),
    };
    let step = match StepNumber::new(step) {
        Some(step) => step,
        None => panic!("the fixture step number is positive"),
    };
    let fact = grapt_event::session_step_ended::StepEndedFact::new(
        step,
        grapt_types::ProviderCompletionDisposition::AssertedComplete,
    );
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    crate::step_ended::record(&tx, &session(), &id, writer, created_ms, &fact, &mut [])?;
    tx.commit()?.discard();
    Ok(())
}

/// Installs one hostile event under the reserved terminal identity, so a
/// name/payload pairing no public writer can produce is still reachable.
fn install_terminal_row(
    store: &EventStore,
    versioned_type: &str,
    data_text: &str,
    created_ms: i64,
) -> TestResult {
    let aggregate = AggregateId::Session(session());
    hostile_rows::install_cursor(store.connection(), store.gate(), &aggregate, 0)?;
    hostile_rows::install_event(
        store.connection(),
        store.gate(),
        crate::write::hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: TERMINAL,
            seq: 0,
            created_ms,
            versioned_type,
            data_text,
            writer_epoch: Some(1),
        },
    )?;
    Ok(())
}

/// THE POSITIVE WITNESS. A receipt whose reserved terminal is a real
/// `session.step.ended.2` is proven and reclaimed.
///
/// At the base this reds as "is not a durable Step terminal fact": the success
/// kind was not in the roster. With the name added but the codec still
/// hard-coded to the failure contract it STILL reds, as "does not decode" —
/// which is the whole finding, and why the roster and the codec had to change
/// together.
#[test]
fn a_receipt_is_proven_and_reclaimed_by_its_success_terminal() -> TestResult {
    let mut store = elected_store()?;
    append_ended_terminal(&mut store, TERMINAL, TERMINAL_MS, 3)?;
    stage_settled_naming(&store, TERMINAL, TERMINAL_MS, 3)?;

    let id = attempt_id(ATTEMPT);
    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    // PANICS on refusal rather than carrying it out by `?`, for the reason the
    // `quarantine` helper above records: an `Err` propagated from a positive
    // witness leaves the mutation harness no `panicked at` line, and the row
    // that removes this dispatch reads as "failed without a captured
    // diagnostic" instead of naming what it broke.
    let outcome = match reclaim_within(&tx, &id, writer, wide()) {
        Ok(outcome) => outcome,
        Err(error) => panic!(
            "a real session.step.ended.2 at the reserved coordinates must prove this receipt; the read-back refused instead: {error}"
        ),
    };
    tx.commit()?.discard();
    assert_eq!(
        outcome,
        AttemptReceiptReclaimed::Reclaimed,
        "a proven success terminal reclaims its receipt"
    );
    assert!(
        read(&store, &id)?.is_none(),
        "the reclaimed receipt is gone"
    );
    Ok(())
}

/// The oracle's v1 of the same NAME is a different contract. A v2-shaped
/// payload under `session.step.ended.1` proves nothing: decode keys on the
/// exact (name, version) pair, so the roster must not match on name alone.
#[test]
fn the_oracle_v1_success_name_is_not_a_durable_step_terminal() -> TestResult {
    let mut store = elected_store()?;
    let payload = ended_payload(3);
    install_terminal_row(&store, "session.step.ended.1", &payload, TERMINAL_MS)?;
    stage_settled_naming(&store, TERMINAL, TERMINAL_MS, 3)?;
    reclaim_refusal(&mut store, "is not a durable Step terminal")
}

/// The success NAME carrying the failure contract's payload refuses. The kind
/// selects the codec, so this cannot be read by the decoder that would accept
/// it.
#[test]
fn a_success_terminal_carrying_a_failure_payload_refuses() -> TestResult {
    let mut store = elected_store()?;
    let payload = failed_payload(3);
    install_terminal_row(&store, "session.step.ended.2", &payload, TERMINAL_MS)?;
    stage_settled_naming(&store, TERMINAL, TERMINAL_MS, 3)?;
    reclaim_refusal(&mut store, "does not decode")
}

/// And the mirror: the failure NAME carrying a success payload refuses under
/// the FAILURE codec.
///
/// PRESERVATION, stated honestly: this passes at the base too, because the base
/// already decodes everything with the failure contract. It is not shown to
/// fail against base code and does not claim to be. Its job is to stop the
/// dispatcher from ever becoming a try-every-decoder fallback, and it is proven
/// by mutant `LB04` rather than by a base red.
#[test]
fn a_failure_terminal_carrying_a_success_payload_refuses() -> TestResult {
    let mut store = elected_store()?;
    let payload = ended_payload(3);
    install_terminal_row(&store, "session.step.failed.2", &payload, TERMINAL_MS)?;
    stage_settled_naming(&store, TERMINAL, TERMINAL_MS, 3)?;
    reclaim_refusal(&mut store, "does not decode")
}

/// The step binding holds through the success codec too: a real success
/// terminal for ANOTHER step cannot prove this receipt.
#[test]
fn a_success_terminal_for_another_step_cannot_prove_this_receipt() -> TestResult {
    let mut store = elected_store()?;
    append_ended_terminal(&mut store, TERMINAL, TERMINAL_MS, 9)?;
    stage_settled_naming(&store, TERMINAL, TERMINAL_MS, 3)?;
    reclaim_refusal(&mut store, "settles step 9")
}

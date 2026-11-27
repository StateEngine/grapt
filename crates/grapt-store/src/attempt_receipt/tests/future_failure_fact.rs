use super::*;
// ---------------------------------------------------------------------------
// Tolerant readers: the receipt's terminal proof against a future writer's
// failure vocabulary.
//
// The terminal is staged through the hostile event seam, because no public
// path can write a failure reason this build does not know: the writer's
// checked constructor refuses `Unknown`. What is under test is the READER —
// a recovered receipt whose terminal a later build named with an unknown
// word must still settle, and a corrupted or non-failure terminal must still
// refuse, in both versions.
// ---------------------------------------------------------------------------

fn stage_terminal(
    store: &EventStore,
    versioned_type: &str,
    data_text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let aggregate = AggregateId::Session(session());
    hostile_rows::install_cursor(store.connection(), store.gate(), &aggregate, 0)?;
    hostile_rows::install_event(
        store.connection(),
        store.gate(),
        hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: TERMINAL,
            seq: 0,
            created_ms: TERMINAL_MS,
            versioned_type,
            data_text,
            writer_epoch: Some(1),
        },
    )?;
    Ok(())
}

/// W6: a v2 terminal whose kind this build does not know is still a durable
/// Step failure. A permitted receipt settles against it, a second pass
/// re-proves it idempotently, and the supported reclaim path removes the
/// settled receipt without touching a terminal byte: the unknown word
/// carries no effect authority, so nothing is re-derived or re-appended.
#[test]
fn a_v2_terminal_with_an_unknown_kind_settles_and_reproves() -> TestResult {
    let mut store = elected_store()?;
    let receipt = reserved_receipt(ATTEMPT, 3);
    reserve(&mut store, &receipt)?;
    let id = attempt_id(ATTEMPT);
    permit(&mut store, &id, PERMIT_MS)?;
    let data = r#"{"step":3,"reason":{"kind":"aborted"}}"#;
    stage_terminal(&store, "session.step.failed.2", data)?;

    assert_eq!(
        settle(&mut store, &id)?,
        AttemptSettlement::Settled,
        "the permitted receipt settles the unknown terminal"
    );
    let held: String =
        store
            .connection()
            .query_row("SELECT data FROM event WHERE id = ?1", [TERMINAL], |r| {
                r.get(0)
            })?;
    assert_eq!(
        held, data,
        "the tolerant reader must not rewrite the terminal it proved"
    );
    assert_eq!(
        settle(&mut store, &id)?,
        AttemptSettlement::AlreadySettled,
        "a recovery pass must re-prove the unknown terminal, not refuse it"
    );
    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    let reclaimed = EventStore::reclaim_attempt_receipt_within(&tx, &id, writer, wide())?;
    tx.commit()?.discard();
    assert!(
        matches!(reclaimed, AttemptReceiptReclaimed::Reclaimed),
        "the settled receipt reclaims through the supported path"
    );
    assert!(
        read(&store, &id)?.is_none(),
        "the reclaimed receipt is gone"
    );
    assert_eq!(
        terminal_rows(&store)?,
        1,
        "reclaim re-proves, it never replays"
    );
    Ok(())
}

/// W6, v3: the observed wrapper carries the same tolerance, end to end — a
/// permitted receipt settles against an unknown kind with a real
/// observation, re-proves idempotently, and reclaims without touching the
/// terminal byte.
#[test]
fn a_v3_terminal_with_an_unknown_kind_settles_and_reproves() -> TestResult {
    let mut store = elected_store()?;
    let receipt = reserved_receipt(ATTEMPT, 3);
    reserve(&mut store, &receipt)?;
    let id = attempt_id(ATTEMPT);
    permit(&mut store, &id, PERMIT_MS)?;
    let data = r#"{"step":3,"reason":{"kind":"aborted"},"completedAt":300}"#;
    stage_terminal(&store, "session.step.failed.3", data)?;

    assert_eq!(
        settle(&mut store, &id)?,
        AttemptSettlement::Settled,
        "the permitted receipt settles the unknown observed terminal"
    );
    let held: String =
        store
            .connection()
            .query_row("SELECT data FROM event WHERE id = ?1", [TERMINAL], |r| {
                r.get(0)
            })?;
    assert_eq!(
        held, data,
        "the tolerant reader must not rewrite the observed terminal it proved"
    );
    assert_eq!(
        settle(&mut store, &id)?,
        AttemptSettlement::AlreadySettled,
        "a recovery pass must re-prove the unknown terminal, not refuse it"
    );
    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    let reclaimed = EventStore::reclaim_attempt_receipt_within(&tx, &id, writer, wide())?;
    tx.commit()?.discard();
    assert!(matches!(reclaimed, AttemptReceiptReclaimed::Reclaimed));
    assert!(
        read(&store, &id)?.is_none(),
        "the reclaimed receipt is gone"
    );
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.step.failed.3'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 1, "reclaim re-proves, it never replays");
    Ok(())
}

/// Corruption is still corruption: a broken terminal refuses settlement in
/// both versions — and a refusal leaves the receipt standing and untouched,
/// the settled marker empty, and the staged terminal byte-for-byte as the
/// hostile seam wrote it, with no second terminal appended. The tolerance
/// admits vocabulary, not damage. Every v3 corruption row carries a genuine
/// `completedAt`, so the refusal is attributed to the terminal's own fault,
/// never to a missing observation. The blank-word rows are the TR-H1
/// correction at the receipt level: a blank is no word, and a terminal that
/// names no word proves nothing, in either version.
#[test]
fn a_broken_future_terminal_still_refuses_settlement() -> TestResult {
    let payloads = [
        ("session.step.failed.2", r#"{"step":3}"#),
        (
            "session.step.failed.2",
            r#"{"step":3,"reason":{"kind":"process-loss","effect":{}}}"#,
        ),
        (
            "session.step.failed.2",
            r#"{"step":3,"reason":{"kind":"ended","completion":"asserted-complete"}}"#,
        ),
        (
            "session.step.failed.2",
            r#"{"step":3,"reason":{"kind":""}}"#,
        ),
        (
            "session.step.failed.2",
            r#"{"step":3,"reason":{"kind":"interrupted","by":""}}"#,
        ),
        (
            "session.step.failed.3",
            r#"{"step":3,"reason":{"kind":"ended","completion":"asserted-complete"},"completedAt":300}"#,
        ),
        (
            "session.step.failed.3",
            r#"{"step":3,"reason":{"kind":"aborted"}}"#,
        ),
        ("session.step.failed.3", r#"{"step":3,"completedAt":300}"#),
        (
            "session.step.failed.3",
            r#"{"step":3,"reason":{"kind":"process-loss","effect":{}},"completedAt":300}"#,
        ),
        (
            "session.step.failed.3",
            r#"{"step":3,"reason":{"kind":""},"completedAt":300}"#,
        ),
        (
            "session.step.failed.3",
            r#"{"step":3,"reason":{"kind":"interrupted","by":""},"completedAt":300}"#,
        ),
    ];
    for (versioned_type, data) in payloads {
        let mut store = elected_store()?;
        let receipt = reserved_receipt(ATTEMPT, 3);
        reserve(&mut store, &receipt)?;
        let id = attempt_id(ATTEMPT);
        stage_terminal(&store, versioned_type, data)?;
        let writer = token(&store)?;
        let tx = store.governed_transaction()?;
        let refused = EventStore::settle_attempt_process_loss_within(
            &tx,
            &id,
            writer,
            wide(),
            SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
            &mut [],
        );
        drop(tx);
        match refused {
            Err(WriteError::ReceiptTerminalUnproven { attempt, .. }) => {
                assert_eq!(attempt, ATTEMPT, "{versioned_type} {data}")
            }
            other => panic!(
                "a broken {versioned_type} terminal must refuse, not settle: {data} -> {other:?}"
            ),
        }
        assert_eq!(
            columns(&store, ATTEMPT)?.settled_ms,
            None,
            "a refused terminal fills no marker: {data}"
        );
        assert!(
            read(&store, &id)?.is_some(),
            "a refused settlement leaves the receipt standing: {data}"
        );
        let held: String = store.connection().query_row(
            "SELECT data FROM event WHERE id = ?1",
            [TERMINAL],
            |r| r.get(0),
        )?;
        assert_eq!(held, data, "a refused terminal is not rewritten: {data}");
        let count: i64 = store.connection().query_row(
            "SELECT COUNT(*) FROM event WHERE type = ?1",
            [versioned_type],
            |r| r.get(0),
        )?;
        assert_eq!(
            count, 1,
            "a refused settlement appends no second terminal: {data}"
        );
    }
    Ok(())
}

/// The contradiction the tolerance must NOT soften: a proven
/// `none-initiated` terminal still denies a receipt whose send permit made
/// the loss ambiguous. An unknown word carries no effect authority, but a
/// KNOWN `none-initiated` word still carries its full refusal.
#[test]
fn a_proven_none_initiated_terminal_still_denies_a_permitted_receipt() -> TestResult {
    let mut store = elected_store()?;
    let receipt = reserved_receipt(ATTEMPT, 3);
    reserve(&mut store, &receipt)?;
    let id = attempt_id(ATTEMPT);
    permit(&mut store, &id, PERMIT_MS)?;
    stage_terminal(
        &store,
        "session.step.failed.2",
        r#"{"step":3,"reason":{"kind":"process-loss","effect":{"kind":"none-initiated"}}}"#,
    )?;

    let writer = token(&store)?;
    let tx = store.governed_transaction()?;
    match EventStore::settle_attempt_process_loss_within(
        &tx,
        &id,
        writer,
        wide(),
        SettledMarker::new(Timestamp::from_millis(SETTLED_MS)),
        &mut [],
    ) {
        Err(WriteError::ReceiptTerminalUnproven { detail, .. }) => {
            assert!(
                detail.contains("denies a surviving attempt's send permit"),
                "the known contradiction must still fire: {detail}"
            );
            Ok(())
        }
        other => {
            panic!("the permitted receipt must refuse the none-initiated terminal, got {other:?}")
        }
    }
}

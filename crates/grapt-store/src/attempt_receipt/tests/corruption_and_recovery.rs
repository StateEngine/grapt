use super::lifecycle::*;
// -----------------------------------------------------------------------
// Corruption
// -----------------------------------------------------------------------

pub(super) fn stage(
    store: &EventStore,
    row: ForeignAttemptReceipt<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    hostile_rows::install_attempt_receipt(store.connection(), store.gate(), row)?;
    Ok(())
}

fn honest_bytes(step: u32) -> Vec<u8> {
    encode_record(&AttemptReceiptRecord::new(
        reserved_receipt(ATTEMPT, step),
        WriterEpoch::FIRST,
        terminal(),
    ))
}

pub(super) fn base_row(bytes: &[u8]) -> ForeignAttemptReceipt<'_> {
    ForeignAttemptReceipt {
        attempt_id: ATTEMPT,
        session_id: SESSION,
        step: 3,
        ordinal: 1,
        writer_epoch: 1,
        terminal_event_id: TERMINAL,
        terminal_created_ms: TERMINAL_MS,
        reserved_ms: RESERVED_MS,
        request_sent_ms: None,
        settled_ms: None,
        record_bytes: bytes,
    }
}

/// All TEN indexed columns are cross-checked against the decoded bytes, each
/// named separately. The two terminal coordinates are in the record exactly
/// so they can be checked at all.
#[test]
fn every_indexed_column_is_checked_against_the_decoded_bytes() -> TestResult {
    let honest = honest_bytes(3);
    let cases: Vec<(&str, ForeignAttemptReceipt<'_>, &str)> = vec![
        (
            "attempt",
            ForeignAttemptReceipt {
                attempt_id: "att_other",
                ..base_row(&honest)
            },
            "not the row's own id",
        ),
        (
            "session",
            ForeignAttemptReceipt {
                session_id: "ses_033602b2bbbb",
                ..base_row(&honest)
            },
            "not the row's session column",
        ),
        (
            "step",
            ForeignAttemptReceipt {
                step: 4,
                ..base_row(&honest)
            },
            "not the row's step column",
        ),
        (
            "ordinal",
            ForeignAttemptReceipt {
                ordinal: 2,
                ..base_row(&honest)
            },
            "not the row's ordinal column",
        ),
        (
            "writer epoch",
            ForeignAttemptReceipt {
                writer_epoch: 2,
                ..base_row(&honest)
            },
            "not the row's epoch column",
        ),
        (
            "terminal event",
            ForeignAttemptReceipt {
                terminal_event_id: "evt_033602b2tfff",
                ..base_row(&honest)
            },
            "not the row's terminal column",
        ),
        (
            "terminal created",
            ForeignAttemptReceipt {
                terminal_created_ms: TERMINAL_MS + 1,
                ..base_row(&honest)
            },
            "not the row's terminal-created column",
        ),
        (
            "reserved",
            ForeignAttemptReceipt {
                reserved_ms: RESERVED_MS + 1,
                ..base_row(&honest)
            },
            "not the row's column",
        ),
        (
            "send permit",
            ForeignAttemptReceipt {
                request_sent_ms: Some(PERMIT_MS),
                ..base_row(&honest)
            },
            "decode to request-sent",
        ),
        (
            "settled",
            ForeignAttemptReceipt {
                settled_ms: Some(SETTLED_MS),
                ..base_row(&honest)
            },
            "decode to settled",
        ),
    ];

    for (dimension, row, expect) in cases {
        let store = elected_store()?;
        let staged_id = row.attempt_id.to_owned();
        stage(&store, row)?;
        match store.attempt_receipt(&attempt_id(&staged_id), wide()) {
            Err(WriteError::ReceiptRecordMismatch { detail, .. }) => assert!(
                detail.contains(expect),
                "the {dimension} disagreement must be named, got: {detail}"
            ),
            other => panic!(
                "the {dimension} column and its bytes disagree; expected a \
                 mismatch refusal, got {other:?}"
            ),
        }
    }
    Ok(())
}

#[test]
fn the_strict_decoder_refuses_every_malformed_record() -> TestResult {
    let honest = honest_bytes(3);

    let mut wrong_tag = honest.clone();
    wrong_tag[0] = b'X';
    let mut trailing = honest.clone();
    trailing.push(0);
    let mut truncated = honest.clone();
    truncated.truncate(honest.len() - 1);
    let settled_at = honest.len() - 9;
    let mut valued_absent = honest.clone();
    valued_absent[settled_at + 8] = 7;
    let mut bad_presence = honest.clone();
    bad_presence[settled_at] = 2;

    let cases = [
        (&wrong_tag, "canonical attempt-receipt tag"),
        (&trailing, "trailing byte"),
        (&truncated, "truncated"),
        (&valued_absent, "marked absent but carries the value"),
        (&bad_presence, "presence byte 2 is not 0 or 1"),
    ];
    for (bytes, expect) in cases {
        let store = elected_store()?;
        stage(&store, base_row(bytes))?;
        match store.attempt_receipt(&attempt_id(ATTEMPT), wide()) {
            Err(WriteError::ReceiptUndecodable { reason, .. }) => assert!(
                reason.contains(expect),
                "the refusal must name {expect:?}, got: {reason}"
            ),
            other => {
                panic!("malformed bytes must refuse as undecodable for {expect:?}, got {other:?}")
            }
        }
    }
    Ok(())
}

#[test]
fn markers_that_run_backwards_are_refused() -> TestResult {
    let backwards = {
        let (sent, _) = reserved_receipt(ATTEMPT, 3).mark_request_sent(RequestSentMarker::new(
            Timestamp::from_millis(RESERVED_MS - 1),
        ));
        encode_record(&AttemptReceiptRecord::new(
            sent,
            WriterEpoch::FIRST,
            terminal(),
        ))
    };
    let store = elected_store()?;
    stage(
        &store,
        ForeignAttemptReceipt {
            request_sent_ms: Some(RESERVED_MS - 1),
            ..base_row(&backwards)
        },
    )?;
    match store.attempt_receipt(&attempt_id(ATTEMPT), wide()) {
        Err(WriteError::ReceiptMarkerOrder { attempt, detail }) => {
            assert_eq!(attempt, ATTEMPT);
            assert!(
                detail.contains("precedes the reserve"),
                "the refusal must say which order broke, got: {detail}"
            );
            Ok(())
        }
        other => panic!("a marker preceding its own reserve must refuse, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// The one-row recovery cursor
// -----------------------------------------------------------------------

/// Stages one reserved row under a distinct attempt id and returns its
/// exact encoded length, so a byte-accounting assertion measures the rows it
/// actually staged.
fn stage_row(
    store: &EventStore,
    index: usize,
    settled: bool,
) -> Result<(String, usize), Box<dyn std::error::Error>> {
    let id = format!("att_{index:04}");
    let bound = wide().0;
    let attempt = match BoundedAttemptId::new(attempt_id(&id), bound) {
        Ok(attempt) => attempt,
        Err(error) => panic!("the fixture id fits: {error:?}"),
    };
    let session_bounded = match BoundedSessionId::new(session(), bound) {
        Ok(session) => session,
        Err(error) => panic!("fits: {error:?}"),
    };
    let receipt = match (StepNumber::new(1), AttemptOrdinal::new(1)) {
        (Some(step), Some(ordinal)) => DurableAttemptReceipt::reserved(
            attempt,
            session_bounded,
            step,
            ordinal,
            ReservedMarker::new(Timestamp::from_millis(RESERVED_MS)),
        ),
        _ => panic!("valid fixture coordinates"),
    };
    let receipt = if settled {
        receipt
            .mark_settled(SettledMarker::new(Timestamp::from_millis(SETTLED_MS)))
            .0
    } else {
        receipt
    };
    let bytes = encode_record(&AttemptReceiptRecord::new(
        receipt,
        WriterEpoch::FIRST,
        terminal(),
    ));
    stage(
        store,
        ForeignAttemptReceipt {
            attempt_id: &id,
            step: 1,
            settled_ms: settled.then_some(SETTLED_MS),
            record_bytes: &bytes,
            ..base_row(&bytes)
        },
    )?;
    Ok((id, bytes.len()))
}

/// Yields a recovery step, or PANICS naming what was expected.
///
/// `?` on this call returns `Err` from the test, and an `Err` return leaves
/// NO `panicked at` line for the mutation harness to quote — three rows came
/// back RED with the detail "failed without a captured diagnostic", which is
/// a red that says nothing about which law fired. Panicking with a named
/// expectation is what makes each row's evidence its own.
///
/// The ceiling is a PARAMETER rather than a hardcoded `wide()`. That is the
/// whole reason PS24 stayed non-diagnostic after the other five call sites
/// were converted: its witness is the one that must pass a TIGHT ceiling, so
/// a helper that assumed the wide one could not carry it and its `?` stayed.
fn step_or_panic(
    store: &EventStore,
    after: Option<&PhysicalAttemptId>,
    remaining: usize,
    ceiling: AttemptRecordCeiling,
    expected: &str,
) -> RecoveryStep {
    match store.next_attempt_recovery_row(&session(), after, remaining, ceiling) {
        Ok(step) => step,
        Err(error) => panic!("{expected}, but the cursor refused: {error:?}"),
    }
}

/// The cursor returns ONE row at a time, in order, exclusively — and it
/// INCLUDES settled rows, because a settled receipt whose reclaim never ran
/// is a leak an unsettled-only scan could never see.
#[test]
fn the_recovery_cursor_returns_one_row_at_a_time_and_includes_settled_leaks() -> TestResult {
    let store = elected_store()?;
    let (first, len) = stage_row(&store, 0, false)?;
    let (leaked, _) = stage_row(&store, 1, true)?;

    let step = step_or_panic(
        &store,
        None,
        WIDE_BUDGET,
        wide(),
        "the first row must come back",
    );
    let (row, next_after, charged) = match step {
        RecoveryStep::Row {
            row,
            next_after,
            bytes_charged,
        } => (row, next_after, bytes_charged),
        other => panic!("the first row must come back, got {other:?}"),
    };
    match row {
        RecoveryRow::Receipt(record) => {
            assert_eq!(record.receipt().attempt().as_str(), first);
            assert_eq!(
                record.receipt().disposition(),
                ReceiptDisposition::Lost(LostEffect::NoneInitiated)
            );
        }
        other => panic!("expected a readable receipt, got {other:?}"),
    }
    assert_eq!(next_after.as_str(), first);
    assert_eq!(charged, len, "exactly this row's bytes were materialized");

    let step = step_or_panic(
        &store,
        Some(&next_after),
        WIDE_BUDGET,
        wide(),
        "the SETTLED leak must be enumerated",
    );
    let (row, next_after) = match step {
        RecoveryStep::Row {
            row, next_after, ..
        } => (row, next_after),
        other => panic!("the SETTLED leak must be enumerated, got {other:?}"),
    };
    match row {
        RecoveryRow::Receipt(record) => {
            assert_eq!(record.receipt().attempt().as_str(), leaked);
            assert_eq!(
                record.receipt().disposition(),
                ReceiptDisposition::AlreadySettled,
                "a settled leak is exactly what the scan must surface"
            );
        }
        other => panic!("expected the settled leak, got {other:?}"),
    }

    match step_or_panic(
        &store,
        Some(&next_after),
        WIDE_BUDGET,
        wide(),
        "the cursor must report exhaustion after the last row",
    ) {
        RecoveryStep::Exhausted => Ok(()),
        other => panic!("the listing must end honestly, got {other:?}"),
    }
}

/// A row larger than the per-row ceiling is refused from its DECLARED size,
/// never fetched, and reported as durable-quarantine work — never skipped,
/// and never given a disposition, so it cannot be classified replay-safe.
#[test]
fn an_oversize_row_is_refused_from_its_declared_size_without_being_fetched() -> TestResult {
    let store = elected_store()?;
    let (_, short) = stage_row(&store, 0, false)?;
    let tight = ceiling(match u32::try_from(short) {
        Ok(len) => len,
        Err(_) => panic!("the fixture record fits a u32"),
    });

    let long_id = format!("att_1{}", "z".repeat(40));
    let bound = wide().0;
    let attempt = match BoundedAttemptId::new(attempt_id(&long_id), bound) {
        Ok(attempt) => attempt,
        Err(error) => panic!("fits the wide bound: {error:?}"),
    };
    let session_bounded = match BoundedSessionId::new(session(), bound) {
        Ok(session) => session,
        Err(error) => panic!("fits: {error:?}"),
    };
    let long = match (StepNumber::new(1), AttemptOrdinal::new(1)) {
        (Some(step), Some(ordinal)) => DurableAttemptReceipt::reserved(
            attempt,
            session_bounded,
            step,
            ordinal,
            ReservedMarker::new(Timestamp::from_millis(RESERVED_MS)),
        ),
        _ => panic!("valid coordinates"),
    };
    let long_bytes = encode_record(&AttemptReceiptRecord::new(
        long,
        WriterEpoch::FIRST,
        terminal(),
    ));
    assert!(long_bytes.len() > short);
    stage(
        &store,
        ForeignAttemptReceipt {
            attempt_id: &long_id,
            step: 1,
            record_bytes: &long_bytes,
            ..base_row(&long_bytes)
        },
    )?;

    // Skip past the short row to reach the oversize one.
    //
    // The ceiling here is TIGHT, which is the whole witness: the budget is
    // wide, so the only thing that can refuse this row is its DECLARED size.
    // Reaching that refusal through `?` is what left this row's evidence as
    // "failed without a captured diagnostic" — with the declared-size check
    // removed the bounded read withholds the blob instead, and the cursor
    // returns `Err` rather than panicking. Naming the expectation here is
    // what makes the mutation's red say which law it broke.
    let short_cursor = attempt_id("att_0000");
    match step_or_panic(
        &store,
        Some(&short_cursor),
        WIDE_BUDGET,
        tight,
        "an oversize row must be refused from its DECLARED size and reported as \
         over-ceiling quarantine work",
    ) {
        RecoveryStep::Row {
            row,
            bytes_charged,
            next_after,
        } => {
            assert_eq!(
                bytes_charged, 0,
                "the oversize blob must never be materialized"
            );
            assert_eq!(next_after.as_str(), long_id);
            match row {
                RecoveryRow::Untrustworthy {
                    attempt,
                    declared_bytes,
                    reason,
                } => {
                    assert_eq!(attempt.as_str(), long_id);
                    assert_eq!(declared_bytes, long_bytes.len());
                    assert_eq!(reason, QuarantineReason::OverCeiling);
                    Ok(())
                }
                other => panic!("an oversize row must be untrustworthy work, got {other:?}"),
            }
        }
        other => panic!("the oversize row must be REPORTED, not skipped, got {other:?}"),
    }
}

/// A row within the ceiling but beyond the remaining budget is NOT fetched
/// and the cursor does NOT move. This is the check order the addendum
/// requires: declared size, then budget, then materialize.
#[test]
fn a_row_beyond_the_remaining_budget_does_not_move_the_cursor() -> TestResult {
    let store = elected_store()?;
    let (_, len) = stage_row(&store, 0, false)?;
    match step_or_panic(
        &store,
        None,
        len - 1,
        wide(),
        "a row beyond the budget must be reported, not refused",
    ) {
        RecoveryStep::BudgetExhausted {
            declared_bytes,
            remaining,
        } => {
            assert_eq!(declared_bytes, len);
            assert_eq!(remaining, len - 1);
        }
        other => panic!("a row beyond the budget must not be fetched, got {other:?}"),
    }
    // Exactly at the budget it IS returned, so the bound is inclusive.
    match step_or_panic(
        &store,
        None,
        len,
        wide(),
        "a row exactly at the budget must be admitted",
    ) {
        RecoveryStep::Row { bytes_charged, .. } => {
            assert_eq!(bytes_charged, len);
            Ok(())
        }
        other => panic!("a row exactly at the budget must be admitted, got {other:?}"),
    }
}

/// A row this decoder cannot read is reported as untrustworthy rather than
/// skipped or aborting the pass: the caller must durably quarantine it.
#[test]
fn a_corrupt_row_is_reported_as_untrustworthy_not_skipped() -> TestResult {
    let store = elected_store()?;
    stage(
        &store,
        ForeignAttemptReceipt {
            attempt_id: "att_0000",
            record_bytes: b"not an attempt receipt",
            ..base_row(b"not an attempt receipt")
        },
    )?;
    match step_or_panic(
        &store,
        None,
        WIDE_BUDGET,
        wide(),
        "an oversize row must be reported as quarantine work, not refused",
    ) {
        RecoveryStep::Row { row, .. } => match row {
            RecoveryRow::Untrustworthy { reason, .. } => {
                assert_eq!(reason, QuarantineReason::Undecodable);
                Ok(())
            }
            other => panic!("a corrupt row must be untrustworthy work, got {other:?}"),
        },
        other => panic!("a corrupt row must be reported, got {other:?}"),
    }
}

use super::*;
// ---------------------------------------------------------------------------
// The NoneInitiated / send-permit contradiction, in every supported version
// ---------------------------------------------------------------------------

/// TR-H2: a proven `none-initiated` terminal denies a receipt whose send
/// permit made the loss ambiguous. The contradiction must be checked for
/// EVERY failure terminal version this store supports, each read through the
/// codec its name owns: a v3 terminal carrying a real observation must deny
/// the same receipt, or a restart would settle it and the contradicted
/// effect could be re-run.
#[test]
fn a_none_initiated_terminal_denies_a_permitted_receipt_in_every_version() -> TestResult {
    let terminals = [
        (
            "session.step.failed.2",
            r#"{"step":3,"reason":{"kind":"process-loss","effect":{"kind":"none-initiated"}}}"#,
        ),
        (
            "session.step.failed.3",
            r#"{"step":3,"reason":{"kind":"process-loss","effect":{"kind":"none-initiated"}},"completedAt":300}"#,
        ),
    ];
    for (versioned_type, data) in terminals {
        let mut store = elected_store()?;
        let receipt = reserved_receipt(ATTEMPT, 3);
        reserve(&mut store, &receipt)?;
        let id = attempt_id(ATTEMPT);
        permit(&mut store, &id, PERMIT_MS)?;
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
                versioned_type,
                data_text: data,
                writer_epoch: Some(1),
            },
        )?;

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
            Err(WriteError::ReceiptTerminalUnproven { detail, .. }) => {
                assert!(
                    detail.contains("denies a surviving attempt's send permit"),
                    "{versioned_type}: the known contradiction must fire: {detail}"
                );
            }
            other => panic!(
                "a {versioned_type} none-initiated terminal must deny the permitted receipt, got {other:?}"
            ),
        }
        assert_eq!(
            columns(&store, ATTEMPT)?.settled_ms,
            None,
            "a contradictory terminal fills no marker: {versioned_type}"
        );
    }
    Ok(())
}

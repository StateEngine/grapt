// CLAIM-2: process-loss recovery promotes only the Step's active attempt's
// checkpointed text, whichever of the Step's receipts it settles first.

const RETRY: &str = "att_033602b20002";
const STEP: u32 = 3;
const MESSAGE: &str = "msg_033602b2m001";

fn receipt_at(attempt: &str, ordinal: u32) -> DurableAttemptReceipt {
    let bound = wide().0;
    let attempt = match BoundedAttemptId::new(attempt_id(attempt), bound) {
        Ok(attempt) => attempt,
        Err(error) => panic!("the fixture attempt id fits: {error:?}"),
    };
    let session = match BoundedSessionId::new(session(), bound) {
        Ok(session) => session,
        Err(error) => panic!("the fixture session id fits: {error:?}"),
    };
    match (StepNumber::new(STEP), AttemptOrdinal::new(ordinal)) {
        (Some(step), Some(ordinal)) => DurableAttemptReceipt::reserved(
            attempt,
            session,
            step,
            ordinal,
            ReservedMarker::new(Timestamp::from_millis(RESERVED_MS)),
        ),
        _ => panic!("the fixture step coordinates are valid"),
    }
}

fn parsed<T: std::str::FromStr>(value: &str) -> T {
    match value.parse() {
        Ok(parsed) => parsed,
        Err(_) => panic!("the fixture identity {value} parses"),
    }
}

fn text_ref(id: &str) -> grapt_types::ArtifactRef {
    match (
        grapt_types::ArtifactId::new(id),
        grapt_types::ArtifactDigest::new("sha256:aa"),
    ) {
        (Some(id), Some(digest)) => grapt_types::ArtifactRef::new(id, 12, digest),
        _ => panic!("the fixture artifact reference is valid"),
    }
}

/// The Step's message, opened by the first attempt.
fn open_message(store: &mut EventStore) -> TestResult {
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    let step = StepNumber::new(STEP).ok_or("step")?;
    let fact = grapt_event::session_message::AssistantMessageOpenedFact::new(
        attempt_id(ATTEMPT),
        step,
        parsed(MESSAGE),
    );
    crate::session_message::record_opened(
        &tx,
        &session(),
        &parsed("evt_033602b2o001"),
        writer,
        RESERVED_MS + 1,
        &fact,
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok(())
}

/// One attempt's open text block and its newest checkpoint.
fn stream_text(store: &mut EventStore, attempt: &str, tag: &str, artifact: &str) -> TestResult {
    use grapt_event::session_message::{checkpoint::ContentCheckpointedFact, ContentBlockKind};
    let writer = token(store)?;
    let tx = store.governed_transaction()?;
    let opened = grapt_event::session_message::ContentBlockOpenedFact::new(
        attempt_id(attempt),
        parsed(MESSAGE),
        0,
        ContentBlockKind::Text,
    );
    crate::session_message::record_block_opened(
        &tx,
        &session(),
        &parsed(&format!("evt_033602b2b{tag}")),
        writer,
        RESERVED_MS + 2,
        &opened,
        &mut [],
    )?;
    let saved =
        ContentCheckpointedFact::new(attempt_id(attempt), parsed(MESSAGE), 0, text_ref(artifact));
    crate::session_message::record_checkpointed(
        &tx,
        &session(),
        &parsed(&format!("evt_033602b2c{tag}")),
        writer,
        RESERVED_MS + 3,
        &saved,
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok(())
}

/// The artifact of every text item the Step's message holds, in order.
fn promoted(store: &EventStore) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut statement = store
        .connection()
        .prepare("SELECT data FROM event WHERE type = 'session.content.appended.1' ORDER BY seq")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut artifacts = Vec::new();
    for data in rows {
        let value = serde_json::from_str(&data?)?;
        let fact = grapt_event::session_message::decode_appended(&value)
            .map_err(|error| format!("{error:?}"))?;
        artifacts.push(fact.item().payload().id().as_str().to_owned());
    }
    Ok(artifacts)
}

#[test]
fn process_loss_promotes_the_active_attempts_text_once_whichever_receipt_settles_first(
) -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &receipt_at(ATTEMPT, 1))?;
    reserve(&mut store, &receipt_at(RETRY, 2))?;
    permit(&mut store, &attempt_id(ATTEMPT), PERMIT_MS)?;
    permit(&mut store, &attempt_id(RETRY), PERMIT_MS + 1)?;
    open_message(&mut store)?;
    stream_text(&mut store, ATTEMPT, "001", "art_discarded")?;
    stream_text(&mut store, RETRY, "002", "art_active")?;

    // The older receipt first, as an attempt-id scan may visit it.
    settle(&mut store, &attempt_id(ATTEMPT))?;
    settle(&mut store, &attempt_id(RETRY))?;
    assert_eq!(promoted(&store)?, ["art_active"]);

    // A second recovery pass reuses the terminal and promotes nothing again.
    assert!(matches!(
        settle(&mut store, &attempt_id(ATTEMPT))?,
        AttemptSettlement::AlreadySettled
    ));
    assert!(matches!(
        settle(&mut store, &attempt_id(RETRY))?,
        AttemptSettlement::AlreadySettled
    ));
    assert_eq!(promoted(&store)?, ["art_active"]);
    assert_eq!(terminal_rows(&store)?, 1);
    Ok(())
}

#[test]
fn process_loss_never_resurrects_text_a_retry_discarded_before_opening_its_own() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &receipt_at(ATTEMPT, 1))?;
    permit(&mut store, &attempt_id(ATTEMPT), PERMIT_MS)?;
    open_message(&mut store)?;
    stream_text(&mut store, ATTEMPT, "001", "art_discarded")?;
    // The retry is reserved and sent but has streamed nothing yet.
    reserve(&mut store, &receipt_at(RETRY, 2))?;
    permit(&mut store, &attempt_id(RETRY), PERMIT_MS + 1)?;

    settle(&mut store, &attempt_id(ATTEMPT))?;
    settle(&mut store, &attempt_id(RETRY))?;
    assert!(promoted(&store)?.is_empty());
    assert_eq!(terminal_rows(&store)?, 1);
    Ok(())
}

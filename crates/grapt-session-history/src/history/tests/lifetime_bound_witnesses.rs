// SC-HIST-01: a Session longer than the page fold's bound still has lifetime
// totals. `fold_bounded` reads it under the caller's bounds, with the page
// fold's own semantics, and still refuses past them.

/// One opened message whose attempt reports usage at both ends of `filler`
/// unowned rows; the last usage row is the message's newest commit.
fn lifetime_rows(filler: usize) -> rusqlite::Connection {
    let conn = stored(&[]);
    let attempt = match PhysicalAttemptId::new("pat_long") {
        Some(attempt) => attempt,
        None => panic!("pat_long is a valid attempt identity"),
    };
    let step = match StepNumber::new(1) {
        Some(step) => step,
        None => panic!("step 1 is valid"),
    };
    let message: MessageId = match "msg_long".parse() {
        Ok(message) => message,
        Err(error) => panic!("msg_long is a valid message identity: {error}"),
    };
    let opened = sm::encode_opened(&AssistantMessageOpenedFact::new(
        attempt.clone(),
        step,
        message,
    ));
    let observed = |count: u64| {
        let field = match TokenCount::new(count) {
            Ok(count) => UsageField::reported(count),
            Err(error) => panic!("the fixture count is valid: {error}"),
        };
        let usage = AttemptUsage::observed(
            UsageField::ABSENT,
            field.clone(),
            field,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        );
        sm::encode_observed(&UsageObservedFact::new(attempt.clone(), usage))
    };
    let mut rows = vec![
        ("session.assistant-message.opened.1", opened.to_string()),
        ("session.usage.observed.2", observed(7).to_string()),
    ];
    rows.extend((0..filler).map(|_| ("session.request.context.1", "{}".to_owned())));
    rows.push(("session.usage.observed.2", observed(11).to_string()));
    for (seq, (kind, data)) in rows.iter().enumerate() {
        let seq = match i64::try_from(seq) {
            Ok(seq) => seq,
            Err(error) => panic!("the fixture sequence fits i64: {error}"),
        };
        if let Err(error) = conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
             VALUES (?1, 'session', 'ses_df0a', ?2, 100 + ?2, ?3, ?4)",
            rusqlite::params![format!("evt_long_{seq}"), seq, kind, data],
        ) {
            panic!("the lifetime row must insert: {error}");
        }
    }
    conn
}

fn lifetime_answer(history: &MessageHistory) -> (Vec<Option<i64>>, String) {
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the lifetime fixture must project: {error}"),
    };
    let updated = messages.iter().map(|message| message.updated_ms).collect();
    (
        updated,
        format!("{:?}", history.usage_attempts().collect::<Vec<_>>()),
    )
}

#[test]
fn a_bounded_lifetime_fold_answers_as_the_page_fold_does() {
    let conn = lifetime_rows(16);
    let store = SessionHistoryStore::new(&conn);
    let page = match store.fold(&session()) {
        Ok(history) => history,
        Err(error) => panic!("a short Session must fold: {error}"),
    };
    let bounded = match store.fold_bounded(&session(), MAX_HISTORY_ROWS, usize::MAX) {
        Ok(history) => history,
        Err(error) => panic!("a short Session must fold under explicit bounds: {error}"),
    };
    assert_eq!(lifetime_answer(&bounded), lifetime_answer(&page));
    assert_eq!(lifetime_answer(&bounded).0, vec![Some(100 + 16 + 2)]);
}

#[test]
fn a_session_past_the_page_bound_keeps_lifetime_totals_and_its_own_refusal() {
    // Rows = opened + two usage + filler: one past the page fold's bound.
    let total = MAX_HISTORY_ROWS + 1;
    let conn = lifetime_rows(total - 3);
    let store = SessionHistoryStore::new(&conn);
    assert!(
        matches!(
            store.fold(&session()),
            Err(HistoryReadError::TooLong { .. })
        ),
        "the transcript page fold must still refuse a history it cannot page"
    );
    let history = match store.fold_bounded(&session(), total, usize::MAX) {
        Ok(history) => history,
        Err(error) => panic!("lifetime totals must not inherit the page bound: {error}"),
    };
    let last = i64::try_from(total - 1).unwrap_or(i64::MAX);
    assert_eq!(lifetime_answer(&history).0, vec![Some(100 + last)]);
    let observations: usize = history
        .usage_attempts()
        .map(|(_, observations)| observations.len())
        .sum();
    assert_eq!(observations, 2, "both ends of the history were read");

    assert!(
        matches!(
            store.fold_bounded(&session(), total - 1, usize::MAX),
            Err(HistoryReadError::WindowTooLong { limit, .. }) if limit == total - 1
        ),
        "a lifetime read past its own bound must refuse, never truncate"
    );
    assert!(
        matches!(
            store.fold_bounded(&session(), total, 1024),
            Err(HistoryReadError::WindowDataTooLong { limit: 1024, .. })
        ),
        "a lifetime read past its byte bound must refuse"
    );
}

/// `count` valid 512-byte rows, then a row no read can map: a fold that
/// reaches it answers a store error instead of its own byte refusal.
fn padded_rows(count: usize) -> rusqlite::Connection {
    let conn = stored(&[]);
    let padded = serde_json::json!({ "pad": "a".repeat(512 - 10) }).to_string();
    assert_eq!(padded.len(), 512);
    for seq in 0..count {
        let seq = i64::try_from(seq).unwrap_or(i64::MAX);
        if let Err(error) = conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
             VALUES (?1, 'session', 'ses_df0a', ?2, 0, 'session.request.context.1', ?3)",
            rusqlite::params![format!("evt_pad_{seq}"), seq, padded],
        ) {
            panic!("the padded row must insert: {error}");
        }
    }
    if let Err(error) = conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
         VALUES ('evt_unmappable', 'session', 'ses_df0a', ?1, 'late', 'session.request.context.1', '{}')",
        [i64::try_from(count).unwrap_or(i64::MAX)],
    ) {
        panic!("the unmappable row must insert: {error}");
    }
    conn
}

#[test]
fn a_lifetime_read_refuses_its_byte_bound_before_holding_past_it() {
    let conn = padded_rows(128);
    let store = SessionHistoryStore::new(&conn);
    assert!(
        matches!(
            store.fold_bounded(&session(), MAX_HISTORY_ROWS, usize::MAX),
            Err(HistoryReadError::Store { .. })
        ),
        "an unbounded read reaches the unmappable row"
    );
    // Two rows fit 1024 bytes; the third is charged as it arrives and the
    // read stops there, never reaching the rows after it.
    assert!(
        matches!(
            store.fold_bounded(&session(), MAX_HISTORY_ROWS, 1024),
            Err(HistoryReadError::WindowDataTooLong { limit: 1024, .. })
        ),
        "the byte bound must refuse while the rows are read, not after"
    );
    // The windowed read the compaction projection uses (SC-COMPACT-02)
    // charges the same way: its window holds the unmappable row too.
    assert!(
        matches!(
            store.fold_window_from_bounded(
                &session(),
                grapt_types::EventSeq::new(0),
                grapt_types::EventSeq::new(129),
                MAX_HISTORY_ROWS,
                1024,
            ),
            Err(HistoryReadError::WindowDataTooLong { limit: 1024, .. })
        ),
        "the windowed byte bound must refuse while the rows are read, not after"
    );
    // A row past the bound by itself is the same refusal, so a reader
    // degrading on the bound degrades on it too.
    assert!(
        matches!(
            store.fold_bounded(&session(), MAX_HISTORY_ROWS, 511),
            Err(HistoryReadError::WindowDataTooLong { limit: 511, .. })
        ),
        "a row the query withheld must refuse as the byte bound"
    );
}

/// SC-COMPACT-01 fix 1: the page fold guards each cell before it copies it.
/// The cell is invalid UTF-8 past the page fold's byte bound: a fold that
/// copied it first would answer a conversion error, not its byte refusal.
#[test]
fn the_page_fold_refuses_an_oversize_cell_without_copying_it() {
    let conn = stored(&[]);
    let bound = i64::try_from(MAX_HISTORY_DATA_BYTES).unwrap_or(i64::MAX);
    if let Err(error) = conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
         VALUES ('evt_oversize', 'session', 'ses_df0a', 0, 0, 'session.request.context.1',
                 CAST(x'FF' || zeroblob(?1) AS TEXT))",
        [bound],
    ) {
        panic!("the oversize row must insert: {error}");
    }
    let store = SessionHistoryStore::new(&conn);
    assert!(
        matches!(
            store.fold(&session()),
            Err(HistoryReadError::WindowDataTooLong { limit, .. }) if limit == MAX_HISTORY_DATA_BYTES
        ),
        "the page fold must refuse an oversize cell before it copies it"
    );
}

fn large_request_cache_row(
    version: u32,
) -> Result<rusqlite::Connection, Box<dyn std::error::Error>> {
    let conn = lifetime_rows(0);
    let hash = "a".repeat(64);
    let data = serde_json::json!({
        "attempt": "pat_diagnostic", "step": 1,
        "fingerprint": {
            "target": hash, "cacheKey": hash, "routing": hash,
            "tools": hash, "instructions": hash, "options": hash, "input": hash,
            "inputItems": grapt_types::request_cache::MAX_PREFIX_ITEMS,
            "prefixes": vec![hash.clone(); grapt_types::request_cache::MAX_PREFIX_ITEMS]
        }
    });
    grapt_event::request_cache::decode(&data)?;
    let raw = data.to_string();
    assert!(raw.len() > 4096 && raw.len() < grapt_event::request_cache::MAX_RECORD_BYTES);
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
         VALUES ('evt_diagnostic', 'session', 'ses_df0a', 3, 103, ?1, ?2)",
        rusqlite::params![format!("session.request.cache.{version}"), raw],
    )?;
    Ok(conn)
}

#[test]
fn request_diagnostics_keep_the_row_bound_without_consuming_conversation_bytes(
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = large_request_cache_row(1)?;
    let store = SessionHistoryStore::new(&conn);
    let expected = lifetime_answer(&SessionHistoryStore::new(&lifetime_rows(0)).fold(&session())?);
    assert_eq!(lifetime_answer(&store.fold(&session())?), expected);
    assert_eq!(
        lifetime_answer(&store.fold_bounded(&session(), 4, 4096)?),
        expected
    );
    assert_eq!(
        lifetime_answer(&store.fold_window_from_bounded(
            &session(),
            grapt_types::EventSeq::new(0),
            grapt_types::EventSeq::new(4),
            4,
            4096,
        )?),
        expected
    );
    assert!(matches!(
        store.fold_bounded(&session(), 3, 4096),
        Err(HistoryReadError::WindowTooLong { limit: 3, .. })
    ));
    Ok(())
}

#[test]
fn request_diagnostics_do_not_bypass_the_page_payload_bound(
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = large_request_cache_row(1)?;
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
         VALUES ('evt_oversize_after_diagnostic', 'session', 'ses_df0a', 4, 104,
                 'session.request.context.1', CAST(x'FF' || zeroblob(?1) AS TEXT))",
        [i64::try_from(MAX_HISTORY_DATA_BYTES)?],
    )?;
    assert!(
        matches!(
            SessionHistoryStore::new(&conn).fold(&session()),
            Err(HistoryReadError::WindowDataTooLong { limit, .. }) if limit == MAX_HISTORY_DATA_BYTES
        ),
        "the diagnostic exemption must preserve the next row's refusal before copying invalid UTF-8"
    );
    Ok(())
}

#[test]
fn request_diagnostics_do_not_hide_later_owned_corruption() -> Result<(), Box<dyn std::error::Error>>
{
    let conn = large_request_cache_row(1)?;
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
         VALUES ('evt_broken', 'session', 'ses_df0a', 4, 104,
                 'session.usage.observed.2', '{not json')",
        [],
    )?;
    let store = SessionHistoryStore::new(&conn);
    for result in [
        store.fold(&session()),
        store.fold_bounded(&session(), 5, 4096),
    ] {
        let history = result?;
        assert!(
            history.omissions().iter().any(|omission| omission.seq == 4),
            "a corrupt owned row was hidden though the read succeeded: {:?}",
            history.omissions()
        );
    }
    Ok(())
}

#[test]
fn future_request_diagnostic_versions_do_not_get_a_materialization_exemption(
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = large_request_cache_row(2)?;
    assert!(matches!(
        SessionHistoryStore::new(&conn).fold_bounded(&session(), 4, 4096),
        Err(HistoryReadError::WindowDataTooLong { limit: 4096, .. })
    ));
    Ok(())
}

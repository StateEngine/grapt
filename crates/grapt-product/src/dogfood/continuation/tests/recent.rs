//! SESSION-RECENT-PARITY-01: unit witnesses for the bounded `recent`
//! projection. Each counterexample fabricates the committed state a
//! corrupt or over-bound Session could hold, through the store's own
//! schema and its own fence trigger, and asserts the projection refuses
//! with its typed product error instead of publishing fabricated parity.
//! The positive parity itself is pinned by the oracle corpus in
//! `corpus_pins_the_oracle_selection` and by the headless
//! `recent_parity` witness.

use super::super::recent::{
    estimate_tokens, prepare_recent, recent_chain, select_recent, serialize_assistant,
    truncate_tool_output_units, ConversationEntry, Part, MAX_RECENT_MATERIALIZATION_BYTES,
    MAX_RECENT_WINDOW_ROWS,
};
use super::super::retained::{Selection, MAX_SUMMARY_BYTES, RETENTION_POLICY};
use super::*;
use grapt_api::u3a::Utf16Text;
use grapt_types::{
    CompactionDescriptor, CompactionDescriptorParts, CompactionId, CompactionSummary,
    CompactionTrigger, ContextEpoch, ContextWindowId, NextRequestRef, RetainedHistory,
    RetentionPolicyVersion,
};

/// A second connection over the fixture database: raw rows are corruption
/// evidence, fabricated exactly as a hostile or drifting writer would
/// leave them, with the schema's own fence trigger satisfied.
pub(super) fn raw_conn(fixture: &Fixture) -> Result<rusqlite::Connection, String> {
    rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())
}

fn max_session_seq(conn: &rusqlite::Connection, session: &SessionId) -> Result<u64, String> {
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    u64::try_from(seq).map_err(|_| "fixture session head is negative".to_owned())
}

/// One oversized first turn under the exchange's own rows: the promotion
/// lands at seq 1, so the 40_000-character user row that breaks the keep
/// walk must sit at seq 0 or the two promoted rows would share a sequence.
fn seed_promoted_rows(conn: &rusqlite::Connection, session: &SessionId) -> Result<(), String> {
    let huge = "a".repeat(40_000);
    conn.execute(
        "INSERT INTO session_message (id, session_id, type, text, delivery, seq)
         VALUES (?1, ?2, 'user', ?3, 'queue', ?4)",
        rusqlite::params!["msg_srp_seed_huge", session.as_str(), huge.as_str(), 0i64],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// One raw durable terminal the fold cannot bind, written with the
/// schema's own fence satisfied.
fn insert_terminal(
    conn: &rusqlite::Connection,
    session: &SessionId,
    id: &str,
    attempt: &str,
    call: &str,
    seq: i64,
) -> Result<(), String> {
    let data = format!(
        "{{\"attempt\":\"{attempt}\",\"call\":\"{call}\",\"outcome\":{{\"kind\":\"completed\"}}}}"
    );
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         VALUES (?1, 'session', ?2, ?3, 1_788_220_800_001, 'session.tool.terminal.1', ?4, 1)",
        rusqlite::params![id, session.as_str(), seq, data],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn capture(
    artifacts: &FilesystemArtifactStore,
    text: &str,
) -> Result<grapt_types::ArtifactRef, String> {
    let bound = CaptureBound::new(MAX_SUMMARY_BYTES, 4096).ok_or("capture bound")?;
    let captured = artifacts
        .capture(
            std::io::Cursor::new(text.as_bytes()),
            bound,
            ArtifactKind::CapturedOutput,
            Sensitivity::Ordinary,
        )
        .map_err(|error| error.to_string())?;
    if !captured.disposition().is_complete() {
        return Err("fixture capture truncated".to_owned());
    }
    Ok(captured.reference().clone())
}

/// One committed anchor: a settled compaction whose selection binds the
/// named cut, so the chain's retained lineage starts where the witness says.
pub(super) fn anchor_descriptor(
    fixture: &Fixture,
    compaction: &str,
    from: u64,
    source_head: u64,
) -> Result<CompactionDescriptor, String> {
    let summary = capture(fixture.artifacts()?, "Summarized the first window.")?;
    let sel = capture(
        fixture.artifacts()?,
        &Selection {
            session: fixture.session.clone(),
            input: fixture.input.clone(),
            compaction: CompactionId::new(compaction).ok_or("compaction id")?,
            from,
            source_head,
            admission: None,
            server: None,
            summary_plan: None,
        }
        .encode()
        .to_string(),
    )?;
    CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new(compaction).ok_or("compaction id")?,
        source_session: fixture.session.clone(),
        source_epoch: ContextEpoch::FIRST,
        target_window: ContextWindowId::new("srp-window").ok_or("window")?,
        target_epoch: ContextEpoch::new(1),
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new(RETENTION_POLICY).ok_or("policy")?,
        summary: CompactionSummary::ClientReadable { artifact: summary },
        retained: RetainedHistory::DeterministicSelection {
            descriptor: sel.clone(),
        },
        baseline: sel.clone(),
        next_request: NextRequestRef::new(StepNumber::new(1).ok_or("step")?, sel),
    })
    .map_err(|error| format!("anchor descriptor: {error:?}"))
}

/// Commits the descriptor through the store's own one-append door, exactly
/// the way a settled compaction lands.
pub(super) fn commit_descriptor(
    fixture: &Fixture,
    event: &str,
    descriptor: &CompactionDescriptor,
) -> Result<(), String> {
    let id: EventId = event
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let mut inbox = lock(fixture.inbox()?);
    let aggregate = grapt_store::AggregateId::Session(fixture.session.clone());
    let held = inbox.store().held_writers();
    let tx = inbox
        .store_mut()
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    grapt_store::compaction::record(
        &tx,
        &id,
        held.token(&aggregate).map_err(|error| error.to_string())?,
        1_788_220_800_001,
        grapt_store::compaction::CompactionAttempt::Settled(descriptor),
        &mut [],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(())
}

#[test]
fn pending_call_serializes_to_its_call_line_only() -> Result<(), String> {
    let fixture = Fixture::new("srp-pending")?;
    {
        let conn = raw_conn(&fixture)?;
        seed_promoted_rows(&conn, &fixture.session)?;
    }
    fixture.record_incomplete_exchange()?;
    commit_descriptor(
        &fixture,
        "evt_srp_anchor_pending",
        &anchor_descriptor(&fixture, "srp-anchor-pending", 0, 2)?,
    )?;
    let head = {
        let conn = raw_conn(&fixture)?;
        max_session_seq(&conn, &fixture.session)?
    };
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, head)?;
    assert_eq!(
        recent.as_str().as_deref(),
        Some("[User]: use the tool\n\n[Assistant tool call]: read({\"path\":\"fixture.txt\"})"),
        "the pending call must publish its call line only"
    );
    Ok(())
}

#[test]
fn completed_exchange_serializes_call_and_result_lines() -> Result<(), String> {
    let fixture = Fixture::new("srp-completed")?;
    {
        let conn = raw_conn(&fixture)?;
        seed_promoted_rows(&conn, &fixture.session)?;
    }
    fixture.record_exchange()?;
    commit_descriptor(
        &fixture,
        "evt_srp_anchor_completed",
        &anchor_descriptor(&fixture, "srp-anchor-completed", 0, 2)?,
    )?;
    let head = {
        let conn = raw_conn(&fixture)?;
        max_session_seq(&conn, &fixture.session)?
    };
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, head)?;
    assert_eq!(
        recent.as_str().as_deref(),
        Some(
            "[User]: use the tool\n\n[Assistant tool call]: read({\"path\":\"fixture.txt\"})\n[Tool result]: fixture output"
        ),
        "the settled exchange must publish call and result lines"
    );
    Ok(())
}

#[test]
fn missing_artifact_refuses_the_projection() -> Result<(), String> {
    let fixture = Fixture::new("srp-missing-artifact")?;
    fixture.record_exchange()?;
    std::fs::remove_dir_all(fixture.artifact_root.join("objects"))
        .map_err(|error| format!("fixture artifact objects survived removal: {error}"))?;
    let head = {
        let conn = raw_conn(&fixture)?;
        max_session_seq(&conn, &fixture.session)?
    };
    let inbox = lock(fixture.inbox()?);
    let error = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, head)
        .err()
        .ok_or_else(|| "the projection published despite missing artifacts".to_owned())?;
    assert!(
        error.contains("artifact"),
        "the refusal lost its typed artifact evidence: {error}"
    );
    Ok(())
}

#[test]
fn negative_sequence_promoted_row_refuses_the_window() -> Result<(), String> {
    let fixture = Fixture::new("srp-negative-seq")?;
    {
        let conn = raw_conn(&fixture)?;
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, text, delivery, seq)
             VALUES ('msg_srp_negative', ?1, 'user', 'negative', 'queue', -1)",
            [fixture.session.as_str()],
        )
        .map_err(|error| error.to_string())?;
    }
    let inbox = lock(fixture.inbox()?);
    let error = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, 0)
        .err()
        .ok_or_else(|| "a negative promoted sequence projected instead of refusing".to_owned())?;
    assert!(
        error.contains("negative"),
        "the refusal lost its negative-sequence evidence: {error}"
    );
    Ok(())
}

#[test]
fn over_bound_promoted_rows_refuse_the_window() -> Result<(), String> {
    let fixture = Fixture::new("srp-promoted-bound")?;
    {
        let inbox = lock(fixture.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        for seq in 1..=MAX_RECENT_WINDOW_ROWS as i64 + 1 {
            seed.user_session_message(
                &format!("msg_srp_bound_{seq}"),
                fixture.session.as_str(),
                "over bound",
                seq,
            )
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    let inbox = lock(fixture.inbox()?);
    let error = prepare_recent(
        &inbox,
        fixture.artifacts()?,
        &fixture.session,
        MAX_RECENT_WINDOW_ROWS as u64 + 1,
    )
    .err()
    .ok_or_else(|| "an over-bound promoted window projected instead of refusing".to_owned())?;
    assert!(
        error.contains(&format!("exceed their {MAX_RECENT_WINDOW_ROWS} bound")),
        "the refusal lost its promoted-bound evidence: {error}"
    );
    Ok(())
}

#[test]
fn duplicate_terminal_refuses_the_window() -> Result<(), String> {
    let fixture = Fixture::new("srp-duplicate-terminal")?;
    fixture.record_exchange()?;
    let head = {
        let conn = raw_conn(&fixture)?;
        max_session_seq(&conn, &fixture.session)?
    };
    {
        let conn = raw_conn(&fixture)?;
        insert_terminal(
            &conn,
            &fixture.session,
            "evt_srp_duplicate",
            "pat_srp_duplicate",
            "call_d2a_read",
            i64::try_from(head + 1).map_err(|_| "fixture head exceeds SQLite range".to_owned())?,
        )?;
    }
    let inbox = lock(fixture.inbox()?);
    let error = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, head + 1)
        .err()
        .ok_or_else(|| "a duplicate terminal projected instead of refusing".to_owned())?;
    assert!(
        error.contains("more than one durable terminal"),
        "the refusal lost its duplicate-terminal evidence: {error}"
    );
    Ok(())
}

#[test]
fn orphan_terminal_refuses_the_window() -> Result<(), String> {
    let fixture = Fixture::new("srp-orphan-terminal")?;
    fixture.record_exchange()?;
    let head = {
        let conn = raw_conn(&fixture)?;
        max_session_seq(&conn, &fixture.session)?
    };
    {
        let conn = raw_conn(&fixture)?;
        insert_terminal(
            &conn,
            &fixture.session,
            "evt_srp_orphan",
            "pat_srp_orphan",
            "call_srp_orphan",
            i64::try_from(head + 1).map_err(|_| "fixture head exceeds SQLite range".to_owned())?,
        )?;
    }
    let inbox = lock(fixture.inbox()?);
    let error = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, head + 1)
        .err()
        .ok_or_else(|| "an orphan terminal projected instead of refusing".to_owned())?;
    assert!(
        error.contains("has no recorded call in the recent window"),
        "the refusal lost its orphan-terminal evidence: {error}"
    );
    Ok(())
}

#[test]
fn contradictory_session_identity_refuses_the_chain() -> Result<(), String> {
    let fixture = Fixture::new("srp-contradiction")?;
    let other: SessionId = "ses_srp_other"
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    let summary = capture(fixture.artifacts()?, "Summarized the other window.")?;
    let sel = capture(
        fixture.artifacts()?,
        &Selection {
            session: other.clone(),
            input: fixture.input.clone(),
            compaction: CompactionId::new("srp-contradiction").ok_or("compaction id")?,
            from: 0,
            source_head: 1,
            admission: None,
            server: None,
            summary_plan: None,
        }
        .encode()
        .to_string(),
    )?;
    let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new("srp-contradiction").ok_or("compaction id")?,
        source_session: other.clone(),
        source_epoch: ContextEpoch::FIRST,
        target_window: ContextWindowId::new("srp-contradiction-window").ok_or("window")?,
        target_epoch: ContextEpoch::new(1),
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new(RETENTION_POLICY).ok_or("policy")?,
        summary: CompactionSummary::ClientReadable { artifact: summary },
        retained: RetainedHistory::DeterministicSelection {
            descriptor: sel.clone(),
        },
        baseline: sel.clone(),
        next_request: NextRequestRef::new(StepNumber::new(1).ok_or("step")?, sel),
    })
    .map_err(|error| format!("contradiction descriptor: {error:?}"))?;
    // A real append must stand first: the event table's FK names the
    // event_sequence row that only a governed append creates, and seq 1 is
    // already the promotion event.
    promote_test_input(fixture.inbox()?, &fixture.session, &fixture.input, "hello")?;
    {
        let conn = raw_conn(&fixture)?;
        conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES ('evt_srp_contradiction', 'session', ?1, 2, 1_788_220_800_001, 'session.compaction.ended.2', ?2, 1)",
            rusqlite::params![
                fixture.session.as_str(),
                grapt_event::session_compaction::encode(&descriptor).to_string()
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    let inbox = lock(fixture.inbox()?);
    let error = recent_chain(&inbox, fixture.artifacts()?, &fixture.session)
        .err()
        .ok_or_else(|| "a contradictory descriptor chained instead of refusing".to_owned())?;
    assert!(
        error.contains("names another Session"),
        "the refusal lost its identity-contradiction evidence: {error}"
    );
    Ok(())
}

#[test]
fn over_bound_selection_refuses_the_chain() -> Result<(), String> {
    let fixture = Fixture::new("srp-oversize-selection")?;
    let oversized = capture(fixture.artifacts()?, &"x".repeat(17_000))?;
    let summary = capture(fixture.artifacts()?, "Summarized the oversized window.")?;
    let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new("srp-oversize").ok_or("compaction id")?,
        source_session: fixture.session.clone(),
        source_epoch: ContextEpoch::FIRST,
        target_window: ContextWindowId::new("srp-oversize-window").ok_or("window")?,
        target_epoch: ContextEpoch::new(1),
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new(RETENTION_POLICY).ok_or("policy")?,
        summary: CompactionSummary::ClientReadable { artifact: summary },
        retained: RetainedHistory::DeterministicSelection {
            descriptor: oversized.clone(),
        },
        baseline: oversized.clone(),
        next_request: NextRequestRef::new(StepNumber::new(1).ok_or("step")?, oversized),
    })
    .map_err(|error| format!("oversize descriptor: {error:?}"))?;
    commit_descriptor(&fixture, "evt_srp_oversize", &descriptor)?;
    let inbox = lock(fixture.inbox()?);
    let error = recent_chain(&inbox, fixture.artifacts()?, &fixture.session)
        .err()
        .ok_or_else(|| "an over-bound selection chained instead of refusing".to_owned())?;
    assert!(
        error.contains("retained selection exceeds its artifact bound"),
        "the refusal lost its selection-bound evidence: {error}"
    );
    Ok(())
}

#[test]
fn no_publication_without_success() -> Result<(), String> {
    let fixture = Fixture::new("srp-no-publication")?;
    promote_test_input(fixture.inbox()?, &fixture.session, &fixture.input, "hello")?;
    let promoted: u64 = {
        let conn = raw_conn(&fixture)?;
        let seq: i64 = conn
            .query_row(
                "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
                rusqlite::params![fixture.session.as_str(), fixture.input.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        u64::try_from(seq).map_err(|_| "fixture promoted sequence is negative".to_owned())?
    };
    commit_descriptor(
        &fixture,
        "evt_srp_anchor_nopub",
        &anchor_descriptor(&fixture, "srp-anchor-nopub", promoted, promoted)?,
    )?;
    let head = {
        let conn = raw_conn(&fixture)?;
        max_session_seq(&conn, &fixture.session)?
    };
    {
        let conn = raw_conn(&fixture)?;
        insert_terminal(
            &conn,
            &fixture.session,
            "evt_srp_nopub_orphan",
            "pat_srp_nopub",
            "call_srp_nopub",
            i64::try_from(head + 1).map_err(|_| "fixture head exceeds SQLite range".to_owned())?,
        )?;
    }
    let compaction = CompactionId::new("srp-next").ok_or("compaction id")?;
    let error = fixture
        .continuation(fixture.input.clone())?
        .prepare_summary("hello", &compaction, 128_000)
        .err()
        .ok_or_else(|| "compaction published despite a corrupt window".to_owned())?;
    assert!(
        matches!(
            &error,
            super::super::summarization::PreparationError::Failed(_)
        ) && error
            .to_string()
            .contains("compaction recent projection failed"),
        "the refusal lost its recent-projection evidence: {error}"
    );
    let published: i64 = {
        let conn = raw_conn(&fixture)?;
        conn.query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session'
             AND aggregate_id = ?1 AND type = 'session.compaction.ended.2'",
            [fixture.session.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?
    };
    assert_eq!(
        published, 1,
        "the failed projection still published a compaction: {published}"
    );
    Ok(())
}

fn entry(seq: u64, user: bool, text: &str) -> ConversationEntry {
    ConversationEntry {
        seq,
        user,
        text: text.encode_utf16().collect(),
    }
}

/// The str form of internal units, or None for a lone surrogate.
fn utf16(units: &[u16]) -> Option<String> {
    String::from_utf16(units).ok()
}

#[test]
fn estimate_rounds_utf16_units_over_four() {
    let table = [
        (0u64, 0u64),
        (1, 0),
        (2, 1),
        (3, 1),
        (4, 1),
        (5, 1),
        (6, 2),
        (7, 2),
        (8, 2),
        (9, 2),
        (10, 3),
        (11, 3),
        (12, 3),
        (13, 3),
        (14, 4),
        (15, 4),
        (16, 4),
    ];
    for (units, expected) in table {
        let ascii: Vec<u16> = "a".repeat(units as usize).encode_utf16().collect();
        assert_eq!(estimate_tokens(&ascii), expected, "units {units}");
    }
    let emoji: Vec<u16> = "\u{1F600}\u{1F600}\u{1F600}\u{1F600}"
        .encode_utf16()
        .collect();
    assert_eq!(estimate_tokens(&emoji), 2, "one emoji is two UTF-16 units");
}

#[test]
fn truncation_cuts_in_unit_space_like_the_oracle() {
    let marker: Vec<u16> = "\n[truncated]".encode_utf16().collect();
    let at_bound: Vec<u16> = "a".repeat(2_000).encode_utf16().collect();
    assert_eq!(truncate_tool_output_units(&at_bound), at_bound);
    let over: Vec<u16> = "a".repeat(2_001).encode_utf16().collect();
    let mut expected = at_bound.clone();
    expected.extend_from_slice(&marker);
    assert_eq!(truncate_tool_output_units(&over), expected);
    let straddle: Vec<u16> = format!("{}\u{1F600}", "a".repeat(1_999))
        .encode_utf16()
        .collect();
    let mut expected_straddle = "a".repeat(1_999).encode_utf16().collect::<Vec<u16>>();
    expected_straddle.push(0xD83D);
    expected_straddle.extend_from_slice(&marker);
    assert_eq!(
        truncate_tool_output_units(&straddle),
        expected_straddle,
        "the cut splits the pair, keeping the lone high surrogate like the oracle"
    );
    let fits: Vec<u16> = format!("{}\u{1F600}", "a".repeat(1_998))
        .encode_utf16()
        .collect();
    assert_eq!(
        truncate_tool_output_units(&fits),
        fits,
        "an emoji that fits stays"
    );
}

#[test]
fn serialize_matches_the_oracle_line_rules() -> Result<(), String> {
    let units = |value: &str| -> Vec<u16> { value.encode_utf16().collect() };
    let parts = [
        Part::Text(units("done")),
        Part::Reasoning(units("")),
        Part::Reasoning(units("think")),
        Part::Tool {
            name: "read".to_owned(),
            input: r#"{"path":"a.txt"}"#.to_owned(),
            result: Some(units("aaa")),
            error: None,
        },
        Part::Tool {
            name: "read".to_owned(),
            input: r#"{"path":"b.txt"}"#.to_owned(),
            result: None,
            error: Some("Tool failed: EffectFailed".to_owned()),
        },
        Part::Tool {
            name: "shell".to_owned(),
            input: "{}".to_owned(),
            result: None,
            error: None,
        },
    ];
    let Some(text) = serialize_assistant(&parts) else {
        return Err("non-empty parts serialized to nothing".to_owned());
    };
    assert_eq!(
        utf16(&text).as_deref(),
        Some(
            "[Assistant]: done\n[Assistant reasoning]: think\n[Assistant tool call]: read({\"path\":\"a.txt\"})\n[Tool result]: aaa\n[Assistant tool call]: read({\"path\":\"b.txt\"})\n[Tool error]: Tool failed: EffectFailed\n[Assistant tool call]: shell({})"
        )
    );
    assert!(
        serialize_assistant(&[]).is_none(),
        "no parts is the oracle falsy serialize"
    );
    let Some(empty_text) = serialize_assistant(&[Part::Text(units(""))]) else {
        return Err("an empty text part serialized to nothing".to_owned());
    };
    assert_eq!(utf16(&empty_text).as_deref(), Some("[Assistant]: "));
    Ok(())
}

#[test]
fn serialize_emits_the_oracle_unit_escapes_at_the_truncation_cut() -> Result<(), String> {
    let straddle: Vec<u16> = format!("{}\u{1F600}", "a".repeat(1_999))
        .encode_utf16()
        .collect();
    let Some(text) = serialize_assistant(&[Part::Tool {
        name: "read".to_owned(),
        input: "{}".to_owned(),
        result: Some(truncate_tool_output_units(&straddle)),
        error: None,
    }]) else {
        return Err("a truncated tool result serialized to nothing".to_owned());
    };
    assert!(
        utf16(&text).is_none(),
        "a lone surrogate at the cut has no str form"
    );
    let text = Utf16Text::try_new(text).map_err(|error| error.to_owned())?;
    let wire = serde_json::to_string(&text).map_err(|error| error.to_string())?;
    assert_eq!(
        wire,
        format!(
            "\"[Assistant tool call]: read({{}})\\n[Tool result]: {}\\ud83d\\n[truncated]\"",
            "a".repeat(1_999)
        ),
        "the wire body carries the oracle's unit escapes verbatim"
    );
    Ok(())
}

#[test]
fn select_walks_pulls_back_and_applies_the_latest_user_rule() {
    let entries = vec![
        entry(0, true, "[User]: u0"),
        entry(1, false, "[Assistant]: a1"),
        entry(2, true, "[User]: u2"),
        entry(3, false, "[Assistant]: a3"),
        entry(
            4,
            false,
            format!("[Assistant]: {}", "x".repeat(40)).as_str(),
        ),
    ];
    let (head, recent, public) = select_recent(&entries, 1, &Utf16Text::empty())
        .unwrap_or_else(|error| panic!("budgeted conversation refused: {error}"));
    assert_eq!(
        utf16(&head).as_deref(),
        Some("[User]: u0\n\n[Assistant]: a1")
    );
    assert_eq!(
        utf16(&recent).as_deref(),
        Some(format!(
            "[User]: u2\n\n[Assistant]: a3\n\n[Assistant]: {}",
            "x".repeat(40)
        ))
        .as_deref()
    );
    assert_eq!(
        public.units(),
        recent.as_slice(),
        "a non-empty head publishes the recent half"
    );
    let small = vec![
        entry(0, true, "[User]: first"),
        entry(1, false, "[Assistant]: a"),
    ];
    let (head, recent, public) = select_recent(&small, 8_000, &Utf16Text::empty())
        .unwrap_or_else(|error| panic!("small conversation refused: {error}"));
    assert_eq!(utf16(&head).as_deref(), Some(""));
    assert_eq!(
        utf16(&recent).as_deref(),
        Some("[User]: first\n\n[Assistant]: a")
    );
    assert!(
        public.units().is_empty(),
        "no previous recent and no head is the summarize-recent branch"
    );
    let anchored = Utf16Text::try_new("anchored".encode_utf16().collect())
        .unwrap_or_else(|error| panic!("test anchor exceeds its bound: {error}"));
    let (head, recent, public) = select_recent(&small, 8_000, &anchored)
        .unwrap_or_else(|error| panic!("anchored conversation refused: {error}"));
    assert_eq!(utf16(&head).as_deref(), Some(""));
    assert_eq!(
        public.units(),
        recent.as_slice(),
        "a previous recent anchors the publication"
    );
    let full = vec![
        entry(0, true, "[User]: u0"),
        entry(1, false, "[Assistant]: a1"),
        entry(2, true, "[User]: u2"),
        entry(3, false, "[Assistant]: a3"),
        entry(4, true, "[User]: u4"),
    ];
    let (_, recent, public) = select_recent(&full, 8_000, &Utf16Text::empty())
        .unwrap_or_else(|error| panic!("walk-to-front refused: {error}"));
    assert_eq!(utf16(&recent).as_deref(), Some("[User]: u4"));
    assert_eq!(
        public.as_str().as_deref(),
        Some("[User]: u4"),
        "a walk to the front splits at the latest user past the front"
    );
    assert!(
        select_recent(&[], 8_000, &Utf16Text::empty()).is_err(),
        "an empty conversation is refused"
    );
}

/// F01: the 8 MiB materialization budget is charged from the stored LENGTHS before
/// any text is fetched. A single row whose length alone breaches the wall is
/// refused on the length evidence; a window whose cumulative lengths
/// breach it within 400 rows reads from a later user input; and a
/// wall-fitting window still materializes, proving the charge is the exact
/// length, not an inflated estimate.
#[test]
fn context_budget_is_charged_from_lengths_before_any_fetch() -> Result<(), String> {
    let single = Fixture::new("srp-f01-single")?;
    {
        let conn = raw_conn(&single)?;
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, text, delivery, seq)
             VALUES (?1, ?2, 'user', ?3, 'queue', 0)",
            rusqlite::params![
                "msg_srp_f01_single",
                single.session.as_str(),
                "a".repeat(MAX_RECENT_MATERIALIZATION_BYTES - 4)
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    let inbox = lock(single.inbox()?);
    let error = prepare_recent(&inbox, single.artifacts()?, &single.session, 0)
        .err()
        .ok_or_else(|| {
            "an over-bound single promoted row projected instead of refusing".to_owned()
        })?;
    assert!(
        error.contains("request context bound"),
        "the single-row refusal lost its context-bound evidence: {error}"
    );

    let cumulative = Fixture::new("srp-f01-cumulative")?;
    {
        let inbox = lock(cumulative.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        for seq in 0..400i64 {
            seed.user_session_message(
                &format!("msg_srp_f01_row_{seq}"),
                cumulative.session.as_str(),
                &"a".repeat(25_000),
                seq,
            )
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    // SC-REL-10: the whole window breaches the wall, so the read starts at
    // the earliest user input that fits and publishes the same final entry.
    let inbox = lock(cumulative.inbox()?);
    let recent = prepare_recent(&inbox, cumulative.artifacts()?, &cumulative.session, 399)
        .map_err(|error| format!("an over-bound cumulative window refused: {error}"))?;
    assert_eq!(
        recent.as_str().as_deref(),
        Some(format!("[User]: {}", "a".repeat(25_000)).as_str()),
        "the over-bound window must publish its final entry"
    );

    let fitting = Fixture::new("srp-f01-fitting")?;
    {
        let inbox = lock(fitting.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        for seq in 0..10i64 {
            seed.user_session_message(
                &format!("msg_srp_f01_fit_{seq}"),
                fitting.session.as_str(),
                &"a".repeat(25_000),
                seq,
            )
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    let inbox = lock(fitting.inbox()?);
    let recent = prepare_recent(&inbox, fitting.artifacts()?, &fitting.session, 9)
        .map_err(|error| format!("a wall-fitting window refused: {error}"))?;
    assert_eq!(
        recent.as_str().as_deref(),
        Some(format!("[User]: {}", "a".repeat(25_000)).as_str()),
        "the wall-fitting window must publish its final entry"
    );
    Ok(())
}

/// SC-COMPACT-02: promoted inputs that carry the request past its byte
/// bound are exactly the window a structural compaction projects. Charged
/// against the request's own bound the projection refused it, so automatic
/// compaction paused; the projection's own budget holds the crossing.
#[test]
fn a_promoted_input_crossing_of_the_request_bound_projects() -> Result<(), String> {
    const INPUT_BYTES: usize = 64_528;
    const INPUTS: usize = MAX_CONTEXT_BYTES / INPUT_BYTES + 1;
    const _: () = assert!(INPUTS * INPUT_BYTES > MAX_CONTEXT_BYTES);
    const _: () = assert!(INPUT_BYTES <= grapt_api::u3a::limits::MAX_PROMPT_TEXT_BYTES);
    let inputs = INPUTS;
    let crossing = Fixture::new("srp-crossing")?;
    {
        let inbox = lock(crossing.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        for seq in 0..inputs {
            seed.user_session_message(
                &format!("msg_srp_crossing_{seq}"),
                crossing.session.as_str(),
                &format!("{seq:02} {}", "a".repeat(INPUT_BYTES - 3)),
                i64::try_from(seq).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    let head = u64::try_from(inputs - 1).map_err(|error| error.to_string())?;
    let inbox = lock(crossing.inbox()?);
    let recent = prepare_recent(&inbox, crossing.artifacts()?, &crossing.session, head)
        .map_err(|error| format!("the promoted-input crossing refused: {error}"))?;
    assert_eq!(
        recent.as_str().as_deref(),
        Some(format!("[User]: {:02} {}", inputs - 1, "a".repeat(INPUT_BYTES - 3)).as_str()),
        "the crossing window must publish its final entry"
    );
    Ok(())
}

/// F06: DB-controlled identity text is refused at its explicit read bound
/// before materialization. A corrupt oversized message identity and an
/// oversized compaction event identity each refuse with the named bound
/// evidence; with the guard absent the same rows surface a different
/// error, so this pins the guard position, not just its existence.
#[test]
fn db_controlled_identity_text_refuses_its_read_bound() -> Result<(), String> {
    let over_id = Fixture::new("srp-f06-over-id")?;
    {
        let conn = raw_conn(&over_id)?;
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, text, delivery, seq)
             VALUES (?1, ?2, 'user', 'tiny', 'queue', 0)",
            rusqlite::params![format!("msg_{}", "a".repeat(300)), over_id.session.as_str()],
        )
        .map_err(|error| error.to_string())?;
    }
    let inbox = lock(over_id.inbox()?);
    let error = prepare_recent(&inbox, over_id.artifacts()?, &over_id.session, 0)
        .err()
        .ok_or_else(|| "an oversized promoted identity projected instead of refusing".to_owned())?;
    assert!(
        error.contains("recent promoted message identity exceeds its 256-byte read bound"),
        "the oversized identity refusal lost its read-bound evidence: {error}"
    );

    let over_event = Fixture::new("srp-f06-over-event")?;
    {
        let conn = raw_conn(&over_event)?;
        conn.execute(
            "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id)
             VALUES ('session', ?1, 0, NULL)",
            [over_event.session.as_str()],
        )
        .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES (?1, 'session', ?2, 0, 1_788_220_800_001, 'session.compaction.ended.2', ?3, 1)",
            rusqlite::params![format!("evt_{}", "a".repeat(300)), over_event.session.as_str(), "{}"],
        )
        .map_err(|error| error.to_string())?;
    }
    let inbox = lock(over_event.inbox()?);
    let error = recent_chain(&inbox, over_event.artifacts()?, &over_event.session)
        .err()
        .ok_or_else(|| "an oversized compaction identity walked instead of refusing".to_owned())?;
    assert!(
        error.contains("recent compaction event identity exceeds its 256-byte read bound"),
        "the oversized event identity refusal lost its read-bound evidence: {error}"
    );
    Ok(())
}

#[path = "recent/image.rs"]
mod image;

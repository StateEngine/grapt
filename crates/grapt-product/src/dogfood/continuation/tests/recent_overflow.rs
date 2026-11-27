//! SC-REL-10: a recent window past its 8 MiB materialization budget. The
//! summarizer read the whole window since the prior cut and refused with
//! "recent conversation exceeds the request context bound" even though it
//! publishes only the newest turns, so a long Session failed to compact.

use super::super::recent::{
    prepare_recent, select_recent, user_inputs_after, ConversationEntry,
    MAX_RECENT_MATERIALIZATION_BYTES, SYNTHETIC_LABEL, USER_LABEL,
};
use super::recent::raw_conn;
use super::*;
use grapt_api::u3a::Utf16Text;

/// 25_000 characters a row: 340 rows breach the budget, 320 fit it.
const ROW: usize = 25_000;

/// `count` user rows at seqs 0.., each naming its seq so the published
/// turn shows which row was chosen.
fn user_rows(fixture: &Fixture, count: i64) -> Result<Vec<ConversationEntry>, String> {
    let inbox = lock(fixture.inbox()?);
    let seed = inbox
        .store()
        .fixture_rows()
        .map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for seq in 0..count {
        let text = format!("{seq:05}{}", "a".repeat(ROW - 5));
        seed.user_session_message(
            &format!("msg_rel10_row_{seq}"),
            fixture.session.as_str(),
            &text,
            seq,
        )
        .map_err(|error| error.to_string())?;
        entries.push(ConversationEntry {
            seq: u64::try_from(seq).map_err(|error| error.to_string())?,
            user: true,
            text: format!("{USER_LABEL}{text}").encode_utf16().collect(),
        });
    }
    seed.commit().map_err(|error| error.to_string())?;
    Ok(entries)
}

/// What the unbounded oracle publishes for the whole window.
fn oracle(entries: &[ConversationEntry]) -> Result<Utf16Text, String> {
    Ok(select_recent(entries, 8_000, &Utf16Text::empty())?.2)
}

#[test]
fn a_window_just_over_the_budget_publishes_what_the_whole_window_would() -> Result<(), String> {
    let fixture = Fixture::new("rel10-over")?;
    let entries = user_rows(&fixture, 340)?;
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, 339)?;
    assert_eq!(recent, oracle(&entries)?);
    assert!(
        recent
            .as_str()
            .is_some_and(|text| text.starts_with("[User]: 00339")),
        "the newest turn must be published"
    );
    Ok(())
}

#[test]
fn a_window_under_the_budget_is_read_whole() -> Result<(), String> {
    let fixture = Fixture::new("rel10-under")?;
    let entries = user_rows(&fixture, 320)?;
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, 319)?;
    assert_eq!(recent, oracle(&entries)?);
    Ok(())
}

/// Raw promoted rows of (kind, text) at seqs 0.., and their entries.
fn promoted(fixture: &Fixture, rows: &[(&str, String)]) -> Result<Vec<ConversationEntry>, String> {
    let inbox = lock(fixture.inbox()?);
    let seed = inbox
        .store()
        .fixture_rows()
        .map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for (seq, (kind, text)) in rows.iter().enumerate() {
        seed.session_message_row(
            &format!("msg_rel10_mixed_{seq}"),
            fixture.session.as_str(),
            kind,
            text.as_str(),
            "queue",
            i64::try_from(seq).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let user = *kind == "user";
        let label = if user { USER_LABEL } else { SYNTHETIC_LABEL };
        entries.push(ConversationEntry {
            seq: u64::try_from(seq).map_err(|error| error.to_string())?,
            user,
            text: format!("{label}{text}").encode_utf16().collect(),
        });
    }
    seed.commit().map_err(|error| error.to_string())?;
    Ok(entries)
}

#[test]
fn a_recent_half_opening_on_the_read_start_is_published_whole() -> Result<(), String> {
    let fixture = Fixture::new("rel10-at-start")?;
    // Only the window from "go" fits, and the pullback from the newest
    // synthetic row stops on "go" itself: the first entry read.
    let mut rows = vec![
        ("user", "first".to_owned()),
        (
            "user",
            "b".repeat(MAX_RECENT_MATERIALIZATION_BYTES - 50_000),
        ),
        ("user", "go".to_owned()),
    ];
    rows.extend((0..4).map(|_| ("synthetic", "s".repeat(ROW))));
    let entries = promoted(&fixture, &rows)?;
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, 6)?;
    assert_eq!(recent, oracle(&entries)?);
    assert!(
        recent
            .as_str()
            .is_some_and(|text| text.starts_with("[User]: go\n\n")),
        "the recent half must open on the read start"
    );
    Ok(())
}

#[test]
fn a_latest_exchange_past_the_budget_alone_is_refused() -> Result<(), String> {
    let fixture = Fixture::new("rel10-alone")?;
    user_rows(&fixture, 10)?;
    // The last user input opens an exchange of synthetic context that
    // overflows on its own: no start after the window's fits it.
    {
        let inbox = lock(fixture.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        for seq in 10..350i64 {
            seed.session_message_row(
                &format!("msg_rel10_synthetic_{seq}"),
                fixture.session.as_str(),
                "synthetic",
                &"s".repeat(ROW),
                "queue",
                seq,
            )
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    let inbox = lock(fixture.inbox()?);
    let error = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, 349)
        .err()
        .ok_or("an exchange past the budget was published")?;
    assert_eq!(
        error,
        "the latest recent exchange exceeds the request context bound"
    );
    Ok(())
}

#[test]
fn an_input_promoted_inside_an_exchange_is_no_window_start() -> Result<(), String> {
    let fixture = Fixture::new("rel10-inside")?;
    fixture.record_exchange()?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(200_000))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(2).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("rel10-inside").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let call = ToolCallId::new("call_rel10_inside").ok_or("call")?;
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: call.clone(),
                tool: READ_TOOL.to_owned(),
                input: "{}".to_owned(),
                execution: ToolExecution::Local,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.to_string())?;
    let steer: MessageId = "msg_rel10_steer"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &steer, "also this")?;
    owner
        .commit(DurableFact::ToolTerminal {
            call,
            outcome: ToolTerminalOutcome::Completed { output: None },
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.to_string())?;
    drop(owner);
    let next: MessageId = "msg_rel10_next"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "then this")?;

    let inbox = lock(fixture.inbox()?);
    let seq_of = |id: &MessageId| -> Result<u64, String> {
        let seq: i64 = inbox
            .store()
            .connection()
            .query_row(
                "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
                (fixture.session.as_str(), id.as_str()),
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        u64::try_from(seq).map_err(|error| error.to_string())
    };
    let (steer, next) = (seq_of(&steer)?, seq_of(&next)?);
    let starts = user_inputs_after(inbox.store(), &fixture.session, 0, next)?;
    assert!(starts.contains(&next), "{starts:?} lost {next}");
    assert!(!starts.contains(&steer), "{starts:?} kept {steer}");
    Ok(())
}

#[test]
fn a_walk_that_reaches_the_read_start_is_refused() -> Result<(), String> {
    let fixture = Fixture::new("rel10-reaches")?;
    // The newest turns are small, so the walk would keep going into the
    // oversized row the read must skip: what it would keep is unknown.
    promoted(
        &fixture,
        &[
            ("user", "b".repeat(MAX_RECENT_MATERIALIZATION_BYTES - 100)),
            ("user", "small".to_owned()),
            ("synthetic", "tail".to_owned()),
        ],
    )?;
    let inbox = lock(fixture.inbox()?);
    let error = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, 2)
        .err()
        .ok_or("a walk past the read start was published")?;
    assert_eq!(
        error,
        "the latest recent exchange exceeds the request context bound"
    );
    Ok(())
}

/// The start query over one raw exchange row of `kind` holding `data`.
fn oversized_identity(label: &str, kind: &str, data: String) -> Result<String, String> {
    starts_over_row(label, kind, data)?
        .err()
        .ok_or_else(|| format!("{label}: an oversized identity was read"))
}

/// An undecodable terminal is no evidence: a non-text call in it is no
/// identity, never a column the start query fails to read.
#[test]
fn an_undecodable_terminal_identity_does_not_refuse_the_start_query() -> Result<(), String> {
    for data in [
        r#"{"attempt":1,"call":2,"message":3}"#,
        r#"{"attempt":"x","call":"y","item":{"call":4}}"#,
    ] {
        starts_over_row(
            "undecodable-terminal",
            "session.tool.terminal.1",
            data.into(),
        )??;
    }
    Ok(())
}

fn starts_over_row(
    label: &str,
    kind: &str,
    data: String,
) -> Result<Result<Vec<u64>, String>, String> {
    let fixture = Fixture::new(label)?;
    {
        let conn = raw_conn(&fixture)?;
        conn.execute(
            "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id)
             VALUES ('session', ?1, 1, NULL)",
            [fixture.session.as_str()],
        )
        .map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES (?1, 'session', ?2, 1, 1_788_220_800_001, ?3, ?4, 1)",
            rusqlite::params![format!("evt_{label}"), fixture.session.as_str(), kind, data],
        )
        .map_err(|error| error.to_string())?;
    }
    let inbox = lock(fixture.inbox()?);
    Ok(user_inputs_after(inbox.store(), &fixture.session, 0, 1))
}

#[test]
fn an_oversized_exchange_identity_is_refused_before_it_is_read() -> Result<(), String> {
    let huge = "m".repeat(1024 * 1024);
    let error = oversized_identity(
        "rel10-message-id",
        "session.assistant-message.opened.2",
        format!("{{\"message\":\"{huge}\"}}"),
    )?;
    assert_eq!(
        error,
        "recent exchange identity exceeds its 256-byte read bound"
    );
    // An oversized terminal is no terminal: it is not fetched and names no call.
    let terminal = format!("{{\"call\":\"{huge}\"}}");
    starts_over_row("rel10-call-id", "session.tool.terminal.1", terminal)??;
    Ok(())
}

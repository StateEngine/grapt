//! SESSION-RECENT-PARITY-01: the F02 compaction-window witness, split out of
//! `recent` so no new module exceeds the 1,000-line guard. The window reads
//! far past the 1_280-row tool page under the 4 MiB data wall; one row past
//! the row bound is refused on the count, before any byte is counted.

use super::super::recent::MAX_RECENT_WINDOW_ROWS;
use super::recent::capture;
use super::*;
use grapt_types::EventSeq;

/// One durable content row the fold reads for a tool call, byte-stable for a
/// five-digit call id: the wall arithmetic over the window is exact.
fn tool_content_row(call: &str, payload: &grapt_types::ArtifactRef) -> String {
    format!(
        "{{\"message\":\"msg_1\",\"item\":{{\"ordinal\":0,\"kind\":\"tool\",\"call\":\"{call}\",\"payload\":{{\"id\":\"{}\",\"length\":{},\"digest\":\"{}\"}},\"disposition\":\"complete\"}}}}",
        payload.id().as_str(),
        payload.length(),
        payload.digest().as_str()
    )
}

/// F02: the compaction window reads far past the 1_280-row tool page. The
/// 4 MiB data wall is what makes the 65_536-row window fit: a window whose
/// content rows sum to exactly the wall reads every call, each resolving
/// its name through the call-keyed identity; one row past the row bound is
/// refused on the count, before any byte is counted.
///
/// The planted rows ride the fixture's raw connection, whose bundled-SQLite
/// build enforces the event -> event_sequence foreign key on every fresh
/// connection (probe-verified): the governed append is the aggregate's only
/// row producer, so the fixture seeds the parent row the append would have
/// left, at the last planted seq, instead of relaxing any enforcement.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn compaction_window_reads_past_the_page_under_the_data_wall() -> Result<(), String> {
    let fixture = Fixture::new("srp-f02-window")?;
    let artifacts = fixture.artifacts()?;
    let payload = capture(artifacts, "ab")?;
    let row_len = tool_content_row("call_00000", &payload).len();
    let n = MAX_CONTEXT_BYTES / row_len;
    assert!(
        n >= 15_000,
        "the window shrank below the witness floor: {n} rows at {row_len} bytes"
    );
    assert!(
        u64::try_from(n)
            .map_err(|_| "window rows overflow".to_owned())?
            .saturating_mul(row_len as u64)
            <= MAX_CONTEXT_BYTES as u64,
        "the wall arithmetic exceeds the budget"
    );
    {
        let inbox = lock(fixture.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        let last_seq = i64::try_from(2 * n - 1)
            .unwrap_or_else(|_| panic!("the planted window seq exceeds i64"));
        let aggregate = grapt_store::AggregateId::Session(fixture.session.clone());
        seed.sequence_cursor(&aggregate, last_seq)
            .map_err(|error| error.to_string())?;
        for i in 0..n as i64 {
            let call = format!("call_{i:05}");
            seed.event_row(&grapt_store::EventRow {
                aggregate: &aggregate,
                id: &format!("evt_srp_wd_i_{i:05}"),
                seq: i * 2,
                created_ms: 1_788_220_800_001,
                versioned_type: "session.tool.input.started.1",
                data: &format!(
                    "{{\"sessionID\":\"{}\",\"assistantMessageID\":\"msg_1\",\"callID\":\"{call}\",\"name\":\"read\"}}",
                    fixture.session.as_str()
                ),
                writer_epoch: 1,
            })
            .map_err(|error| error.to_string())?;
            seed.event_row(&grapt_store::EventRow {
                aggregate: &aggregate,
                id: &format!("evt_srp_wd_c_{i:05}"),
                seq: i * 2 + 1,
                created_ms: 1_788_220_800_001,
                versioned_type: "session.content.appended.1",
                data: &tool_content_row(&call, &payload),
                writer_epoch: 1,
            })
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    let inbox = lock(fixture.inbox()?);
    // The byte wall is F01's law, charged in the length phase: no budget is
    // threaded here, the store's own window bound is the only wall in play.
    let calls = stored_calls_for(
        inbox.store(),
        &fixture.session,
        artifacts,
        BoundedWindow {
            from: Some(EventSeq::new(0)),
            until: Some(EventSeq::new(2 * n as u64)),
            max_rows: MAX_RECENT_WINDOW_ROWS,
        },
        None,
        None,
    )
    .map_err(|error| format!("the windowed read refused a wall-fitting window: {error}"))?;
    assert_eq!(calls.len(), n, "the windowed read lost rows");
    for held in &calls {
        assert!(
            held.tool.is_some(),
            "call {} lost its call-keyed identity",
            held.call
        );
    }

    let over = Fixture::new("srp-f02-overbound")?;
    let over_payload = capture(over.artifacts()?, "ab")?;
    {
        let inbox = lock(over.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        let last_seq = i64::try_from(MAX_RECENT_WINDOW_ROWS)
            .unwrap_or_else(|_| panic!("the row bound exceeds i64"));
        let aggregate = grapt_store::AggregateId::Session(over.session.clone());
        seed.sequence_cursor(&aggregate, last_seq)
            .map_err(|error| error.to_string())?;
        for i in 0..(MAX_RECENT_WINDOW_ROWS as i64 + 1) {
            seed.event_row(&grapt_store::EventRow {
                aggregate: &aggregate,
                id: &format!("evt_srp_wd_r_{i:05}"),
                seq: i,
                created_ms: 1_788_220_800_001,
                versioned_type: "session.content.appended.1",
                data: &tool_content_row(&format!("call_{i:05}"), &over_payload),
                writer_epoch: 1,
            })
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    let inbox = lock(over.inbox()?);
    let error = stored_calls_for(
        inbox.store(),
        &over.session,
        over.artifacts()?,
        BoundedWindow {
            from: Some(EventSeq::new(0)),
            until: Some(EventSeq::new(MAX_RECENT_WINDOW_ROWS as u64 + 1)),
            max_rows: MAX_RECENT_WINDOW_ROWS,
        },
        None,
        None,
    )
    .map_err(String::from)
    .err()
    .ok_or_else(|| "an over-bound window read instead of refusing".to_owned())?;
    assert!(
        error.contains(&format!(
            "has more than {MAX_RECENT_WINDOW_ROWS} durable tool-call rows"
        )),
        "the row-bound refusal lost its evidence: {error}"
    );
    Ok(())
}

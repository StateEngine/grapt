use super::*;
use serde_json::json;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const REQUEST: u64 = 3;
const GENERATION: u64 = 2;

fn output(schema: &str, value: Value) -> Vec<u8> {
    let response = json!({"version": {"major": 1, "minor": 0}, "request_id": REQUEST,
        "generation": GENERATION,
        "result": {"state": "output", "value": {"schema": schema, "value": value, "window": null}}});
    let body = response.to_string().into_bytes();
    let mut frame = u32::try_from(body.len())
        .unwrap_or(u32::MAX)
        .to_be_bytes()
        .to_vec();
    frame.extend(body);
    frame
}

fn stream(frames: &[Vec<u8>]) -> Result<EventStream<tokio::io::DuplexStream>, std::io::Error> {
    let (mut host, client) = tokio::io::duplex(1 << 16);
    let bytes: Vec<u8> = frames.concat();
    tokio::spawn(async move { host.write_all(&bytes).await });
    Ok(EventStream::new(client, REQUEST, GENERATION, 65_536, None))
}

#[tokio::test]
async fn frames_decode_in_order_and_a_lagged_frame_ends_the_stream() -> TestResult {
    let mut stream = stream(&[
        output(
            STREAM_SCHEMA,
            json!({"kind": "heartbeat", "head_cursor": "evcur_v1_b"}),
        ),
        output(
            STREAM_SCHEMA,
            json!({"cursor": "evcur_v1_c", "event_id": "ev_fixture",
            "kind": "mail.notice", "occurred_at_us": 5, "payload": {"message_id": "msg_fixture"}}),
        ),
        output(
            STREAM_SCHEMA,
            json!({"kind": "lagged", "missed_through_cursor": "evcur_v1_d"}),
        ),
    ])?;
    assert!(matches!(
        stream.next().await?,
        StreamFrame::Heartbeat { .. }
    ));
    assert_eq!(
        stream.resume_cursor().map(EventCursor::as_str),
        Some("evcur_v1_b")
    );
    let StreamFrame::Event(event) = stream.next().await? else {
        return Err("the event frame did not decode".into());
    };
    assert_eq!(
        (event.kind, event.cursor.as_str()),
        (EventKind::MailNotice, "evcur_v1_c")
    );
    assert!(matches!(stream.next().await?, StreamFrame::Lagged { .. }));
    assert!(matches!(stream.next().await, Err(IpcError::StreamEnded)));
    // The lagged cursor is not a resume point: events before it are gone.
    assert_eq!(
        stream.resume_cursor().map(EventCursor::as_str),
        Some("evcur_v1_c")
    );
    Ok(())
}

#[tokio::test]
async fn a_frame_outside_its_schema_ends_the_stream() -> TestResult {
    for frame in [
        output(
            "se.other/v1",
            json!({"kind": "heartbeat", "head_cursor": "evcur_v1_b"}),
        ),
        output(
            STREAM_SCHEMA,
            json!({"kind": "heartbeat", "head_cursor": "not-a-cursor"}),
        ),
        output(
            STREAM_SCHEMA,
            json!({"cursor": "evcur_v1_c", "event_id": "ev_fixture",
            "kind": "unknown.kind", "occurred_at_us": 5, "payload": {}}),
        ),
        output(
            STREAM_SCHEMA,
            json!({"cursor": "evcur_v1_c", "event_id": "ev_fixture",
            "kind": "mail.notice", "occurred_at_us": 5, "payload": null}),
        ),
    ] {
        let mut stream = stream(&[frame])?;
        assert!(matches!(stream.next().await, Err(IpcError::Malformed(_))));
        assert!(matches!(stream.next().await, Err(IpcError::StreamEnded)));
        assert_eq!(stream.resume_cursor(), None);
    }
    Ok(())
}

#[tokio::test]
async fn a_cancelled_next_loses_no_bytes_of_the_frame_it_awaited() -> TestResult {
    let (mut host, client) = tokio::io::duplex(1 << 16);
    let mut stream = EventStream::new(client, REQUEST, GENERATION, 65_536, None);
    let heartbeat = output(
        STREAM_SCHEMA,
        json!({"kind": "heartbeat", "head_cursor": "evcur_v1_b"}),
    );
    let (head, tail) = heartbeat.split_at(6);
    // Cancelled while idle, then again partway through a frame.
    for part in [&[][..], head] {
        host.write_all(part).await?;
        let cancelled = tokio::time::timeout(Duration::ZERO, stream.next()).await;
        assert!(cancelled.is_err());
    }
    host.write_all(tail).await?;
    assert!(matches!(
        stream.next().await?,
        StreamFrame::Heartbeat { .. }
    ));
    Ok(())
}

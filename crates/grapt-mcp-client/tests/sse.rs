use grapt_mcp_client::{SseDecodeError, SseDecoder, SseEvent};
use std::error::Error;
use std::time::Duration;

type TestResult = Result<(), Box<dyn Error>>;

fn feed(decoder: &mut SseDecoder, bytes: &[u8]) -> Result<Vec<SseEvent>, SseDecodeError> {
    assert!(bytes.len() <= 8192, "test input exceeded its fixture bound");
    let mut events = Vec::new();
    for &byte in bytes {
        if let Some(event) = decoder.push(byte)? {
            events.push(event);
        }
    }
    Ok(events)
}

#[test]
fn split_utf8_bom_crlf_and_multiline_data() -> TestResult {
    let mut decoder = SseDecoder::new(256)?;
    let events = feed(
        &mut decoder,
        "\u{feff}: heartbeat\r\nid: cursor-1\r\nevent: message\r\ndata: {\r\ndata: \"text\": \"é😀\"}\r\n\r\n".as_bytes(),
    )?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "{\n\"text\": \"é😀\"}");
    assert_eq!(events[0].event_type, "message");
    assert_eq!(events[0].id.as_deref(), Some("cursor-1"));
    decoder.finish()?;
    Ok(())
}

#[test]
fn mixed_line_terminators_and_many_coalesced_events() -> TestResult {
    let mut decoder = SseDecoder::new(1)?;
    let events = feed(&mut decoder, b"data: a\r\rdata: b\n\ndata: c\r\n\r\n")?;
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].data, "a");
    assert_eq!(events[1].data, "b");
    assert_eq!(events[2].data, "c");
    decoder.finish()?;
    Ok(())
}

#[test]
fn cursor_commits_only_at_an_event_boundary() -> TestResult {
    let mut decoder = SseDecoder::new(128)?;
    feed(&mut decoder, b"id: delivered\ndata: one\n\n")?;
    feed(&mut decoder, b"id: not-delivered\ndata: two\n")?;
    assert_eq!(decoder.last_event_id(), Some("delivered"));
    assert_eq!(decoder.finish(), Err(SseDecodeError::Truncated));
    assert_eq!(decoder.last_event_id(), Some("delivered"));
    Ok(())
}

#[test]
fn metadata_only_events_persist_and_empty_id_resets_cursor() -> TestResult {
    let mut decoder = SseDecoder::new(128)?;
    assert!(feed(&mut decoder, b"id: first\nretry: 1200\n\n")?.is_empty());
    assert_eq!(decoder.last_event_id(), Some("first"));
    assert_eq!(decoder.retry_delay(), Some(Duration::from_millis(1200)));
    let events = feed(
        &mut decoder,
        b"data: one\n\nid: bad\0id\ndata: two\n\nid:\n\ndata:\n\n",
    )?;
    assert_eq!(events[0].id.as_deref(), Some("first"));
    assert_eq!(events[1].id.as_deref(), Some("first"));
    assert_eq!(events[2].id.as_deref(), Some(""));
    assert!(events[2].data.is_empty());
    decoder.finish()?;
    Ok(())
}

#[test]
fn fields_without_colon_and_single_space_rules() -> TestResult {
    let mut decoder = SseDecoder::new(128)?;
    let events = feed(
        &mut decoder,
        b"event: custom\ndata\ndata:  keep space\n\ndata: next\n\n",
    )?;
    assert_eq!(events[0].data, "\n keep space");
    assert_eq!(events[0].event_type, "custom");
    assert_eq!(events[1].event_type, "message");
    Ok(())
}

#[test]
fn oversized_line_without_newline_is_refused_before_further_growth() -> TestResult {
    let mut decoder = SseDecoder::new(8)?;
    for _ in 0..136 {
        assert!(decoder.push(b'x')?.is_none());
    }
    assert_eq!(decoder.push(b'x'), Err(SseDecodeError::Oversized));
    assert_eq!(decoder.push(b'\n'), Err(SseDecodeError::Oversized));
    assert_eq!(decoder.finish(), Err(SseDecodeError::Oversized));
    Ok(())
}

#[test]
fn multiline_data_bound_includes_the_separator() -> TestResult {
    let mut decoder = SseDecoder::new(3)?;
    feed(&mut decoder, b"data: abc\n")?;
    assert_eq!(
        feed(&mut decoder, b"data:\n"),
        Err(SseDecodeError::Oversized)
    );
    let mut valid = SseDecoder::new(3)?;
    let events = feed(&mut valid, b"data: a\ndata: b\n\n")?;
    assert_eq!(events[0].data, "a\nb");
    Ok(())
}

#[test]
fn malformed_utf8_and_partial_codepoint_are_distinct_from_clean_eof() -> TestResult {
    let mut decoder = SseDecoder::new(64)?;
    assert_eq!(
        feed(&mut decoder, b"data: \xff\n"),
        Err(SseDecodeError::NotUtf8)
    );
    let mut partial = SseDecoder::new(64)?;
    feed(&mut partial, b"data: \xc3")?;
    assert_eq!(partial.finish(), Err(SseDecodeError::NotUtf8));
    let mut truncated = SseDecoder::new(64)?;
    feed(&mut truncated, b"data: valid")?;
    assert_eq!(truncated.finish(), Err(SseDecodeError::Truncated));
    let mut clean = SseDecoder::new(64)?;
    clean.finish()?;
    Ok(())
}

#[test]
fn invalid_retry_is_ignored_and_overflow_does_not_clear_previous_delay() -> TestResult {
    let mut decoder = SseDecoder::new(128)?;
    feed(
        &mut decoder,
        b"retry: 42\nretry: -1\nretry: 3.5\nretry: 999999999999999999999999\n\n",
    )?;
    assert_eq!(decoder.retry_delay(), Some(Duration::from_millis(42)));
    Ok(())
}

#[test]
fn metadata_and_constructor_limits_are_enforced() -> TestResult {
    assert!(matches!(
        SseDecoder::new(0),
        Err(SseDecodeError::InvalidLimit)
    ));
    assert!(matches!(
        SseDecoder::new(1024 * 1024 + 1),
        Err(SseDecodeError::InvalidLimit)
    ));
    let mut id = SseDecoder::new(2048)?;
    let oversized_id = format!("id: {}\n", "x".repeat(1025));
    assert_eq!(
        feed(&mut id, oversized_id.as_bytes()),
        Err(SseDecodeError::Oversized)
    );
    let mut event = SseDecoder::new(2048)?;
    let oversized_event = format!("event: {}\n", "x".repeat(129));
    assert_eq!(
        feed(&mut event, oversized_event.as_bytes()),
        Err(SseDecodeError::Oversized)
    );
    Ok(())
}

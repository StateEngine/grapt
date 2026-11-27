//! The pending-frame bound is independent of transport read boundaries.
use super::*;

fn decoded(bytes: &[u8], chunk_size: usize) -> Result<Vec<String>, SseDecodeError> {
    let mut decoder = SseDecoder::new();
    let mut payloads = Vec::new();
    for chunk in bytes.chunks(chunk_size) {
        payloads.extend(decoder.feed(chunk)?);
        assert!(decoder.buffer.len() + decoder.carry.len() <= AdapterState::MAX_BYTES);
    }
    payloads.extend(decoder.finish()?);
    Ok(payloads)
}

#[test]
fn coalesced_valid_events_release_pending_capacity() {
    let expected: Vec<String> = (0..3)
        .map(|index| format!("{index}{}", "x".repeat(100 * 1024)))
        .collect();
    let bytes: Vec<u8> = expected
        .iter()
        .map(|payload| format!("data: {payload}\n\n"))
        .collect::<String>()
        .into_bytes();
    assert!(bytes.len() > AdapterState::MAX_BYTES);
    let coalesced = decoded(&bytes, bytes.len())
        .unwrap_or_else(|error| panic!("COALESCED_VALID_EVENTS_REFUSED: {error:?}"));
    assert_eq!(coalesced, expected);
    assert_eq!(decoded(&bytes, 64 * 1024), Ok(expected));
}

#[test]
fn one_oversized_frame_is_refused_under_every_chunking() {
    let bytes = format!("data: {}\n\n", "x".repeat(AdapterState::MAX_BYTES + 1)).into_bytes();
    for chunk_size in [bytes.len(), 64 * 1024, AdapterState::MAX_BYTES] {
        assert_eq!(decoded(&bytes, chunk_size), Err(SseDecodeError::Oversized));
    }
    let mut decoder = SseDecoder::new();
    assert_eq!(
        decoder.feed(&bytes[..AdapterState::MAX_BYTES]),
        Ok(Vec::new())
    );
    assert_eq!(
        decoder.feed(&bytes[AdapterState::MAX_BYTES..AdapterState::MAX_BYTES + 1]),
        Err(SseDecodeError::Oversized)
    );
    assert!(decoder.buffer.is_empty() && decoder.carry.is_empty());
    assert_eq!(
        decoder.feed(b"data: later\n\n"),
        Err(SseDecodeError::Oversized)
    );
}

#[test]
fn an_exact_bound_cr_delimiter_does_not_borrow_the_next_frame() {
    let payload = "x".repeat(AdapterState::MAX_BYTES - b"data: \r\r".len());
    let first = format!("data: {payload}\r\r");
    let bytes = format!("{first}data: next\n\n").into_bytes();
    for chunk_size in [bytes.len(), 64 * 1024, AdapterState::MAX_BYTES] {
        assert_eq!(
            decoded(&bytes, chunk_size),
            Ok(vec![payload.clone(), "next".to_owned()])
        );
    }
}

#[test]
fn bounded_internal_slices_preserve_utf8_between_coalesced_events() {
    let expected = vec!["😀".repeat(25 * 1024); 3];
    let bytes = expected
        .iter()
        .map(|payload| format!("data: {payload}\n\n"))
        .collect::<String>()
        .into_bytes();
    for chunk_size in [bytes.len(), 64 * 1024, 65_535] {
        assert_eq!(decoded(&bytes, chunk_size), Ok(expected.clone()));
    }
}

#[test]
fn only_the_initial_bom_is_ignored_across_chunks() {
    let stream = "\u{feff}data: one\n\ndata: \u{feff}two\n\n\u{feff}data: hidden\n\n";
    let expected = Ok(vec!["one".to_owned(), "\u{feff}two".to_owned()]);
    for chunk_size in 1..=stream.len() {
        assert_eq!(decoded(stream.as_bytes(), chunk_size), expected);
    }
}

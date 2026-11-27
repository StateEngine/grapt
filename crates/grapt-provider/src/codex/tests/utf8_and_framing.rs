use super::*;

// -----------------------------------------------------------------------
// Each test below witnesses one corrected decoder defect.
// -----------------------------------------------------------------------

/// A code point split across two transport chunks is an ordinary HTTP
/// event, not malformed provider output.
#[test]
fn a_code_point_split_across_chunks_is_decoded_not_refused() {
    let mut decoder = SseDecoder::new();
    let whole = "data: {\"type\":\"t\",\"delta\":\"e\u{301}\"}\n\n"
        .as_bytes()
        .to_vec();
    // Split immediately after the FIRST byte of the two-byte combining
    // acute, so the code point straddles the chunk boundary.
    let split_at = match whole.iter().position(|byte| *byte == 0xCC) {
        Some(index) => index + 1,
        None => panic!("the fixture must contain a two-byte code point"),
    };
    let first = match decoder.feed(&whole[..split_at]) {
        Ok(payloads) => payloads,
        Err(error) => panic!("a boundary is not malformed UTF-8: {error:?}"),
    };
    assert!(first.is_empty(), "no complete event yet");
    let second = match decoder.feed(&whole[split_at..]) {
        Ok(payloads) => payloads,
        Err(error) => panic!("the carried tail must complete: {error:?}"),
    };
    assert_eq!(second.len(), 1, "{second:?}");
    assert!(second[0].contains('\u{301}'), "{second:?}");
}

/// The carry is structurally bounded: three bytes is the longest prefix
/// of a UTF-8 sequence that is not yet a character.
#[test]
fn the_carry_never_exceeds_three_bytes() {
    let mut decoder = SseDecoder::new();
    let four_byte = "\u{1F600}".as_bytes();
    match decoder.feed(&four_byte[..3]) {
        Ok(payloads) => assert!(payloads.is_empty()),
        Err(error) => panic!("an incomplete sequence is carried, not refused: {error:?}"),
    }
    assert_eq!(decoder.carry.len(), 3);
    assert!(decoder.carry.len() <= SseDecoder::MAX_CARRY);
}

/// Genuinely invalid bytes are still refused — the fix must not make
/// the decoder permissive.
#[test]
fn genuinely_invalid_utf8_is_still_refused() {
    let mut decoder = SseDecoder::new();
    assert_eq!(
        decoder.feed(&[0xFF, 0xFE, 0xFD]),
        Err(SseDecodeError::NotUtf8)
    );
    assert_eq!(decoder.feed(b"data: {}\n\n"), Err(SseDecodeError::NotUtf8));
}

/// An incomplete code point outstanding at EOF is a typed refusal, not
/// a silently dropped byte.
#[test]
fn an_incomplete_code_point_at_eof_is_refused() {
    let mut decoder = SseDecoder::new();
    let two_byte = "\u{301}".as_bytes();
    match decoder.feed(&two_byte[..1]) {
        Ok(_) => {}
        Err(error) => panic!("a partial code point is carried: {error:?}"),
    }
    assert_eq!(decoder.finish(), Err(SseDecodeError::NotUtf8));
}

/// A stream that ends cleanly has nothing outstanding.
#[test]
fn a_clean_stream_finishes_without_a_carry() {
    let mut decoder = SseDecoder::new();
    match decoder.feed(b"data: {\"type\":\"t\"}\n\n") {
        Ok(payloads) => assert_eq!(payloads.len(), 1),
        Err(error) => panic!("clean frame: {error:?}"),
    }
    assert_eq!(decoder.finish(), Ok(Vec::new()));
}

/// CRLF event delimiters are standards-valid and must frame.
#[test]
fn crlf_event_delimiters_are_recognized() {
    let mut decoder = SseDecoder::new();
    let payloads = match decoder.feed(b"data: {\"type\":\"a\"}\r\n\r\n") {
        Ok(payloads) => payloads,
        Err(error) => panic!("CRLF framing: {error:?}"),
    };
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    assert_eq!(payloads[0], "{\"type\":\"a\"}");
}

/// A delimiter split across chunks — including between the CR and the
/// LF — must not be missed and must not frame early.
#[test]
fn a_delimiter_split_across_chunks_still_frames_exactly_once() {
    let whole = b"data: {\"type\":\"a\"}\r\n\r\n";
    for split in 1..whole.len() {
        let mut decoder = SseDecoder::new();
        let mut seen = Vec::new();
        match decoder.feed(&whole[..split]) {
            Ok(payloads) => seen.extend(payloads),
            Err(error) => panic!("split at {split}: {error:?}"),
        }
        match decoder.feed(&whole[split..]) {
            Ok(payloads) => seen.extend(payloads),
            Err(error) => panic!("split at {split}: {error:?}"),
        }
        assert_eq!(seen.len(), 1, "split at {split} produced {seen:?}");
    }
}

/// A lone trailing CR is undecided: it may still become CRLF, so it
/// must not close an event while it is the last byte in the buffer.
#[test]
fn a_trailing_carriage_return_waits_for_its_newline() {
    let mut decoder = SseDecoder::new();
    match decoder.feed(b"data: {\"type\":\"a\"}\r\n\r") {
        Ok(payloads) => assert!(payloads.is_empty(), "{payloads:?}"),
        Err(error) => panic!("{error:?}"),
    }
    let payloads = match decoder.feed(b"\n") {
        Ok(payloads) => payloads,
        Err(error) => panic!("{error:?}"),
    };
    assert_eq!(payloads.len(), 1, "{payloads:?}");
}

/// Mixed LF and CRLF terminators in one stream both frame.
#[test]
fn lf_and_crlf_frames_coexist() {
    let mut decoder = SseDecoder::new();
    let payloads = match decoder
        .feed(b"data: {\"type\":\"a\"}\n\ndata: {\"type\":\"b\"}\r\n\r\ndata: {\"type\":\"c\"}\n\n")
    {
        Ok(payloads) => payloads,
        Err(error) => panic!("{error:?}"),
    };
    assert_eq!(payloads.len(), 3, "{payloads:?}");
}

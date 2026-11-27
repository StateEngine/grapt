use super::support::*;
use super::*;

/// Opening a reasoning block must not renumber a live text block.
#[tokio::test]
async fn interleaved_text_and_reasoning_keep_distinct_ordinals() {
    const INTERLEAVED: &str = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"one\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"why\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"two\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
    );
    let base = serve_once(INTERLEAVED, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let opened = match adapter.open(input_for(&base, "att_interleaved")).await {
        Ok(opened) => opened,
        Err(error) => panic!("open: {error}"),
    };
    let mut stream = match opened {
        ProviderOpen::Streaming(stream) => stream,
        ProviderOpen::Failed(fragment) | ProviderOpen::FailedWithDetail { fragment, .. } => {
            panic!("expected a stream, got {fragment:?}")
        }
    };
    let fragments = drain(&mut stream).await;

    let text_blocks: Vec<u32> = fragments
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::TextDelta { block, .. } => Some(block.value()),
            _ => None,
        })
        .collect();
    let reasoning_blocks: Vec<u32> = fragments
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::ReasoningDelta { block, .. } => Some(block.value()),
            _ => None,
        })
        .collect();

    assert_eq!(
        text_blocks,
        vec![0, 0],
        "both text deltas belong to the block opened for them: {fragments:?}"
    );
    assert_eq!(reasoning_blocks, vec![1], "{fragments:?}");
    assert_ne!(text_blocks[0], reasoning_blocks[0]);

    let closes: Vec<(&'static str, u32)> = fragments
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::TextClosed { block, .. } => Some(("text", block.value())),
            AttemptFragment::ReasoningClosed { block, .. } => Some(("reasoning", block.value())),
            _ => None,
        })
        .collect();
    assert!(closes.contains(&("text", 0)), "{closes:?}");
    assert!(closes.contains(&("reasoning", 1)), "{closes:?}");

    let mut validator = StreamValidator::new();
    for fragment in &fragments {
        if validator.accept(fragment).is_err() {
            panic!("the adapter emitted an illegal sequence: {fragments:?}");
        }
    }
}

/// N active attempts succeed, N+1 is refused, and finishing one frees a
/// slot. The fixture capacity is 2 and is not production policy.
#[tokio::test]
async fn the_active_registry_bounds_and_then_frees() {
    let first_base = serve_once(TEXT_STREAM, "HTTP/1.1 200 OK").await;
    let second_base = serve_once(TEXT_STREAM, "HTTP/1.1 200 OK").await;
    let third_base = serve_once(TEXT_STREAM, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };

    let first = match adapter.open(input_for(&first_base, "att_a")).await {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a stream: {other:?}"),
    };
    let second = match adapter.open(input_for(&second_base, "att_b")).await {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a stream: {other:?}"),
    };
    assert_eq!(adapter.active_len(), 2);

    match adapter.open(input_for(&third_base, "att_c")).await {
        Err(CodexError::ActiveAttemptCapacity { capacity }) => assert_eq!(capacity, 2),
        other => panic!("expected a capacity refusal, got {other:?}"),
    }

    drop(second);
    assert_eq!(adapter.active_len(), 1);

    // A duplicate of a still-LIVE attempt is refused by identity, and the
    // refusal is a different fact from capacity: there is room now.
    match adapter.open(input_for(&third_base, "att_a")).await {
        Err(CodexError::DuplicateActiveAttempt { attempt }) => assert_eq!(attempt, "att_a"),
        other => panic!("expected a duplicate refusal, got {other:?}"),
    }

    drop(first);
    assert_eq!(adapter.active_len(), 0);

    match adapter.open(input_for(&third_base, "att_c")).await {
        Ok(ProviderOpen::Streaming(_)) => {}
        other => panic!("a freed slot must be reusable: {other:?}"),
    }
}

/// Case 1 of 3. A **successful** terminal releases capacity at the
/// terminal itself, not when the caller happens to drop the object.
#[tokio::test]
async fn a_successful_terminal_releases_capacity_before_the_object_is_dropped() {
    let base = serve_once(TEXT_STREAM, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let mut stream = match adapter.open(input_for(&base, "att_done")).await {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a stream: {other:?}"),
    };
    assert_eq!(adapter.active_len(), 1, "held while the stream is running");
    let fragments = drain(&mut stream).await;
    assert!(matches!(
        fragments.last(),
        Some(AttemptFragment::ProviderTerminal { .. })
    ));
    // The stream object is STILL HELD by this test. Capacity must already
    // be free by then: capacity release is not drop release.
    assert_eq!(
        adapter.active_len(),
        0,
        "a finished stream must not occupy capacity while the caller holds it"
    );
    // And releasing again on drop is idempotent, not a double free.
    drop(stream);
    assert_eq!(adapter.active_len(), 0);
}

/// Case 2 of 3. An **error** terminal releases capacity the same way,
/// through a different code path (`finish_with`).
#[tokio::test]
async fn an_error_terminal_releases_capacity_before_the_object_is_dropped() {
    let base = serve_once(TOOL_STREAM, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let mut stream = match adapter.open(input_for(&base, "att_err")).await {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a stream: {other:?}"),
    };
    assert_eq!(adapter.active_len(), 1);
    let fragments = drain(&mut stream).await;
    assert!(
        fragments.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderFailure {
                class: ProviderFailureClass::InvalidProviderOutput,
                ..
            }
        )),
        "{fragments:?}"
    );
    assert_eq!(
        adapter.active_len(),
        0,
        "an error terminal must free capacity while the caller still holds the stream"
    );
    drop(stream);
    assert_eq!(adapter.active_len(), 0);
}

/// Case 3 of 3. Drop remains the **cancellation backstop**: a stream
/// abandoned BEFORE any terminal still frees its slot.
#[tokio::test]
async fn dropping_an_unterminated_stream_still_releases_capacity() {
    let base = serve_once(TEXT_STREAM, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let stream = match adapter.open(input_for(&base, "att_cancel")).await {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a stream: {other:?}"),
    };
    // Never driven to a terminal — this is what cancellation looks like.
    assert_eq!(adapter.active_len(), 1);
    drop(stream);
    assert_eq!(
        adapter.active_len(),
        0,
        "Drop is the cancellation backstop and must still release"
    );
}

/// A frame whose only delimiter is a pair of carriage returns at the
/// very end of the stream frames exactly once, at every split position.
#[test]
fn a_lone_cr_delimited_frame_at_eof_frames_exactly_once() {
    let whole = b"data: {\"type\":\"a\"}\r\r";
    for split in 0..=whole.len() {
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
        match decoder.finish() {
            Ok(payloads) => seen.extend(payloads),
            Err(error) => panic!("split at {split}: {error:?}"),
        }
        assert_eq!(
            seen,
            vec!["{\"type\":\"a\"}".to_owned()],
            "split at {split} produced {seen:?}"
        );
    }
}

/// The trailing carriage return stays undecided while more
/// bytes may still arrive, so mid-stream framing is unchanged.
#[test]
fn a_trailing_cr_is_still_undecided_before_eof() {
    let mut decoder = SseDecoder::new();
    match decoder.feed(b"data: {\"type\":\"a\"}\r") {
        Ok(payloads) => assert!(payloads.is_empty(), "{payloads:?}"),
        Err(error) => panic!("{error:?}"),
    }
    // A following LF makes it CRLF, not a second terminator.
    match decoder.feed(b"\ndata: {\"type\":\"b\"}\n\n") {
        Ok(payloads) => assert_eq!(payloads.len(), 1, "{payloads:?}"),
        Err(error) => panic!("{error:?}"),
    }
}

/// An end-of-stream frame reaches the fragment alphabet, rather than
/// being reported as a truncated stream.
#[tokio::test]
async fn a_terminal_event_delimited_only_at_eof_is_decoded() {
    const CR_TERMINAL: &str = "data: {\"type\":\"response.completed\",\"response\":{}}\r\r";
    let base = serve_once(CR_TERMINAL, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let mut stream = match adapter.open(input_for(&base, "att_cr_eof")).await {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a stream: {other:?}"),
    };
    let fragments = drain(&mut stream).await;
    assert!(
        matches!(
            fragments.last(),
            Some(AttemptFragment::ProviderTerminal {
                finish: FinishReason::Stop,
                ..
            })
        ),
        "an event delimited only at EOF must not be reported as truncated: {fragments:?}"
    );
}

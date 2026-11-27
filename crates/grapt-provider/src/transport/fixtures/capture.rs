//! Capture seam over the real wire: what reaches a sink, and what happens
//! when it is saturated, failed or absent.

use super::*;

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

fn exchange() -> (
    crate::transport::capture::CaptureExchange,
    std::sync::mpsc::Receiver<AttemptCapture>,
) {
    let big = match std::num::NonZeroUsize::new(256) {
        Some(big) => big,
        None => panic!("non-zero"),
    };
    capture_exchange(big, big, big)
}

#[tokio::test]
async fn capture_receives_the_transports_bytes_and_one_terminal_notice() {
    let events = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let fixture = serve(200, &[], vec![events.clone()]).await;

    let (exchange, attempts) = exchange();
    let adapter = adapter(2).with_capture(exchange);
    let open = match adapter
        .open(input(
            "att_capture",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    assert_eq!(text_of(&fragments), "Hi");

    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("the attempt must be announced: {error}"),
    };
    // Attribution is structural: the feed names its own attempt.
    assert_eq!(announced.attempt().as_str(), "att_capture");
    let (bytes, settled) = captured(&drain_capture(&announced));
    // The RAW bytes, not the decoded fragments: capture must be able to explain
    // a normalization failure, which a post-normalization capture cannot.
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        String::from_utf8_lossy(&events)
    );
    assert_eq!(
        settled,
        Some((
            CaptureOutcome::Terminal,
            crate::transport::capture::CaptureLoss::default(),
        ))
    );
}

#[tokio::test]
async fn capture_sees_the_bytes_of_a_stream_this_adapter_refuses() {
    // The case capture exists for. The provider sends bytes that are not valid
    // UTF-8; the adapter refuses the stream, and the capture must still hold
    // what actually arrived — otherwise the only evidence of the defect is the
    // adapter's own opinion about it.
    let mut bytes = b"data: {\"type\":\"ping\"}\n\n".to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    let fixture = serve(200, &[], vec![bytes.clone()]).await;

    let (exchange, attempts) = exchange();
    let adapter = adapter(2).with_capture(exchange);
    let open = match adapter
        .open(input(
            "att_capture_bad",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    assert_eq!(
        failure_of(&fragments),
        Some(ProviderFailureClass::InvalidProviderOutput)
    );

    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    let (captured_bytes, settled) = captured(&drain_capture(&announced));
    assert_eq!(
        captured_bytes, bytes,
        "the refused bytes must still be captured"
    );
    assert_eq!(
        settled,
        Some((
            CaptureOutcome::Incomplete,
            crate::transport::capture::CaptureLoss::default(),
        )),
        "a refused stream is Incomplete, and a consumer must be able to tell"
    );
}

#[tokio::test]
async fn cancelling_a_stream_settles_its_capture_incomplete() {
    // At the wire: a dropped stream must still settle its capture, or a
    // consumer waits forever on a cancelled attempt.
    let fixture = serve(
        200,
        &[],
        vec![sse(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}"#,
            r#"{"type":"message_stop"}"#,
        ])],
    )
    .await;

    let (exchange, attempts) = exchange();
    let adapter = adapter(2).with_capture(exchange);
    let mut stream = match adapter
        .open(input(
            "att_cap_cancel",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a live stream: {other:?}"),
    };
    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    assert_eq!(
        announced.feed().try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "the capture tap must stay owned and unsettled while the stream is live"
    );
    // Read one fragment so the attempt is genuinely live, then CANCEL.
    let _ = stream.next_fragment().await;
    drop(stream);

    let (_bytes, settled) = captured(&drain_capture(&announced));
    assert_eq!(
        settled.map(|(outcome, _)| outcome),
        Some(CaptureOutcome::Incomplete),
        "a cancelled attempt must settle Incomplete, with nobody calling settle"
    );
}

#[tokio::test]
async fn a_saturated_capture_reports_end_while_the_settled_stream_stays_alive() {
    // At the wire: the one data slot is full when the terminal tries to
    // publish Finished, so the terminal event is genuinely dropped.
    // ProviderStream must relinquish its sender at settlement rather than keep
    // it inside a still-live, already-finished object.
    let events = sse(&[r#"{"type":"message_stop"}"#]);
    let fixture = serve(200, &[], vec![events.clone()]).await;
    let one = match std::num::NonZeroUsize::new(1) {
        Some(one) => one,
        None => panic!("non-zero"),
    };
    let max_chunk = match std::num::NonZeroUsize::new(events.len()) {
        Some(max_chunk) => max_chunk,
        None => panic!("the SSE fixture is non-empty"),
    };
    let (exchange, attempts) = capture_exchange(one, one, max_chunk);
    let adapter = adapter(2).with_capture(exchange);
    let mut stream = match adapter
        .open(input(
            "att_cap_saturated_terminal",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a live stream: {other:?}"),
    };
    while stream.next_fragment().await.is_some() {}

    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    assert_eq!(
        announced.feed().try_recv(),
        Ok(CaptureEvent::Chunk(events)),
        "the data event must genuinely occupy the only slot"
    );
    assert_eq!(
        announced.feed().try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected),
        "settlement must relinquish the sender even while ProviderStream stays alive"
    );
    assert!(
        stream.next_fragment().await.is_none(),
        "the settled stream remains alive for this assertion"
    );
}

#[tokio::test]
async fn a_saturated_failed_capture_reports_end_while_the_settled_stream_stays_alive() {
    let mut bytes = b"data: {\"type\":\"ping\"}\n\n".to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    let fixture = serve(200, &[], vec![bytes.clone()]).await;
    let one = match std::num::NonZeroUsize::new(1) {
        Some(one) => one,
        None => panic!("non-zero"),
    };
    let max_chunk = match std::num::NonZeroUsize::new(bytes.len()) {
        Some(max_chunk) => max_chunk,
        None => panic!("the failure fixture is non-empty"),
    };
    let (exchange, attempts) = capture_exchange(one, one, max_chunk);
    let adapter = adapter(2).with_capture(exchange);
    let mut stream = match adapter
        .open(input(
            "att_cap_saturated_failure",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a live stream: {other:?}"),
    };
    let mut fragments = Vec::new();
    while let Some(fragment) = stream.next_fragment().await {
        fragments.push(fragment);
    }
    assert_eq!(
        failure_of(&fragments),
        Some(ProviderFailureClass::InvalidProviderOutput),
        "the provider-output failure must remain observable"
    );

    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    assert_eq!(
        announced.feed().try_recv(),
        Ok(CaptureEvent::Chunk(bytes)),
        "the failed response bytes must genuinely occupy the only slot"
    );
    assert_eq!(
        announced.feed().try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected),
        "failed settlement must relinquish the sender even while ProviderStream stays alive"
    );
    assert!(
        stream.next_fragment().await.is_none(),
        "the failed, settled stream remains alive for this assertion"
    );
}
#[tokio::test]
async fn an_attempt_without_a_capture_sink_behaves_identically() {
    // Capture is a deployment decision. If attaching one changed the fragments,
    // every witness in this crate would be conditional on whether capture was
    // configured.
    let events = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"same"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_stop"}"#,
    ]);

    let without_fixture = serve(200, &[], vec![events.clone()]).await;
    let without = drain(
        match adapter(2)
            .open(input(
                "att_nocap",
                &without_fixture.base,
                ProtocolFamily::AnthropicMessages,
                anthropic_request(),
            ))
            .await
        {
            Ok(open) => open,
            Err(error) => panic!("open: {error}"),
        },
    )
    .await;

    let with_fixture = serve(200, &[], vec![events]).await;
    let (exchange, attempts) = exchange();
    let with = drain(
        match adapter(2)
            .with_capture(exchange)
            .open(input(
                "att_cap",
                &with_fixture.base,
                ProtocolFamily::AnthropicMessages,
                anthropic_request(),
            ))
            .await
        {
            Ok(open) => open,
            Err(error) => panic!("open: {error}"),
        },
    )
    .await;

    assert_eq!(format!("{without:?}"), format!("{with:?}"));
    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("announced: {error}"),
    };
    let (bytes, _) = captured(&drain_capture(&announced));
    assert!(!bytes.is_empty(), "the control must actually capture");
}

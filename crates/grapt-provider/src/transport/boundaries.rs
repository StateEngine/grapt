//! Transport boundary witnesses: crash, race, retry, cancellation.
//!
//! `#[cfg(test)]`. The parser boundary is witnessed in `crate::routes::tests`
//! and `crate::transport::fixtures`; this module covers the four that need a
//! whole attempt, several attempts at once, or a decision the runtime makes
//! from what this crate supplies.
//!
//! # Why these are here rather than left to the runtime
//!
//! The retry law is the runtime's to enforce, but its two inputs are this
//! crate's: which failure classes are retryable, and whether visible output has
//! occurred. A witness that a NEW route's mid-stream rate limit reaches the
//! runtime as a retryable class BEFORE output and as a terminal one AFTER is
//! the only place those two inputs are checked together against a real decoded
//! stream rather than against hand-built fragments.

use std::sync::Arc;

use crate::disposition::AttemptDisposition;
use crate::fragment::AttemptFragment;
use crate::normalize::StreamValidator;
use crate::request::ProtocolFamily;
use crate::routes::anthropic::AnthropicDecoder;
use crate::routes::compat::CompatibleDecoder;
use crate::routes::responses::ResponsesDecoder;
use crate::transport::blocks::BlockState;
use crate::transport::sink::{DecodeSink, ToolAccumulations};

/// Decode payloads through one route and return the fragments.
fn decode(family: ProtocolFamily, payloads: &[&str]) -> Vec<AttemptFragment> {
    let mut blocks = BlockState::default();
    let mut tools = ToolAccumulations::new();
    let mut pending = std::collections::VecDeque::new();
    let mut unmodeled = 0_u32;
    let mut terminated = false;
    let mut reported_usage = None;
    let mut usage_warning = None;
    let mut usage_invalid = false;

    let mut responses = ResponsesDecoder::new();
    let mut anthropic = AnthropicDecoder::new();
    let mut compat = if family == ProtocolFamily::WatsonxChat {
        CompatibleDecoder::watsonx(
            &grapt_types::PhysicalAttemptId::new("fixture-attempt")
                .unwrap_or_else(|| panic!("fixture attempt")),
        )
    } else {
        CompatibleDecoder::new()
    };
    let mut bedrock = crate::routes::bedrock::BedrockDecoder::with_binding(
        crate::replay::ReplayBinding::fixture(),
    );
    let mut gemini = crate::gemini::Decoder::new(
        crate::replay::ReplayBinding::fixture(),
        "fixture-attempt".into(),
        &[],
    );
    let mut cohere = crate::cohere::Decoder::new(&[], None);

    for payload in payloads {
        let mut sink = DecodeSink::new(
            &mut blocks,
            &mut pending,
            &mut tools,
            &mut unmodeled,
            &mut terminated,
            &mut reported_usage,
            &mut usage_warning,
            &mut usage_invalid,
        );
        match family {
            ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex => {
                responses.decode(payload, &mut sink)
            }
            ProtocolFamily::AnthropicMessages => anthropic.decode(payload, &mut sink),
            ProtocolFamily::VertexAnthropic => anthropic.decode(payload, &mut sink),
            ProtocolFamily::OpenAiCompatible => compat.decode(payload, &mut sink),
            ProtocolFamily::BedrockConverse => bedrock.decode(payload, &mut sink),
            ProtocolFamily::GoogleGemini => gemini.decode(payload, &mut sink),
            ProtocolFamily::CohereChat => cohere.decode(payload, &mut sink),
            ProtocolFamily::VertexGemini => gemini.decode(payload, &mut sink),
            ProtocolFamily::WatsonxChat => compat.decode(payload, &mut sink),
        }
    }
    if matches!(family, ProtocolFamily::OpenAiCompatible) {
        let mut sink = DecodeSink::new(
            &mut blocks,
            &mut pending,
            &mut tools,
            &mut unmodeled,
            &mut terminated,
            &mut reported_usage,
            &mut usage_warning,
            &mut usage_invalid,
        );
        compat.finish(&mut sink);
    }
    pending.into_iter().collect()
}

/// Run the decoded fragments through the real validator and classify the
/// failure exactly as the runtime would.
fn disposition_of(fragments: &[AttemptFragment]) -> Option<AttemptDisposition> {
    let mut validator = StreamValidator::new();
    let mut failure = None;
    // The opening law is positional: the validator expects AttemptOpened first,
    // which the stream supplies. These fragments come from a decoder directly,
    // so it is supplied here.
    let _ = validator.accept(&AttemptFragment::AttemptOpened);
    for fragment in fragments {
        if let AttemptFragment::ProviderFailure {
            class,
            classification,
            ..
        } = fragment
        {
            failure = Some((class.clone(), *classification));
        }
        let _ = validator.accept(fragment);
    }
    let (class, classification) = failure?;
    Some(AttemptDisposition::classify(
        class,
        classification,
        validator.no_visible_output(),
    ))
}

// ---------------------------------------------------------------------------
// Retry boundary
// ---------------------------------------------------------------------------

#[test]
fn a_rate_limit_before_visible_output_is_retryable_on_every_new_route() {
    // The two inputs this crate owns, checked together against a REAL decoded
    // stream rather than hand-built fragments: the class is retryable, and the
    // validator saw no visible output.
    let cases = [
        (
            ProtocolFamily::AnthropicMessages,
            vec![r#"{"type":"error","error":{"type":"rate_limit_error"}}"#],
        ),
        (
            ProtocolFamily::OpenAiCompatible,
            vec![r#"{"error":{"type":"rate_limit_exceeded"}}"#],
        ),
    ];
    for (family, payloads) in cases {
        let fragments = decode(family, &payloads);
        let disposition = match disposition_of(&fragments) {
            Some(disposition) => disposition,
            None => panic!("{family:?}: a rate limit must produce a failure"),
        };
        assert!(
            disposition.is_retry(),
            "{family:?}: a rate limit before any output must be retryable, got {disposition:?}"
        );
    }
}

#[test]
fn a_rate_limit_mid_block_retries_and_leaves_the_block_unfinished_on_every_new_route() {
    // Streamed-but-unfinished text is not visible output: as in
    // Codex, the failure drops the unfinished item and the retry rebuilds from
    // history. The failure must therefore not close the block itself — a
    // close it emitted would commit the partial and, being visible, would
    // forbid the very retry it precedes.
    let cases = [
        (
            ProtocolFamily::AnthropicMessages,
            vec![
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"shown"}}"#,
                r#"{"type":"error","error":{"type":"rate_limit_error"}}"#,
            ],
        ),
        (
            ProtocolFamily::OpenAiCompatible,
            vec![
                r#"{"choices":[{"delta":{"content":"shown"}}]}"#,
                r#"{"error":{"type":"rate_limit_exceeded"}}"#,
            ],
        ),
        (
            ProtocolFamily::OpenAiResponses,
            vec![
                r#"{"type":"response.output_text.delta","delta":"shown"}"#,
                r#"{"type":"response.failed"}"#,
            ],
        ),
    ];
    for (family, payloads) in cases {
        let fragments = decode(family, &payloads);
        assert!(
            fragments
                .iter()
                .any(|fragment| matches!(fragment, AttemptFragment::TextDelta { .. })),
            "{family:?}: the fixture must stream a partial, or it proves nothing: {fragments:?}"
        );
        assert!(
            !fragments
                .iter()
                .any(|fragment| matches!(fragment, AttemptFragment::TextClosed { .. })),
            "{family:?}: a failure that may retry must not close the partial: {fragments:?}"
        );
        let disposition = match disposition_of(&fragments) {
            Some(disposition) => disposition,
            None => panic!("{family:?}: the fixture must produce a failure"),
        };
        assert!(
            disposition.is_retry(),
            "{family:?}: a retryable failure mid-block must retry, got {disposition:?}"
        );
    }
}

#[test]
fn a_rate_limit_after_a_closed_block_is_terminal() {
    // The direction that matters more. A closed block is a committed item the
    // user has seen whole: "Once visible output exists, that Step is not
    // silently retried." A witness for the retryable direction alone would
    // pass against an implementation that retries unconditionally.
    let cases = [
        (
            ProtocolFamily::AnthropicMessages,
            vec![
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"shown"}}"#,
                r#"{"type":"content_block_stop","index":0}"#,
                r#"{"type":"error","error":{"type":"rate_limit_error"}}"#,
            ],
        ),
        (
            ProtocolFamily::OpenAiResponses,
            vec![
                r#"{"type":"response.output_text.delta","delta":"shown"}"#,
                r#"{"type":"response.output_text.done"}"#,
                r#"{"type":"response.failed"}"#,
            ],
        ),
    ];
    for (family, payloads) in cases {
        let fragments = decode(family, &payloads);
        // The visible output must be the close the PROVIDER streamed, ahead of
        // the failure, not one the failure path emitted on its own.
        let failure = fragments
            .iter()
            .position(|fragment| matches!(fragment, AttemptFragment::ProviderFailure { .. }));
        let closed = fragments
            .iter()
            .position(|fragment| matches!(fragment, AttemptFragment::TextClosed { .. }));
        assert!(
            matches!((closed, failure), (Some(closed), Some(failure)) if closed < failure),
            "{family:?}: the fixture must close its block before failing: {fragments:?}"
        );
        let disposition = match disposition_of(&fragments) {
            Some(disposition) => disposition,
            None => panic!("{family:?}: the fixture must produce a failure"),
        };
        assert!(
            disposition.is_terminal(),
            "{family:?}: a failure after visible output must be terminal, got {disposition:?}"
        );
    }
}

#[test]
fn a_failure_that_ends_the_step_still_closes_its_partial() {
    // Edge (a): only a failure the Step may retry leaves its block
    // unfinished. A terminal class keeps today's close, so the failed Step's
    // durable truth still holds what was streamed.
    let fragments = decode(
        ProtocolFamily::AnthropicMessages,
        &[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"shown"}}"#,
            r#"{"type":"error","error":{"type":"authentication_error"}}"#,
        ],
    );
    let closed = fragments
        .iter()
        .position(|fragment| matches!(fragment, AttemptFragment::TextClosed { .. }));
    let failure = fragments
        .iter()
        .position(|fragment| matches!(fragment, AttemptFragment::ProviderFailure { .. }));
    assert!(
        matches!((closed, failure), (Some(closed), Some(failure)) if closed < failure),
        "a terminal failure must close the partial before failing: {fragments:?}"
    );
}

#[test]
fn a_terminal_class_is_never_retryable_however_early_it_arrives() {
    // A hard authentication failure at position zero: the earliest possible
    // point, with no visible output at all. It must STILL be terminal, or a
    // rejected credential would be retried to the attempt cap.
    let fragments = decode(
        ProtocolFamily::AnthropicMessages,
        &[r#"{"type":"error","error":{"type":"authentication_error"}}"#],
    );
    let disposition = match disposition_of(&fragments) {
        Some(disposition) => disposition,
        None => panic!("an authentication error must produce a failure"),
    };
    assert!(
        disposition.is_terminal(),
        "no-visible-output must not launder a terminal class into a retry: {disposition:?}"
    );
}

// ---------------------------------------------------------------------------
// Crash boundary
// ---------------------------------------------------------------------------

#[test]
fn no_malformed_payload_can_panic_any_route() {
    // Red-team §B.9: malformed provider fragments are `Effect.die` crash-class
    // defects in the oracle across 30 sites, bypassing `Step.Failed`. The
    // corrected expectation is one TYPED failure, so the property is that every
    // one of these is survivable — this crate has no panicking path outside
    // `#[cfg(test)]`.
    let hostile = [
        "",
        "{",
        "null",
        "[]",
        "0",
        r#""a string, not an object""#,
        r#"{"type":null}"#,
        r#"{"type":123}"#,
        r#"{"type":"content_block_delta"}"#,
        r#"{"type":"content_block_delta","index":"not a number"}"#,
        r#"{"type":"content_block_start","index":0}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{}}"#,
        r#"{"type":"content_block_start","index":-1,"content_block":{"type":"text"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":42}}"#,
        r#"{"type":"response.output_item.added","item":null}"#,
        r#"{"type":"response.function_call_arguments.delta"}"#,
        r#"{"type":"response.completed","response":{"usage":"not an object"}}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":-5}}}"#,
        r#"{"choices":null}"#,
        r#"{"choices":[{}]}"#,
        r#"{"choices":[{"delta":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":"not an array"}}]}"#,
        r#"{"choices":[{"finish_reason":99}]}"#,
        r#"{"error":null}"#,
        r#"{"usage":{"prompt_tokens":"lots"}}"#,
    ];
    let families = [
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::AnthropicMessages,
        ProtocolFamily::OpenAiCompatible,
    ];

    for family in families {
        for payload in hostile {
            // Each in its own decoder, so one payload's terminal latch does not
            // mask the next. A panic in any of these fails the test by
            // unwinding, which is the assertion.
            let fragments = decode(family, &[payload]);
            // And whatever came out must be a legal fragment sequence — a
            // decoder that survived by emitting nonsense would pass a
            // no-panic test and still corrupt a projection.
            let mut validator = StreamValidator::new();
            let _ = validator.accept(&AttemptFragment::AttemptOpened);
            for fragment in fragments {
                if let Err(violation) = validator.accept(&fragment) {
                    panic!(
                        "{family:?} produced an illegal fragment from {payload:?}: \
                         {fragment:?} rejected as {violation:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_hostile_payload_sequence_still_reaches_at_most_one_terminal() {
    // Surviving each payload alone is weaker than surviving them in sequence:
    // the latch, the block state and the tool map all carry across payloads.
    let payloads = [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#,
        r#"{"type":"error","error":{"type":"overloaded_error"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"after"}}"#,
        r#"{"type":"message_stop"}"#,
        r#"{"type":"message_stop"}"#,
    ];
    let fragments = decode(ProtocolFamily::AnthropicMessages, &payloads);
    let terminals = fragments
        .iter()
        .filter(|fragment| fragment.is_terminal())
        .count();
    assert_eq!(
        terminals, 1,
        "a hostile sequence must still settle exactly once: {fragments:?}"
    );
}

// ---------------------------------------------------------------------------
// Race boundary
// ---------------------------------------------------------------------------

/// Concurrent attempts against one adapter, with HELD cleanup.
///
/// The cleanup is held deliberately: every stream is kept alive in `held` until
/// after the capacity assertion. A race witness that let its streams drop as it
/// went would be asserting against whatever the scheduler happened to have
/// finished, and would pass whether or not the registry bounded anything.
#[tokio::test]
async fn concurrent_attempts_never_exceed_the_injected_capacity() {
    use crate::transport::fixtures::{adapter, anthropic_request, input, serve, sse};

    const CAPACITY: usize = 3;
    const OFFERED: usize = 12;

    let fixture = serve(
        200,
        &[],
        vec![sse(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#,
        ])],
    )
    .await;
    let adapter = Arc::new(adapter(CAPACITY));

    let mut held = Vec::new();
    let mut refused = 0_usize;
    for index in 0..OFFERED {
        let opened = adapter
            .open(input(
                &format!("att_race_{index}"),
                &fixture.base,
                ProtocolFamily::AnthropicMessages,
                anthropic_request(),
            ))
            .await;
        match opened {
            Ok(crate::transport::adapter::ProviderOpen::Streaming(stream)) => {
                // HELD: not drained, not dropped. The slot stays claimed.
                held.push(stream);
            }
            Ok(crate::transport::adapter::ProviderOpen::Failed(_))
            | Ok(crate::transport::adapter::ProviderOpen::FailedWithDetail { .. }) => refused += 1,
            Err(_) => refused += 1,
        }
    }

    assert_eq!(
        adapter.active_len(),
        CAPACITY,
        "the registry must hold exactly its capacity while every stream is held"
    );
    assert_eq!(
        held.len(),
        CAPACITY,
        "no more than the capacity may be admitted at once"
    );
    assert_eq!(
        refused,
        OFFERED - CAPACITY,
        "every attempt over capacity must be refused, not queued or silently dropped"
    );

    // Only NOW release, so the assertions above described a genuinely
    // saturated adapter rather than one the scheduler had already drained.
    drop(held);
    assert_eq!(
        adapter.active_len(),
        0,
        "releasing every held stream must return the full capacity"
    );
}

#[tokio::test]
async fn a_saturated_adapter_reports_capacity_rather_than_a_provider_fault() {
    use crate::transport::fixtures::{adapter, anthropic_request, input, serve, sse};

    let fixture = serve(200, &[], vec![sse(&[r#"{"type":"ping"}"#])]).await;
    let adapter = adapter(1);
    let _held = match adapter
        .open(input(
            "att_sat_1",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(crate::transport::adapter::ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a live stream: {other:?}"),
    };

    // Saturation is a HARNESS condition, so it must arrive as a typed error and
    // NOT as an AttemptFragment::ProviderFailure — a provider-failure class
    // would enter the retry vocabulary and let a full adapter look like a
    // provider fault the runtime should back off from.
    match adapter
        .open(input(
            "att_sat_2",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Err(crate::codex::CodexError::ActiveAttemptCapacity { capacity }) => {
            assert_eq!(capacity, 1);
        }
        other => panic!("a saturated adapter must say so in its own vocabulary: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Cancellation boundary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_mid_stream_produces_no_success_terminal() {
    use crate::transport::fixtures::{adapter, anthropic_request, input, serve, sse};

    // The contract: "cancellation leaves no live provider reader, tool task,
    // pending durable settlement, or false successful Step." This crate owns
    // the first clause and the "no false success" half of the last.
    let fixture = serve(
        200,
        &[],
        vec![sse(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}"#,
            // A terminal the fixture WOULD have sent, had the reader kept
            // reading. Cancelling before it must not produce it.
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            r#"{"type":"message_stop"}"#,
        ])],
    )
    .await;

    let adapter = adapter(2);
    let mut stream = match adapter
        .open(input(
            "att_cancel_mid",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(crate::transport::adapter::ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a live stream: {other:?}"),
    };

    let mut seen = Vec::new();
    // Read only as far as the first text delta, then cancel — which is what a
    // Session interrupt looks like from here.
    while let Some(fragment) = stream.next_fragment().await {
        let is_delta = matches!(fragment, AttemptFragment::TextDelta { .. });
        seen.push(fragment);
        if is_delta {
            break;
        }
    }
    assert!(
        seen.iter()
            .any(|fragment| matches!(fragment, AttemptFragment::TextDelta { .. })),
        "the fixture must stream a partial before cancelling, or it proves nothing"
    );
    assert!(
        !seen.iter().any(AttemptFragment::is_terminal),
        "no terminal may have been produced before the cancellation: {seen:?}"
    );

    drop(stream);
    assert_eq!(
        adapter.active_len(),
        0,
        "cancellation must leave no live reader holding capacity"
    );
}

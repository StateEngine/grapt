//! Witnesses for the three shared route decoders.
//!
//! Each decoder is driven through the real [`DecodeSink`], so these assert the
//! fragments a runtime would actually receive — not an intermediate the decoder
//! happens to build. The one thing they do not exercise is the socket; the
//! loopback wire fixtures in `crate::opener::tests` do that, and they drive the
//! same decoders through the same sink.
//!
//! Every test names the defect it would catch. A decoder test that only asserts
//! "text came out" passes against a decoder that emits text for every event it
//! sees, which is exactly the failure mode worth catching on a route whose
//! events this crate has never seen live.

use grapt_types::ToolCallId;

use crate::fragment::{AttemptFragment, ProviderFailureClass, ToolExecution};
use crate::routes::anthropic::AnthropicDecoder;
use crate::routes::compat::CompatibleDecoder;
use crate::routes::responses::ResponsesDecoder;
use crate::transport::blocks::BlockState;
use crate::transport::sink::{DecodeSink, ToolAccumulations};
use crate::usage::UsageRoute;

/// Drive a decoder over a sequence of `data:` payloads and collect what the
/// sink produced.
///
/// Mirrors exactly what `ProviderStream::decode` does per payload, including
/// the terminal latch, so a fragment sequence here is a fragment sequence a
/// runtime would see.
struct Harness {
    blocks: BlockState,
    tools: ToolAccumulations,
    pending: std::collections::VecDeque<AttemptFragment>,
    unmodeled: u32,
    terminated: bool,
    reported_usage: Option<crate::TokenUsage>,
    usage_warning: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
    usage_invalid: bool,
}

impl Harness {
    fn new() -> Self {
        Self {
            blocks: BlockState::default(),
            tools: ToolAccumulations::new(),
            pending: std::collections::VecDeque::new(),
            unmodeled: 0,
            terminated: false,
            reported_usage: None,
            usage_warning: None,
            usage_invalid: false,
        }
    }

    fn sink(&mut self) -> DecodeSink<'_> {
        DecodeSink::new(
            &mut self.blocks,
            &mut self.pending,
            &mut self.tools,
            &mut self.unmodeled,
            &mut self.terminated,
            &mut self.reported_usage,
            &mut self.usage_warning,
            &mut self.usage_invalid,
        )
    }

    fn fragments(&self) -> Vec<&AttemptFragment> {
        self.pending.iter().collect()
    }

    /// A compact rendering, so a sequence assertion reads as a transcript
    /// rather than as a wall of struct literals.
    fn shape(&self) -> Vec<String> {
        self.pending
            .iter()
            .map(|fragment| match fragment {
                AttemptFragment::AttemptOpened => "opened".to_owned(),
                AttemptFragment::TextOpened { block, .. } => format!("text.open#{}", block.value()),
                AttemptFragment::TextDelta { block, text, .. } => {
                    format!("text#{}:{text}", block.value())
                }
                AttemptFragment::TextClosed { block, .. } => {
                    format!("text.close#{}", block.value())
                }
                AttemptFragment::ReasoningOpened { block, .. } => {
                    format!("reason.open#{}", block.value())
                }
                AttemptFragment::ReasoningDelta { block, text, .. } => {
                    format!("reason#{}:{text}", block.value())
                }
                AttemptFragment::ReasoningClosed { block, .. } => {
                    format!("reason.close#{}", block.value())
                }
                AttemptFragment::ToolInputOpened { call, tool, .. } => {
                    format!("tool.open:{}:{tool}", call.as_str())
                }
                AttemptFragment::ToolInputDelta { call, text, .. } => {
                    format!("tool.delta:{}:{text}", call.as_str())
                }
                AttemptFragment::ToolInputClosed { call, .. } => {
                    format!("tool.close:{}", call.as_str())
                }
                AttemptFragment::ToolInputInvalid { call, .. } => {
                    format!("tool.invalid:{}", call.as_str())
                }
                AttemptFragment::ToolCalled {
                    call, tool, input, ..
                } => format!("tool.called:{}:{tool}:{input}", call.as_str()),
                AttemptFragment::HostedToolResult { call, .. } => {
                    format!("hosted.result:{}", call.as_str())
                }
                AttemptFragment::HostedToolFailed { call, .. } => {
                    format!("hosted.failed:{}", call.as_str())
                }
                AttemptFragment::Usage { .. } => "usage".to_owned(),
                AttemptFragment::ProviderTerminal { finish, .. } => format!("terminal:{finish:?}"),
                AttemptFragment::ProviderFailure { class, .. } => format!("failure:{class:?}"),
            })
            .collect()
    }

    fn terminal_class(&self) -> Option<&ProviderFailureClass> {
        self.pending.iter().find_map(|fragment| match fragment {
            AttemptFragment::ProviderFailure { class, .. } => Some(class),
            _ => None,
        })
    }
}

fn responses(payloads: &[&str]) -> Harness {
    responses_for_route(payloads, UsageRoute::OpenAiResponses)
}

fn responses_for_route(payloads: &[&str], route: UsageRoute) -> Harness {
    let mut harness = Harness::new();
    let mut decoder = ResponsesDecoder::for_route(route);
    for payload in payloads {
        let mut sink = harness.sink();
        decoder.decode(payload, &mut sink);
    }
    harness
}

fn anthropic(payloads: &[&str]) -> Harness {
    let mut harness = Harness::new();
    let mut decoder = AnthropicDecoder::new();
    for payload in payloads {
        let mut sink = harness.sink();
        decoder.decode(payload, &mut sink);
    }
    harness
}

/// Drives the compat decoder INCLUDING its end-of-stream hook, because that is
/// where this route's terminal is emitted — see `routes::compat`. A harness
/// that stopped at the last payload would assert a transcript no runtime ever
/// sees.
fn compat(payloads: &[&str]) -> Harness {
    let mut harness = Harness::new();
    let mut decoder = CompatibleDecoder::new();
    for payload in payloads {
        let mut sink = harness.sink();
        decoder.decode(payload, &mut sink);
    }
    let mut sink = harness.sink();
    decoder.finish(&mut sink);
    harness
}

// ===========================================================================
// OpenAI Responses
// ===========================================================================

#[test]
fn responses_normalizes_text_reasoning_usage_and_stop() {
    let harness = responses(&[
        r#"{"type":"response.reasoning_text.delta","delta":"thinking"}"#,
        r#"{"type":"response.reasoning_text.done"}"#,
        r#"{"type":"response.output_text.delta","delta":"Hel"}"#,
        r#"{"type":"response.output_text.delta","delta":"lo"}"#,
        r#"{"type":"response.output_text.done"}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":4}}}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "reason.open#0",
            "reason#0:thinking",
            "reason.close#0",
            "text.open#1",
            "text#1:Hel",
            "text#1:lo",
            "text.close#1",
            "terminal:Stop",
        ]
    );
}

#[test]
fn responses_gives_interleaved_text_and_reasoning_distinct_ordinals() {
    // Independent block ordinals: reasoning opens at 0 and text at 1;
    // a decoder deriving "the current block" from one counter would attach the
    // later reasoning delta to block 1 and close both under one ordinal.
    let harness = responses(&[
        r#"{"type":"response.reasoning_text.delta","delta":"a"}"#,
        r#"{"type":"response.output_text.delta","delta":"b"}"#,
        r#"{"type":"response.reasoning_text.delta","delta":"c"}"#,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "reason.open#0",
            "reason#0:a",
            "text.open#1",
            "text#1:b",
            "reason#0:c",
            "text.close#1",
            "reason.close#0",
            "terminal:Stop",
        ],
        "each block must keep its own ordinal for its whole life"
    );
}

// ===========================================================================
// Anthropic Messages
// ===========================================================================

#[test]
fn anthropic_normalizes_thinking_then_text_with_merged_usage() {
    let harness = anthropic(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":20}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"fixture-hmm"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "reason.open#0",
            "reason#0:hmm",
            "reason.close#0",
            "text.open#1",
            "text#1:Hi",
            "text.close#1",
            "terminal:Stop",
        ]
    );

    // The terminal must carry BOTH halves: input counts came from
    // `message_start`, output from `message_delta`. A decoder that kept only
    // the later usage would report an attempt with no input tokens.
    let usage = harness
        .fragments()
        .into_iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ProviderTerminal { usage, .. } => usage.as_ref(),
            _ => None,
        });
    let usage = match usage {
        Some(usage) => usage,
        None => panic!("the terminal must carry the merged usage"),
    };
    assert_eq!(
        usage.input_fresh().value(),
        Some(100),
        "fresh input came from message_start"
    );
    assert_eq!(
        usage.output().value(),
        Some(7),
        "output came from message_delta"
    );
    assert_eq!(usage.cache_read().value(), Some(20));
    // Absent is not zero: Anthropic reports no reasoning-token counter here.
    assert_eq!(usage.reasoning().value(), None);
}

#[test]
fn anthropic_latches_decrease_before_a_recovering_snapshot() {
    for (field, initial) in [
        ("output_tokens", 1),
        ("input_tokens", 25),
        ("cache_read_input_tokens", 2),
        ("cache_creation_input_tokens", 3),
    ] {
        let start = serde_json::json!({"type": "message_start", "message": {
            "usage": {field: initial}
        }})
        .to_string();
        let decrease = serde_json::json!({"type": "message_delta", "usage": {
            field: initial - 1
        }})
        .to_string();
        let recovery = serde_json::json!({"type": "message_delta", "usage": {
            field: initial + 1
        }})
        .to_string();
        let harness = anthropic(&[&start, &decrease, &recovery, r#"{"type":"message_stop"}"#]);
        assert_eq!(harness.terminal_class(), None, "{field}");
        assert!(harness.usage_warning.is_some(), "{field}");
        assert!(
            harness
                .fragments()
                .iter()
                .any(|fact| matches!(fact, AttemptFragment::ProviderTerminal { usage: None, .. })),
            "a later recovery must not restore {field}"
        );
    }
}

#[test]
fn anthropic_does_not_terminate_on_message_delta() {
    // `message_delta` carries the stop reason but is NOT the end of the stream.
    // A decoder that terminated there would truncate every response by one
    // frame and lose anything the provider still had to send.
    let harness = anthropic(&[
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
    ]);
    assert!(
        harness.shape().is_empty(),
        "message_delta must retain the finish, not emit a terminal: {:?}",
        harness.shape()
    );
}

#[test]
fn anthropic_never_uses_the_provider_index_as_an_ordinal() {
    // The defect this route would otherwise reintroduce. The provider opens its
    // FIRST block at index 5; the normalized ordinal must still be 0, because
    // the index is the provider's addressing scheme and the ordinal is the
    // transcript's position.
    let harness = anthropic(&[
        r#"{"type":"content_block_start","index":5,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":5,"delta":{"type":"text_delta","text":"x"}}"#,
        r#"{"type":"content_block_stop","index":5}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "text.open#0",
            "text#0:x",
            "text.close#0",
            "terminal:Unknown"
        ],
        "the ordinal is the transcript's, not the provider's index"
    );
}

#[test]
fn anthropic_decodes_a_tool_use_block() {
    let harness = anthropic(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"search"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"rust\"}"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "tool.open:toolu_1:search",
            "tool.delta:toolu_1:{\"q\":",
            "tool.delta:toolu_1:\"rust\"}",
            "tool.close:toolu_1",
            "tool.called:toolu_1:search:{\"q\":\"rust\"}",
            "terminal:ToolCalls",
        ]
    );
}

#[test]
fn anthropic_refuses_a_delta_for_a_block_it_never_opened() {
    let harness = anthropic(&[
        r#"{"type":"content_block_delta","index":3,"delta":{"type":"text_delta","text":"x"}}"#,
    ]);
    assert_eq!(
        harness.terminal_class(),
        Some(&ProviderFailureClass::InvalidProviderOutput)
    );
}

#[test]
fn pc_anthropic_refuses_duplicate_open_unmatched_stop_and_truncated_blocks() {
    for payloads in [
        vec![
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        ],
        vec![r#"{"type":"content_block_stop","index":0}"#],
        vec![
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    ] {
        let harness = anthropic(&payloads);
        assert!(harness.fragments().iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderFailure {
                class: ProviderFailureClass::InvalidProviderOutput,
                ..
            }
        )));
        assert!(!harness
            .fragments()
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::ProviderTerminal { .. })));
    }
}

#[test]
fn anthropic_overload_is_retryable_and_authentication_is_not() {
    // The classes are what the retry law reads, so the DIRECTION matters in
    // both senses: a retryable class on a terminal error would loop, and a
    // terminal class on backpressure would fail a request that would have
    // succeeded a second later.
    let overloaded = anthropic(&[r#"{"type":"error","error":{"type":"overloaded_error"}}"#]);
    let class = match overloaded.terminal_class() {
        Some(class) => class.clone(),
        None => panic!("an error frame must produce a failure"),
    };
    assert!(class.is_retryable(), "overloaded_error is backpressure");

    let auth = anthropic(&[r#"{"type":"error","error":{"type":"authentication_error"}}"#]);
    let class = match auth.terminal_class() {
        Some(class) => class.clone(),
        None => panic!("an error frame must produce a failure"),
    };
    assert!(!class.is_retryable(), "a rejected credential is terminal");
}

#[test]
fn anthropic_maps_max_tokens_to_length_not_to_an_ordinary_stop() {
    // The contract: "Provider `length`/equivalent maps to `Truncated`, never
    // ordinary idle." Reporting Stop would tell the runtime the model finished.
    let harness = anthropic(&[
        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    assert_eq!(harness.shape(), vec!["terminal:Length"]);
}

// ===========================================================================
// OpenAI-compatible chat completions
// ===========================================================================

#[test]
fn compat_normalizes_content_deltas_and_a_stop() {
    let harness = compat(&[
        r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "text.open#0",
            "text#0:Hel",
            "text#0:lo",
            "text.close#0",
            "terminal:Stop"
        ]
    );
}

#[test]
fn compat_applies_a_delta_that_shares_its_chunk_with_the_finish_reason() {
    // The defect: latching the terminal before applying the delta drops the
    // last token of every response that packs both into one chunk — which is
    // ordinary in this protocol, not an edge case.
    let harness = compat(&[r#"{"choices":[{"delta":{"content":"last"},"finish_reason":"stop"}]}"#]);
    assert_eq!(
        harness.shape(),
        vec![
            "text.open#0",
            "text#0:last",
            "text.close#0",
            "terminal:Stop"
        ],
        "the final token must survive the chunk that also ends the stream"
    );
}

#[test]
fn compat_reads_a_vendor_reasoning_channel_without_requiring_one() {
    let with_reasoning = compat(&[
        r#"{"choices":[{"delta":{"reasoning_content":"why"}}]}"#,
        r#"{"choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}]}"#,
    ]);
    assert_eq!(
        with_reasoning.shape(),
        vec![
            "reason.open#0",
            "reason#0:why",
            "text.open#1",
            "text#1:answer",
            "text.close#1",
            "reason.close#0",
            "terminal:Stop",
        ]
    );

    // A server without the extension is fully conformant and must not be
    // penalised: no reasoning fragments, no failure.
    let without = compat(&[r#"{"choices":[{"delta":{"content":"a"},"finish_reason":"stop"}]}"#]);
    assert_eq!(
        without.shape(),
        vec!["text.open#0", "text#0:a", "text.close#0", "terminal:Stop"]
    );
}

#[test]
fn compat_accumulates_a_tool_call_across_chunks_by_index() {
    // `id` and `name` arrive only on the first chunk for an index; the rest
    // carry arguments alone. A decoder that required them on every chunk would
    // drop every argument fragment after the first.
    let harness = compat(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"go","arguments":"{\"a\":"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]);
    // This assertion previously expected
    // `tool.invalid` + `tool.close` and NO `tool.called`, with a comment
    // calling that "truthful". It was not: chat-completions has no per-tool
    // "arguments done" event, so `finish_reason` IS the completion signal, and
    // this is a VALID complete tool call. The old expectation meant a Step
    // terminated looking successful while the tool the model asked for was
    // never dispatched. **The witness certified the defect**, which is why the
    // defect can survive without this split-arguments case.
    assert_eq!(
        harness.shape(),
        vec![
            "tool.open:call_1:go",
            "tool.delta:call_1:{\"a\":",
            "tool.delta:call_1:1}",
            "tool.close:call_1",
            "tool.called:call_1:go:{\"a\":1}",
            "terminal:ToolCalls",
        ]
    );
}

#[test]
fn compat_settles_every_open_tool_call_when_the_response_completes() {
    // POSITIVE, plural. One call could be settled by an accidental
    // "close the most recent" rather than by settling the open SET, and a
    // single-call witness cannot tell those apart.
    let harness = compat(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"one","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"two","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]);
    let called: Vec<String> = harness
        .fragments()
        .into_iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::ToolCalled { call, input, .. } => {
                Some(format!("{}:{input}", call.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        called,
        vec!["call_a:{}".to_owned(), "call_b:{}".to_owned()],
        "every open call must be settled, not just the last: {:?}",
        harness.shape()
    );
    assert!(
        !harness
            .shape()
            .iter()
            .any(|entry| entry.starts_with("tool.invalid")),
        "a completed response has no invalid tool input: {:?}",
        harness.shape()
    );
}

#[test]
fn compat_still_refuses_a_tool_call_the_provider_never_completed() {
    // FAILURE half of the same contract, and the reason the fix is not simply
    // "always settle". A stream that ends with NO `finish_reason` is genuinely
    // truncated: the provider never said the call was complete, so calling the
    // tool with whatever arrived would dispatch arguments that may be a partial
    // JSON prefix. This must still refuse.
    let harness = compat(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"go","arguments":"{\"a\":"}}]}}]}"#,
    ]);
    assert!(
        !harness
            .shape()
            .iter()
            .any(|entry| entry.starts_with("tool.called")),
        "an uncompleted tool call must never be dispatched: {:?}",
        harness.shape()
    );
    // And no terminal at all: the shared stream turns EOF-without-terminal into
    // the typed transport failure, which is the truthful outcome.
    assert!(
        !harness
            .shape()
            .iter()
            .any(|entry| entry.starts_with("terminal")),
        "a truncated stream must not manufacture a terminal: {:?}",
        harness.shape()
    );
}

#[test]
fn compat_reads_a_trailing_usage_only_chunk() {
    // `stream_options.include_usage` sends a final chunk with an EMPTY choices
    // array. A decoder that walked choices first would count it as unmodeled
    // and lose the only usage the protocol reports for a streamed response.
    let harness = compat(&[
        r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}}"#,
    ]);
    // The usage must reach the TERMINAL, not merely be "seen". The terminal is
    // what a runtime reconciles against, and this protocol delivers usage
    // AFTER the finish reason — so a decoder that terminated on the finish
    // would emit a terminal with no usage and then discard the chunk that had
    // it, which is precisely what this assertion caught.
    let usage = harness
        .fragments()
        .into_iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ProviderTerminal { usage, .. } => usage.as_ref(),
            _ => None,
        });
    let usage = match usage {
        Some(usage) => usage,
        None => panic!(
            "the terminal must carry the trailing usage: {:?}",
            harness.shape()
        ),
    };
    assert_eq!(usage.input_total().value(), Some(9));
    assert_eq!(usage.output().value(), Some(2));
    assert_eq!(usage.total().value(), Some(11));
}

#[test]
fn compat_treats_an_in_band_error_object_as_a_failure_not_an_unknown_frame() {
    // Compatible servers deliver errors in-band routinely. Counting one as
    // unmodeled would let the stream reach EOF with no terminal, which the
    // transport reports as a RETRYABLE transport failure — so a hard
    // authentication error would be retried four more times.
    let harness = compat(&[r#"{"error":{"type":"invalid_api_key","message":"bad key"}}"#]);
    let class = match harness.terminal_class() {
        Some(class) => class.clone(),
        None => panic!("an in-band error must produce a failure terminal"),
    };
    assert_eq!(class, ProviderFailureClass::Authentication);
    assert!(!class.is_retryable());
}

#[test]
fn compat_does_not_leak_the_vendor_error_message_into_the_diagnostic() {
    // The master requires a diagnostic to reveal "neither payload nor
    // secret-dependent length". A vendor error message is provider-controlled
    // text on an error path and can echo request content.
    let harness = compat(&[
        r#"{"error":{"type":"invalid_request_error","message":"key sk-secret-xyz is bad"}}"#,
    ]);
    let message = harness
        .fragments()
        .into_iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ProviderFailure { message, .. } => Some(message.clone()),
            _ => None,
        });
    let message = match message {
        Some(message) => message,
        None => panic!("an in-band error must produce a failure terminal"),
    };
    assert!(
        !message.contains("sk-secret-xyz"),
        "the vendor message must not travel into the diagnostic: {message}"
    );
}

#[test]
fn compat_maps_length_to_length_and_leaves_an_unmodeled_reason_unknown() {
    let length = compat(&[r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#]);
    assert_eq!(length.shape(), vec!["terminal:Length"]);

    let odd = compat(&[r#"{"choices":[{"delta":{},"finish_reason":"something_new"}]}"#]);
    assert_eq!(
        odd.shape(),
        vec!["terminal:Unknown"],
        "an unmapped finish is Unknown, never silently Stop"
    );
}

// ===========================================================================
// Shared sink laws, witnessed on the routes that would break them
// ===========================================================================

#[test]
fn a_second_terminal_cannot_be_emitted_by_any_route() {
    // The latch. Each route is given two terminals in a row; the second must
    // produce nothing, because "no fragment follows either terminal fragment".
    let responses = responses(&[
        r#"{"type":"response.completed","response":{}}"#,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    assert_eq!(responses.shape(), vec!["terminal:Stop"]);

    let anthropic = anthropic(&[r#"{"type":"message_stop"}"#, r#"{"type":"message_stop"}"#]);
    assert_eq!(anthropic.shape(), vec!["terminal:Unknown"]);

    let compat = compat(&[
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ]);
    assert_eq!(compat.shape(), vec!["terminal:Stop"]);
}

#[test]
fn output_after_a_terminal_is_dropped_rather_than_appended() {
    // A late text delta must not append to a terminated attempt: the validator
    // would refuse it, and appending would corrupt a projection that is already
    // settled.
    let harness = responses(&[
        r#"{"type":"response.completed","response":{}}"#,
        r#"{"type":"response.output_text.delta","delta":"late"}"#,
    ]);
    assert_eq!(harness.shape(), vec!["terminal:Stop"]);
}

#[test]
fn a_tool_input_exceeding_the_bound_is_refused_before_it_grows() {
    // The bound is on ALLOCATION, not a report made afterwards. The witness is
    // that the emitted invalid fragment carries no accumulated payload and the
    // call is never made with input the adapter admits it lost.
    let big = "x".repeat(crate::transport::sink::MAX_TOOL_INPUT_BYTES + 1);
    let payload = format!(
        r#"{{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{big}"}}"#
    );
    let harness = responses(&[
        r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_1","name":"t"}}"#,
        &payload,
        r#"{"type":"response.function_call_arguments.done","item_id":"item_1"}"#,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    assert_eq!(
        harness.shape(),
        vec![
            "tool.open:call_1:t",
            "tool.invalid:call_1",
            "tool.close:call_1",
            "terminal:Stop",
        ],
        "an over-bound input must not become a ToolCalled with lost arguments"
    );
}

#[test]
fn more_concurrent_tool_inputs_than_the_budget_is_refused() {
    let mut payloads = Vec::new();
    for index in 0..=crate::transport::sink::MAX_OPEN_TOOL_INPUTS {
        payloads.push(format!(
            r#"{{"type":"response.output_item.added","item":{{"type":"function_call","id":"item_{index}","call_id":"call_{index}","name":"t"}}}}"#
        ));
    }
    let borrowed: Vec<&str> = payloads.iter().map(String::as_str).collect();
    let harness = responses(&borrowed);
    assert_eq!(
        harness.terminal_class(),
        Some(&ProviderFailureClass::InvalidProviderOutput),
        "the accumulation map must be bounded like every other provider-controlled structure"
    );
}

#[test]
fn a_tool_call_carries_its_execution_owner_as_a_closed_enum() {
    // `execution` is a closed enum, never an optional boolean. Every call these
    // routes decode is Local: none of the three families' function-calling
    // surfaces are provider-hosted, and claiming ProviderHosted would route the
    // call to a settlement path that never fires.
    let harness = responses(&[
        r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"i","call_id":"c","name":"t"}}"#,
        r#"{"type":"response.function_call_arguments.done","item_id":"i"}"#,
    ]);
    let execution = harness
        .fragments()
        .into_iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ToolCalled { execution, .. } => Some(*execution),
            _ => None,
        });
    assert_eq!(execution, Some(ToolExecution::Local));
}

#[test]
fn the_durable_call_id_is_the_providers_call_id_not_the_accumulation_key() {
    // The contract's ProviderToolInputKey / ProviderToolHandle split. Persisting
    // the attempt-local key as the durable handle is one of the master's named
    // mutation failures.
    let harness = responses(&[
        r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_local","call_id":"call_durable","name":"t"}}"#,
        r#"{"type":"response.function_call_arguments.done","item_id":"item_local"}"#,
    ]);
    let call = harness
        .fragments()
        .into_iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ToolCalled { call, .. } => Some(call.clone()),
            _ => None,
        });
    assert_eq!(call, ToolCallId::new("call_durable"));
}

#[test]
fn scc_live_route_overflow_classification_preserves_visible_output_and_redacts_error() {
    let responses_error = r#"{"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"SCC_PRIVATE"}}}"#;
    let compat_error = r#"{"error":{"type":"invalid_request_error","code":"context_length_exceeded","message":"SCC_PRIVATE"}}"#;
    let cases = [
        responses_for_route(&[responses_error], UsageRoute::ChatGptCodex),
        compat(&[compat_error]),
        responses_for_route(
            &[
                r#"{"type":"response.output_text.delta","delta":"visible"}"#,
                responses_error,
            ],
            UsageRoute::ChatGptCodex,
        ),
        compat(&[
            r#"{"choices":[{"index":0,"delta":{"content":"visible"},"finish_reason":null}]}"#,
            compat_error,
        ]),
    ];
    for (index, harness) in cases.iter().enumerate() {
        let fragments = harness.fragments();
        assert_eq!(
            fragments
                .iter()
                .filter(|fragment| matches!(
                    fragment,
                    AttemptFragment::ProviderFailure {
                        classification: Some(crate::FailureClassification::ContextOverflow),
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            fragments
                .iter()
                .filter(|fragment| matches!(fragment, AttemptFragment::TextDelta { .. }))
                .count(),
            usize::from(index >= 2)
        );
        // Responses now preserves backend text; the public transport masks
        // credentials before exposing it (covered by its HTTP fixture).
        assert_eq!(
            format!("{fragments:?}").contains("SCC_PRIVATE"),
            index % 2 == 0
        );
    }
}

#[path = "pc_tests.rs"]
mod pc_tests;

#[path = "tool_call_tests.rs"]
mod tool_call_tests;

#[path = "native_search_tests.rs"]
mod native_search_tests;

#[path = "anthropic_search_tests.rs"]
mod anthropic_search_tests;

#[path = "citation_tests.rs"]
mod citation_tests;
#[path = "text_tool_tests.rs"]
mod text_tool_tests;

#[test]
fn anthropic_overflow_frames_preserve_visible_output_and_invalid_request_failures() {
    for (kind, message, expected) in [
        ("invalid_request_error", "prompt is too long", true),
        (
            "invalid_request_error",
            "prompt is too long: 213462 tokens > 200000 maximum",
            true,
        ),
        (
            "invalid_request_error",
            "max_tokens must be positive SCC_PRIVATE",
            false,
        ),
        (
            "invalid_request_error",
            "the quoted prompt is too long SCC_PRIVATE",
            false,
        ),
        ("api_error", "prompt is too long", false),
    ] {
        let error =
            serde_json::json!({"type":"error","error":{"type":kind,"message":message}}).to_string();
        for visible in [false, true] {
            let mut payloads = Vec::new();
            if visible {
                payloads.push(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#);
                payloads.push(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"visible"}}"#);
            }
            payloads.push(&error);
            let harness = anthropic(&payloads);
            let fragments = harness.fragments();
            let classification = fragments.iter().find_map(|fragment| match fragment {
                AttemptFragment::ProviderFailure { classification, .. } => *classification,
                _ => None,
            });
            assert_eq!(
                classification,
                expected.then_some(crate::FailureClassification::ContextOverflow)
            );
            assert_eq!(
                fragments
                    .iter()
                    .filter(|fragment| matches!(fragment, AttemptFragment::TextDelta { .. }))
                    .count(),
                usize::from(visible)
            );
            assert!(!format!("{fragments:?}").contains(message));
            if kind == "invalid_request_error" {
                assert_eq!(
                    harness.terminal_class(),
                    Some(&ProviderFailureClass::InvalidRequest)
                );
            }
        }
    }
}

#[path = "failed_usage_tests.rs"]
mod failed_usage_tests;

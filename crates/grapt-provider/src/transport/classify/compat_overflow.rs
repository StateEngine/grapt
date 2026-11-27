//! Overflow wording from local and third-party OpenAI-compatible servers.
//!
//! Each body is a shape the server documents or emits: a machine code, a
//! message that names the context window, or (Ollama) the message as the whole
//! `error` string. A server that answers a too-long request with prose has to
//! reach the same rebuild path as OpenAI's own code, or a local model fails the
//! session hard instead of compacting.

use super::super::fixtures::*;
use crate::fragment::{AttemptFragment, ProviderFailureClass};
use crate::ProtocolFamily;

/// One server's refusal body.
struct OverflowBody {
    server: &'static str,
    body: serde_json::Value,
    /// A vendor message the body carries, which the failure must state.
    reason: Option<&'static str>,
    expected: bool,
}

/// Bodies that state an input overflow, in each server's own vocabulary.
fn overflow_bodies() -> Vec<OverflowBody> {
    vec![
        OverflowBody {
            // OpenAI's own code, and every server that copies it.
            server: "openai-code",
            body: serde_json::json!({
                "error": {
                    "message": "This model's maximum context length is 8192 tokens.",
                    "type": "invalid_request_error",
                    "code": "context_length_exceeded"
                }
            }),
            reason: None,
            expected: true,
        },
        OverflowBody {
            // llama.cpp's typed error, which is not a status-derived class.
            server: "llama-cpp",
            body: serde_json::json!({
                "error": {
                    "code": 400,
                    "message": "the request exceeds the available context size, try increasing it",
                    "type": "exceed_context_size_error"
                }
            }),
            reason: None,
            expected: true,
        },
        OverflowBody {
            // vLLM's input overflow: it names the prompt as the overflowing
            // part, so the shared window sentence classifies here.
            server: "vllm",
            body: serde_json::json!({
                "object": "error",
                "message": "This model's maximum context length is 32768 tokens. However, you requested 5000 tokens and your prompt contains 33100 tokens, leaving 27768 tokens for output. Please reduce the length of the messages or completion.",
                "type": "BadRequestError",
                "param": null,
                "code": 400
            }),
            reason: None,
            expected: true,
        },
        OverflowBody {
            // LM Studio and llama.cpp chat-completions wording.
            server: "lm-studio",
            body: serde_json::json!({
                "error": {
                    "message": "Trying to keep the first 4096 tokens when context overflows. The input length exceeds the context length.",
                    "type": "invalid_request_error",
                    "code": 400
                }
            }),
            reason: None,
            expected: true,
        },
        OverflowBody {
            // Ollama puts the message itself at `error`, not an object.
            server: "ollama",
            body: serde_json::json!({
                "error": "the input length exceeds the context length"
            }),
            reason: None,
            expected: true,
        },
        OverflowBody {
            // Some gateways report the window as a plain sentence.
            server: "gateway",
            body: serde_json::json!({
                "error": {
                    "message": "Request too large: context length exceeded (33000 > 32768)",
                    "type": "invalid_request_error"
                }
            }),
            reason: None,
            expected: true,
        },
    ]
}

/// Refusals that share wording with an overflow but are a different fault.
fn near_miss_bodies() -> Vec<OverflowBody> {
    vec![
        OverflowBody {
            // vLLM's output-budget refusal shares the window sentence with its
            // input-overflow refusal but is not an overflow of the prompt.
            // Compacting here would spend a summary and a reload, then fail.
            server: "vllm-output-budget",
            body: serde_json::json!({
                "object": "error",
                "message": "This model's maximum context length is 32768 tokens. However, you requested 40000 output tokens and your prompt contains 5000 tokens, leaving 27768 tokens for output. Please reduce the length of the messages or completion.",
                "type": "BadRequestError",
                "param": null,
                "code": 400
            }),
            reason: None,
            expected: false,
        },
        OverflowBody {
            // An output-parameter refusal is a different fault. Compacting
            // would rebuild a healthy session, so it stays unclassified.
            server: "max-tokens",
            body: serde_json::json!({
                "error": {
                    "message": "max_tokens is too large: 999999. This model supports at most 8192 completion tokens.",
                    "type": "invalid_request_error",
                    "code": "invalid_value"
                }
            }),
            reason: None,
            expected: false,
        },
        OverflowBody {
            // Truncation policy, not an overflow of this request.
            server: "truncation",
            body: serde_json::json!({
                "error": {
                    "message": "truncation is not supported for this model",
                    "type": "invalid_request_error",
                    "code": "unsupported_parameter"
                }
            }),
            reason: None,
            expected: false,
        },
        OverflowBody {
            // Unknown provider text never supplies proof.
            server: "unrecognized",
            body: serde_json::json!({
                "error": {
                    "message": "COMPAT_PRIVATE vendor diagnostic",
                    "type": "invalid_request_error",
                    "code": "unlisted"
                }
            }),
            reason: Some("COMPAT_PRIVATE vendor diagnostic"),
            expected: false,
        },
    ]
}

/// Every fixture body: the overflows first, then the near-misses.
fn bodies() -> Vec<OverflowBody> {
    overflow_bodies()
        .into_iter()
        .chain(near_miss_bodies())
        .collect()
}

/// Every server's body over the real adapter: the ones that state an overflow
/// reach [`FailureClassification::ContextOverflow`], the near-misses do not,
/// and the failure states the vendor's own message.
#[tokio::test]
async fn scc_compat_servers_reach_overflow_classification_from_their_own_wording() {
    for case in bodies() {
        let body = case.body.to_string();
        let fixture = serve(400, &[], vec![body.into_bytes()]).await;
        let adapter = adapter(2);
        let open = adapter
            .open(input(
                "att_compat_overflow",
                &fixture.base,
                ProtocolFamily::OpenAiCompatible,
                compat_request(),
            ))
            .await
            .unwrap_or_else(|error| panic!("fixture open failed: {error}"));
        let fragments = drain(open).await;
        let failure = fragments.iter().find_map(|fragment| match fragment {
            AttemptFragment::ProviderFailure {
                class,
                classification,
                ..
            } => Some((class, *classification)),
            _ => None,
        });
        assert_eq!(
            failure.as_ref().map(|(_, classification)| *classification),
            Some(
                case.expected
                    .then_some(crate::FailureClassification::ContextOverflow)
            ),
            "{}",
            case.server
        );
        // The Step rebuilds on the classification alone
        // (`step/classification.rs`), and reads it beside the status class, so
        // an overflowing compat body keeps the class its status derived.
        assert_eq!(
            failure.map(|(class, _)| class),
            Some(&ProviderFailureClass::InvalidRequest),
            "{}",
            case.server
        );
        if let Some(reason) = case.reason {
            assert!(
                format!("{fragments:?}").contains(reason),
                "{} hid the vendor's reason",
                case.server
            );
        }
        assert_eq!(adapter.active_len(), 0, "{}", case.server);
    }
}

/// A status that carries no overflow vocabulary, and an in-band vendor error
/// on a stream, both keep the class they always had.
#[tokio::test]
async fn scc_compat_overflow_requires_a_bounded_overflow_status() {
    for (status, expected) in [(401, false), (429, false), (500, false)] {
        let body = serde_json::json!({
            "error": {
                "message": "This model's maximum context length is 8192 tokens.",
                "type": "invalid_request_error"
            }
        })
        .to_string();
        let fixture = serve(status, &[], vec![body.into_bytes()]).await;
        let adapter = adapter(2);
        let open = adapter
            .open(input(
                "att_compat_status",
                &fixture.base,
                ProtocolFamily::OpenAiCompatible,
                compat_request(),
            ))
            .await
            .unwrap_or_else(|error| panic!("fixture open failed: {error}"));
        let fragments = drain(open).await;
        let classification = fragments.iter().find_map(|fragment| match fragment {
            AttemptFragment::ProviderFailure { classification, .. } => *classification,
            _ => None,
        });
        assert_eq!(
            classification,
            expected.then_some(crate::FailureClassification::ContextOverflow),
            "{status}"
        );
        assert_eq!(adapter.active_len(), 0);
    }
}

/// Near-misses that share wording with an overflow but are a different fault.
/// Compacting on any of them would spend a summary and a reload, then fail.
#[test]
fn scc_a_prompt_that_fits_is_not_compacted_for_an_output_budget() {
    let cases = [
        serde_json::json!({
            "object": "error",
            "message": "This model's maximum context length is 32768 tokens. However, you requested 40000 output tokens and your prompt contains 5000 tokens, leaving 27768 tokens for output. Please reduce the length of the messages or completion.",
            "type": "BadRequestError",
            "code": 400
        }),
        serde_json::json!({
            "error": {
                "message": "max_tokens is too large: 999999",
                "type": "invalid_request_error"
            }
        }),
    ];
    for body in cases {
        let error = body.get("error").cloned().unwrap_or_else(|| body.clone());
        assert_eq!(
            crate::transport::classify::compat_context_classification(&error),
            None,
            "{body}"
        );
    }
}

/// The status path and the in-band path agree for every body: each hands the
/// classifier the member the HTTP path would (the `error` object, or the whole
/// body when the server puts the error at the top level).
#[test]
fn scc_compat_status_and_stream_paths_share_one_vocabulary() {
    for case in bodies() {
        let error = case
            .body
            .get("error")
            .cloned()
            .unwrap_or_else(|| case.body.clone());
        assert_eq!(
            crate::transport::classify::compat_context_classification(&error),
            case.expected
                .then_some(crate::FailureClassification::ContextOverflow),
            "{}",
            case.server
        );
    }
}

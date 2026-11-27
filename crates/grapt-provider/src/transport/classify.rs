//! Provider-neutral status, retry-after and failure-fragment classification.
//!
//! Shared with the ChatGPT/Codex route because the mapping is
//! not Codex's: HTTP 401 is authentication and 429 is a rate limit on the
//! OpenAI Responses route, on Anthropic Messages, and on every
//! OpenAI-compatible endpoint this crate now serves. Three private copies of
//! one table would be three places for the classes to drift apart, and the
//! retry law that reads them (`ProviderFailureClass::is_retryable`) is single.
//!
//! Route-SPECIFIC classification stays with its adapter: Anthropic's
//! `overloaded_error` and the compat routes' vendor error bodies are decoded by
//! the adapter that understands them, and reach the runtime through these same
//! classes.

use std::time::Duration;

use crate::fragment::{AdapterState, AttemptFragment, FailureClassification, ProviderFailureClass};

/// OpenAI-family overflow requires the exact machine code.
/// Diagnostic prose, malformed JSON and unknown codes never supply that proof.
pub(crate) fn context_classification(value: &serde_json::Value) -> Option<FailureClassification> {
    (value.get("code").and_then(serde_json::Value::as_str) == Some("context_length_exceeded"))
        .then_some(FailureClassification::ContextOverflow)
}

/// Machine words local and third-party OpenAI-compatible servers use for a
/// request that does not fit the model's context window.
///
/// Each is a word the server documents or emits verbatim: OpenAI's code,
/// llama.cpp's typed `exceed_context_size_error`, and the code some gateways
/// spell after it. A server that only sends prose is matched by
/// [`COMPAT_OVERFLOW_PHRASES`] instead. Matching is exact and case-insensitive;
/// a vendor's unrecognized text stays unclassified rather than being guessed
/// at. A false overflow would compact and rebuild a healthy session, so the set
/// stays closed and short.
const COMPAT_OVERFLOW_CODES: &[&str] = &["context_length_exceeded", "exceed_context_size_error"];

/// Prose that names the request's own input overflowing the window. Every
/// phrase here is produced only by an input overflow, so an output-budget
/// refusal cannot match: `max_tokens is too large`, `unsupported_parameter`
/// and a bare `too many tokens` do not either.
///
/// Anthropic's own wording is deliberately absent: it belongs to that route's
/// typed classifier, and matching it here would blur which route said what.
const COMPAT_OVERFLOW_PHRASES: &[&str] = &[
    "exceeds the available context size",
    "exceeds the context length",
    "exceeded the context length",
    "input length exceeds the context length",
    "context length exceeded",
];

/// The statement of the window alone: vLLM opens both refusals with it, so on
/// its own it proves nothing. It classifies only beside a prompt figure and away
/// from an output budget — see [`compat_context_classification`].
const COMPAT_WINDOW_PHRASE: &str = "maximum context length is";

/// Words that mark a refusal as the output budget rather than the prompt:
/// vLLM asks the caller to lower these when the request is only too long
/// because of what it asked the model to write.
const COMPAT_OUTPUT_WORDS: &[&str] = &[
    "output tokens",
    "completion tokens",
    "max_tokens",
    "max_completion_tokens",
];

/// The prompt is named as the part that overflowed, as vLLM spells it
/// (`30000 in the messages`).
const COMPAT_PROMPT_PHRASES: &[&str] = &["in the messages", "in the prompt", "prompt contains"];

/// The longest vendor message inspected for overflow prose. A longer one is
/// diagnostic text, not a classification input.
const COMPAT_MESSAGE_BYTES: usize = 2 * 1024;

/// Overflow on an OpenAI-compatible route, where servers predate or diverge
/// from OpenAI's exact code.
///
/// The body is the error object itself, or the string some servers put at
/// `error` for the whole failure (Ollama's shape). Only the bounded sets above
/// classify; everything else stays unclassified and the status decides the
/// class, exactly as before.
///
/// The window statement on its own is not proof. vLLM opens its output-budget
/// refusal with the same sentence, so that phrase counts only when the message
/// names the prompt as the overflowing part and names no output budget.
/// Compacting on the output-budget refusal would spend a summary and a reload
/// and then fail anyway.
pub(crate) fn compat_context_classification(
    value: &serde_json::Value,
) -> Option<FailureClassification> {
    let text = match value {
        serde_json::Value::String(text) => text.as_str(),
        serde_json::Value::Object(_) => {
            let named = [value.get("code"), value.get("type")]
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .any(|word| {
                    COMPAT_OVERFLOW_CODES
                        .iter()
                        .any(|code| code.eq_ignore_ascii_case(word))
                });
            if named {
                return Some(FailureClassification::ContextOverflow);
            }
            value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        }
        _ => return None,
    };
    if text.len() > COMPAT_MESSAGE_BYTES {
        return None;
    }
    let text = text.to_ascii_lowercase();
    let names = |set: &[&str]| set.iter().any(|phrase| text.contains(phrase));
    let input_overflow = names(COMPAT_OVERFLOW_PHRASES)
        || (text.contains(COMPAT_WINDOW_PHRASE)
            && names(COMPAT_PROMPT_PHRASES)
            && !names(COMPAT_OUTPUT_WORDS));
    input_overflow.then_some(FailureClassification::ContextOverflow)
}

fn quota_code(code: &str) -> bool {
    matches!(
        code,
        "insufficient_quota"
            | "billing_error"
            | "usage_limit_reached"
            | "credit_balance_exhausted"
            | "organization_spend_limit_exceeded"
            | "project_spend_limit_exceeded"
            | "organization_usage_limit_exceeded"
            | "subscription_sharing_usage_limit_exceeded"
    )
}

pub(crate) fn quota_classification(value: &serde_json::Value) -> Option<FailureClassification> {
    let quota = [value.get("code"), value.get("type")]
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .any(quota_code);
    quota.then(|| FailureClassification::UsageLimit {
        reset: value
            .get("resets_at")
            .and_then(serde_json::Value::as_u64)
            .and_then(grapt_types::provider_limit::ProviderLimitReset::from_unix_seconds),
    })
}

/// Anthropic uses a typed invalid request with a stable overflow message,
/// optionally followed by token counts. Other invalid requests stay terminal.
pub(crate) fn anthropic_context_classification(
    value: &serde_json::Value,
) -> Option<FailureClassification> {
    let invalid_request =
        value.get("type").and_then(serde_json::Value::as_str) == Some("invalid_request_error");
    let overflow_message = value
        .get("message")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|message| {
            message == "prompt is too long" || message.starts_with("prompt is too long: ")
        });
    (invalid_request && overflow_message).then_some(FailureClassification::ContextOverflow)
}

/// Which route's overflow vocabulary applies to an error body. Modelled as a
/// type, not a boolean: a third route must decide here rather than silently
/// inheriting another's classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverflowRoute {
    /// Anthropic Messages and its Vertex twin: a typed invalid-request message.
    Anthropic,
    /// OpenAI Responses and ChatGPT/Codex: the exact OpenAI machine code.
    Responses,
    /// OpenAI-compatible chat completions: the wider server set, with prose.
    Compatible,
    /// Every other family; only the shared machine code applies.
    Other,
}

/// Bounded overflow classification and allowlisted vendor machine words.
#[derive(Debug, Default)]
pub(crate) struct ErrorDetails {
    pub(crate) classification: Option<FailureClassification>,
    pub(crate) error_type: Option<crate::VendorCode>,
    pub(crate) error_code: Option<crate::VendorCode>,
    pub(crate) text: Option<crate::MaskedErrorText>,
}

pub(crate) const MAX_ERROR_BYTES: usize = 16 * 1024;

pub(crate) async fn error_details(
    response: &mut reqwest::Response,
    route: OverflowRoute,
    capture: Option<&crate::CredentialLease>,
) -> ErrorDetails {
    let classify = response.status().is_client_error();
    let overflow_status = matches!(response.status().as_u16(), 400 | 413 | 422);
    let mut oversized = response
        .content_length()
        .is_some_and(|length| length > MAX_ERROR_BYTES as u64);
    // The failure row shows an excerpt of the vendor's message for any failed
    // status, so a captured body is read whether or not it can classify. Without
    // a capture the body is read only when classification needs it.
    if capture.is_none() && (!classify || oversized) {
        return ErrorDetails::default();
    }
    let mut bytes = Vec::new();
    let mut complete = false;
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let keep = chunk.len().min(MAX_ERROR_BYTES.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..keep]);
                if keep < chunk.len() || oversized {
                    oversized = true;
                    break;
                }
            }
            Ok(None) => {
                complete = true;
                break;
            }
            Err(_) => break,
        }
    }
    // A successful empty EOF is evidence. A failed read before any bytes is
    // unavailable and must not erase the preceding local diagnostic.
    let text = capture
        .filter(|_| complete || !bytes.is_empty())
        .map(|lease| lease.error_preview(&bytes));
    let parsed = if classify && complete && !oversized {
        serde_json::from_slice::<serde_json::Value>(&bytes).ok()
    } else {
        None
    };
    // Most routes and servers nest the failure under `error`. Some
    // OpenAI-compatible servers put the error object itself at the top level
    // (vLLM answers `{"object":"error","message":...}`), so on that route a
    // body without an `error` member is the error object. Scoped to Compatible:
    // another route's unrecognized body must not gain a classification here.
    let error = parsed.map(|mut body| match body.get_mut("error") {
        Some(error) => serde_json::Value::take(error),
        None if route == OverflowRoute::Compatible => body,
        None => serde_json::Value::Null,
    });
    match error.filter(|error| !error.is_null()) {
        Some(error) => ErrorDetails {
            classification: if let Some(quota) = quota_classification(&error) {
                Some(quota)
            } else if overflow_status {
                let shared = context_classification(&error);
                match route {
                    OverflowRoute::Anthropic => {
                        shared.or_else(|| anthropic_context_classification(&error))
                    }
                    OverflowRoute::Compatible => {
                        shared.or_else(|| compat_context_classification(&error))
                    }
                    OverflowRoute::Responses | OverflowRoute::Other => shared,
                }
            } else {
                None
            },
            error_type: error.get("type").and_then(crate::VendorCode::from_json),
            error_code: error.get("code").and_then(crate::VendorCode::from_json),
            text,
        },
        None => ErrorDetails {
            text,
            ..ErrorDetails::default()
        },
    }
}

// ---------------------------------------------------------------------------
// Status and transport classification
// ---------------------------------------------------------------------------

/// Classify a non-success status.
///
/// The response body is **never** read into the message. An error body on this
/// route can carry account-identifying text, and the master requires a
/// diagnostic to reveal "neither payload nor secret-dependent length", so the
/// message carries the status code and nothing that varies with the body.
pub(crate) fn classify_status(status: u16, retry_after: Option<Duration>) -> ProviderFailureClass {
    match status {
        401 | 403 | 498 => ProviderFailureClass::Authentication,
        402 => ProviderFailureClass::QuotaExceeded,
        400 | 422 => ProviderFailureClass::InvalidRequest,
        404 => ProviderFailureClass::NoRoute,
        429 => ProviderFailureClass::RateLimit { retry_after },
        500..=599 => ProviderFailureClass::ProviderInternal { retry_after },
        _ => ProviderFailureClass::InvalidProviderOutput,
    }
}

/// Existing allowlisted machine codes refine a client-error status; diagnostic
/// prose, unknown or incomplete bodies never change retry eligibility.
pub(crate) fn classify_error(
    status: u16,
    retry_after: Option<Duration>,
    details: &ErrorDetails,
) -> ProviderFailureClass {
    if (400..500).contains(&status) {
        if matches!(
            details.classification,
            Some(FailureClassification::UsageLimit { .. })
        ) {
            return ProviderFailureClass::QuotaExceeded;
        }
        let codes = [details.error_code.as_ref(), details.error_type.as_ref()];
        if codes.iter().flatten().any(|code| quota_code(code.as_str())) {
            return ProviderFailureClass::QuotaExceeded;
        }
        if codes
            .iter()
            .flatten()
            .any(|code| matches!(code.as_str(), "invalid_api_key" | "authentication_error"))
        {
            return ProviderFailureClass::Authentication;
        }
    }
    classify_status(status, retry_after)
}

/// Parse `Retry-After` as delta-seconds only.
///
/// The HTTP-date form is deliberately not parsed: it needs a date library this
/// workspace does not have. An
/// unparseable value becomes `None`, which is truthful — the adapter did not
/// learn a delay — rather than a fabricated one.
///
/// **No cap is applied here, and that is a disclosed gap, not an oversight.**
/// A hostile or mistaken `Retry-After: 999999999` would be handed to
/// `StepPolicy::delay_for`, which takes the larger of it and the backoff. The
/// bound belongs to the policy that decides how long to sleep, not to the
/// adapter that merely reports what the provider said.
pub(crate) fn parse_retry_after(raw: Option<&str>) -> Option<Duration> {
    let value = raw?.trim();
    match value.parse::<u64>() {
        Ok(seconds) => Some(Duration::from_secs(seconds)),
        Err(_) => None,
    }
}

pub(crate) fn failure(class: ProviderFailureClass, message: impl Into<String>) -> AttemptFragment {
    AttemptFragment::ProviderFailure {
        class,
        classification: None,
        diagnostic: None,
        message: message.into(),
        state: AdapterState::none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cr_recovery_machine_codes_distinguish_credit_from_rate_limit() {
        let mut details = ErrorDetails {
            error_code: crate::VendorCode::from_json(&serde_json::json!("insufficient_quota")),
            ..ErrorDetails::default()
        };
        assert_eq!(
            classify_error(429, None, &details),
            ProviderFailureClass::QuotaExceeded
        );
        details.error_code =
            crate::VendorCode::from_json(&serde_json::json!("rate_limit_exceeded"));
        assert!(matches!(
            classify_error(429, None, &details),
            ProviderFailureClass::RateLimit { .. }
        ));
        details.error_code = crate::VendorCode::from_json(&serde_json::json!("invalid_api_key"));
        assert_eq!(
            classify_error(429, None, &details),
            ProviderFailureClass::Authentication
        );
        details.error_code =
            crate::VendorCode::from_json(&serde_json::json!("unknown private provider text"));
        assert!(matches!(
            classify_error(429, None, &details),
            ProviderFailureClass::RateLimit { .. }
        ));
        details.error_type = crate::VendorCode::from_json(&serde_json::json!("billing_error"));
        assert_eq!(
            classify_error(403, None, &details),
            ProviderFailureClass::QuotaExceeded
        );
        assert!(matches!(
            classify_error(503, None, &details),
            ProviderFailureClass::ProviderInternal { .. }
        ));
    }

    #[test]
    fn scc_overflow_requires_exact_machine_code_not_diagnostic_text() {
        assert_eq!(
            context_classification(
                &serde_json::json!({"code":"context_length_exceeded","message":"private diagnostic"})
            ),
            Some(FailureClassification::ContextOverflow)
        );
        for value in [
            serde_json::json!({"message":"context_length_exceeded"}),
            serde_json::json!({"code":null}),
            serde_json::json!({"code":"invalid_request_error"}),
            serde_json::json!({"code":42}),
        ] {
            assert_eq!(context_classification(&value), None);
        }
    }
}

/// The two overflow vocabularies' wire fixtures live beside this classifier.
#[cfg(test)]
#[path = "classify/compat_overflow.rs"]
mod compat_overflow;

#[cfg(test)]
#[path = "classify/anthropic_overflow.rs"]
mod anthropic_overflow;

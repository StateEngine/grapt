//! Responses terminal status takes precedence over optional accounting.
use crate::fragment::ProviderFailureClass;
use crate::transport::sink::DecodeSink;

pub(super) fn fail(kind: &str, response: &serde_json::Value, sink: &mut DecodeSink<'_>) {
    let error = &response["error"];
    let reason = response["incomplete_details"]["reason"].as_str();
    let code = error["code"]
        .as_str()
        .or_else(|| error["type"].as_str())
        .or(reason);
    let classification = crate::transport::classify::quota_classification(error)
        .or_else(|| crate::transport::classify::context_classification(error))
        .or_else(|| {
            (reason == Some("context_length_exceeded"))
                .then_some(crate::FailureClassification::ContextOverflow)
        });
    let class = if matches!(
        classification,
        Some(crate::FailureClassification::UsageLimit { .. })
    ) {
        ProviderFailureClass::QuotaExceeded
    } else if classification.is_some() {
        ProviderFailureClass::InvalidRequest
    } else {
        match code {
            Some("rate_limit_exceeded" | "rate_limit_error") => {
                ProviderFailureClass::RateLimit { retry_after: None }
            }
            Some("insufficient_quota" | "billing_error") => ProviderFailureClass::QuotaExceeded,
            Some("invalid_api_key" | "authentication_error") => {
                ProviderFailureClass::Authentication
            }
            Some("invalid_request_error" | "invalid_value" | "unsupported_parameter") => {
                ProviderFailureClass::InvalidRequest
            }
            Some("content_filter") => ProviderFailureClass::ContentPolicy,
            _ => ProviderFailureClass::ProviderInternal { retry_after: None },
        }
    };
    // The shared stream masks these provider words with the attempt's credential
    // before any fragment crosses its public boundary.
    let mut message = format!("the provider reported {kind}");
    for (label, text) in [
        ("status", response["status"].as_str()),
        ("code", code),
        ("reason", reason),
        ("message", error["message"].as_str()),
    ] {
        if let Some(text) = text {
            message.push_str(&format!("; {label}: {text}"));
        }
    }
    sink.fail_classified(class, classification, &message);
}

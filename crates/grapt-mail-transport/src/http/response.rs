//! Private schema projection, emitted only when a response fails closed decoding.
use super::*;

const MAX_DIAGNOSTIC: usize = 4096;
const MAX_PATTERNS: usize = 256;
const MAX_PRIVATE_BYTES: usize = 65536;

pub(super) struct Response {
    pub(super) value: Value,
    pub(super) evidence: Evidence,
}

impl Response {
    pub(super) fn decode<T>(
        self,
        decode: impl FnOnce(&Value) -> Result<T, OperationFailure>,
    ) -> Result<T, OperationFailure> {
        let result = decode(&self.value);
        self.evidence.observe("value", &result);
        result
    }
}

pub(super) struct Evidence {
    operation: &'static str,
    status: u16,
    content_type: &'static str,
    body: String,
}

impl Evidence {
    pub(super) fn capture(
        operation: &'static str,
        status: u16,
        content_type: &str,
        body: &[u8],
        capability: &str,
        args: &Value,
    ) -> Self {
        let mut private = vec![capability.to_owned()];
        collect(args, &mut private);
        let within = private.len() <= MAX_PATTERNS
            && private.iter().map(String::len).sum::<usize>() <= MAX_PRIVATE_BYTES;
        let content_type =
            if within && content_type == "application/json" && !secret(content_type, &private) {
                "application/json"
            } else {
                "[withheld or absent]"
            };
        let body = if !within {
            "[private-value bound exceeded]".into()
        } else if body.len() > MAX_DIAGNOSTIC {
            "[withheld: body exceeds diagnostic bound]".into()
        } else {
            match decode::strict_value(body) {
                Ok(value) => bounded(&project(&value, "", &private, 0).to_string()),
                Err(_) => "[withheld: invalid or duplicate JSON]".into(),
            }
        };
        Self {
            operation,
            status,
            content_type,
            body,
        }
    }

    pub(super) fn rejected_headers(
        operation: &'static str,
        status: u16,
        content_type: &str,
        capability: &str,
        args: &Value,
    ) -> Self {
        let mut evidence = Self::capture(operation, status, content_type, b"", capability, args);
        evidence.body = "[not read: invalid headers]".into();
        evidence
    }

    pub(super) fn observe<T>(&self, stage: &str, result: &Result<T, OperationFailure>) {
        if result.as_ref().is_err_and(|error| {
            error.cause == OperationCause::Connection(ConnectionFailure::InvalidEnvelope)
        }) {
            grapt_diagnostics::report(self.render(stage));
        }
    }

    fn render(&self, stage: &str) -> String {
        format!(
            "native mail invalid response operation={} stage={stage} status={} content_type={} body={}",
            self.operation, self.status, self.content_type, self.body
        )
    }
}

fn collect(value: &Value, private: &mut Vec<String>) {
    if private.len() > MAX_PATTERNS {
        return;
    }
    match value {
        Value::String(text) if !text.is_empty() => private.push(text.clone()),
        Value::Object(object) => {
            for value in object.values() {
                collect(value, private);
            }
        }
        Value::Array(array) => {
            for value in array {
                collect(value, private);
            }
        }
        _ => {}
    }
}

fn secret(text: &str, private: &[String]) -> bool {
    private
        .iter()
        .any(|value| !value.is_empty() && text.contains(value))
}

// Diagnostic admission is deliberately narrower than DTO decoding: unknown
// keys, payloads and free-form reasons never become log text. Only explicit
// schema labels and vetted failure vocabulary are public, even in malformed
// responses. This projection has no influence on transport admission.
fn project(value: &Value, field: &str, private: &[String], depth: usize) -> Value {
    if depth >= 16 {
        return json!("[depth limit]");
    }
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .enumerate()
                .map(|(i, (key, value))| {
                    let label = if schema_key(key) && !secret(key, private) {
                        key.clone()
                    } else {
                        format!("[withheld key {i}]")
                    };
                    (label, project(value, key, private, depth + 1))
                })
                .collect(),
        ),
        Value::Array(array) => Value::Array(
            array
                .iter()
                .map(|value| project(value, "", private, depth + 1))
                .collect(),
        ),
        Value::String(text) if vocabulary(field, text) && !secret(text, private) => value.clone(),
        Value::Bool(_) | Value::Null => value.clone(),
        Value::Number(_) => json!("[number]"),
        _ => json!("[private]"),
    }
}

fn schema_key(key: &str) -> bool {
    matches!(
        key,
        "ok" | "value"
            | "failure"
            | "family"
            | "tag"
            | "field"
            | "reason"
            | "source_state"
            | "effect"
            | "recovery"
            | "detail"
            | "completed"
            | "tasks_remaining"
            | "binding_id"
            | "generation"
            | "subscription_id"
            | "snapshot"
            | "token"
            | "joined_tasks"
            | "ticks_used"
            | "projects"
            | "source"
            | "target"
            | "protocol"
            | "adapter_version"
            | "verified_principal"
            | "local_recipient"
            | "mapping_version"
            | "takeover"
            | "limits"
            | "capabilities"
            | "messages"
            | "body"
            | "refs"
            | "source_cursor"
            | "source_binding"
            | "source_message"
            | "recipient"
            | "sender"
            | "dismissed"
            | "already_dismissed"
            | "by_principal"
            | "by_client"
    )
}

fn vocabulary(field: &str, text: &str) -> bool {
    match field {
        "family" => matches!(
            text,
            "OpenFailure"
                | "TransportFailure"
                | "DeliveryAckFailure"
                | "SubscriptionClosed"
                | "LifecycleOutcome"
        ),
        "tag" => matches!(
            text,
            "Unauthenticated"
                | "ForeignProjectMismatch"
                | "ExactPrincipalMismatch"
                | "RecipientMappingMissing"
                | "RecipientMismatch"
                | "MappingVersionMismatch"
                | "ProtocolVersionUnsupported"
                | "RequiredCapabilityMissing"
                | "LimitProfileIncomplete"
                | "TransportUnavailable"
                | "Disconnected"
                | "SubscriptionRefused"
                | "SnapshotExpired"
                | "CursorRegressed"
                | "PageCountExceeded"
                | "PageBodyBytesExceeded"
                | "PageDecodedBytesExceeded"
                | "MessageBodyExceeded"
                | "ReferenceCountExceeded"
                | "ReferenceBytesExceeded"
                | "CursorBytesExceeded"
                | "MalformedEnvelope"
                | "UnknownRequiredField"
                | "UnknownKind"
                | "UnknownArchiveReason"
                | "SupersessionConflict"
                | "ProjectProvenanceMissing"
                | "CursorInvalid"
                | "ReplayFrom"
                | "RecoveryUnavailable"
                | "MessageNeverEnumerated"
                | "AcknowledgementCoordinateMismatch"
                | "AcknowledgementDigestMismatch"
                | "AcknowledgementConflict"
                | "DeliveryAckUnsupported"
                | "ReadAckSubstitutionRefused"
                | "LocalShutdown"
                | "DisplacedByTakeover"
                | "ShutdownBudgetExhausted"
        ),
        "field" => failure::known_field(text).is_some(),
        "reason" => {
            CursorInvalidReason::parse(text).is_some()
                || matches!(text, "capability-required" | "operation-aborted")
        }
        "detail" => matches!(
            text,
            "source-file-replaced" | "source-prefix-unavailable" | "checkpoint-not-retained"
        ),
        "effect" => matches!(text, "none" | "unknown-retry-exact"),
        "source_state" => matches!(text, "absent" | "incomplete" | "unreadable"),
        _ => false,
    }
}

fn bounded(text: &str) -> String {
    let mut at = text.len().min(MAX_DIAGNOSTIC);
    while !text.is_char_boundary(at) {
        at = at.saturating_sub(1);
    }
    let mut output = text[..at].to_owned();
    if at < text.len() {
        output.push_str(" [truncated]");
    }
    output
}

#[cfg(test)]
mod tests;

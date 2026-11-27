//! Bounded server diagnostics without private continuity request handles.
use super::super::mcp::McpCallRefusal;
use grapt_mcp::session::Unusable;
use grapt_types::{Secret, SessionId};

const MAX_DETAIL: usize = 512;

pub(super) fn report(
    session: &SessionId,
    tool: &str,
    stage: &str,
    detail: &str,
    private: &[Secret<String>],
) {
    let detail = sanitise(detail, private);
    crate::diagnostics::report(format!(
        "stateengine-continuity session={session} tool={tool} stage={stage} error={detail}"
    ));
}

pub(super) fn report_call(
    session: &SessionId,
    tool: &str,
    error: &McpCallRefusal,
    private: &[Secret<String>],
) {
    report(
        session,
        tool,
        "call-with",
        &call_detail(error, private),
        &[],
    );
}

fn call_detail(error: &McpCallRefusal, private: &[Secret<String>]) -> String {
    let mut error = error.clone();
    // Redact raw fields before Debug can escape quotes, slashes or controls.
    match &mut error {
        McpCallRefusal::NoSuchServer(detail)
        | McpCallRefusal::NoSuchLocation(detail)
        | McpCallRefusal::Session(detail)
        | McpCallRefusal::Disconnected(detail)
        | McpCallRefusal::Protocol(detail)
        | McpCallRefusal::NotConnected(Unusable::Failed(detail)) => {
            *detail = redact(detail, private);
        }
        McpCallRefusal::ConnectionReplaced { approved, current } => {
            *approved = redact(approved, private);
            *current = redact(current, private);
        }
        McpCallRefusal::TimedOut
        | McpCallRefusal::Busy { .. }
        | McpCallRefusal::ProducerVersionChanged { .. }
        | McpCallRefusal::ProducerReadRetryRefused
        | McpCallRefusal::ProducerWriteRetryRefused
        | McpCallRefusal::NotConnected(_) => {}
    }
    sanitise(&format!("{error:?}"), &[])
}

fn redact(detail: &str, private: &[Secret<String>]) -> String {
    let mut detail = detail.to_owned();
    for value in private {
        if !value.expose().is_empty() {
            detail = detail.replace(value.expose(), "[private]");
        }
    }
    detail
}

fn sanitise(detail: &str, private: &[Secret<String>]) -> String {
    // Redact before truncation so the output cannot expose a partial handle.
    // Arguments and metadata are never logged; only the failure is considered.
    redact(detail, private)
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_DETAIL)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_call_failure_remains_distinct_from_generic_continuity_transport() {
        assert_eq!(call_detail(&McpCallRefusal::TimedOut, &[]), "TimedOut");
        let refusal = McpCallRefusal::Protocol("identity proof unavailable: git ENOENT".into());
        let detail = call_detail(&refusal, &[]);
        assert!(detail.contains("Protocol"));
        assert!(detail.contains("identity proof unavailable: git ENOENT"));
    }

    #[test]
    fn call_error_redacts_quoted_and_backslashed_private_handles_before_debug() {
        let private = [Secret::new("cursor\"DO-NOT-LOG\\end\n".to_owned())];
        let echoed = format!("identity proof rejected {}", private[0].expose());
        for error in [
            McpCallRefusal::Protocol(echoed.clone()),
            McpCallRefusal::NotConnected(Unusable::Failed(echoed.clone())),
            McpCallRefusal::ConnectionReplaced {
                approved: echoed.clone(),
                current: echoed.clone(),
            },
        ] {
            let detail = call_detail(&error, &private);
            assert!(detail.contains("[private]"));
            assert!(!detail.contains("DO-NOT-LOG"));
            assert!(!detail.contains(private[0].expose()));
            assert!(!detail.chars().any(char::is_control));
        }
    }

    #[test]
    fn private_request_handles_are_redacted_before_the_unicode_and_line_bound() {
        let capability = "capability-do-not-print".to_owned();
        let cursor = "cursor-do-not-print".to_owned();
        let packet = "packet-do-not-print".to_owned();
        let private = [capability.clone(), cursor.clone(), packet.clone()].map(Secret::new);
        let raw = format!(
            "server refused {capability} {cursor} {packet}\n{}",
            "界".repeat(600)
        );
        let detail = sanitise(&raw, &private);
        assert!(detail.contains("server refused [private] [private] [private]"));
        assert!(!detail.contains(&capability));
        assert!(!detail.contains(&cursor));
        assert!(!detail.contains(&packet));
        assert!(!detail.chars().any(char::is_control));
        assert_eq!(detail.chars().count(), MAX_DETAIL);
        let crossing = format!("{}{capability}", "x".repeat(MAX_DETAIL - 3));
        assert!(!sanitise(&crossing, &private).contains("cap"));
    }
}

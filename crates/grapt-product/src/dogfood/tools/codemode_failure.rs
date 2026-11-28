//! Worker failure classes and bounded, authenticated MCP diagnostics.
//!
//! Safe model-facing refusal mapping.
//! Adapted for typed worker provenance and private nested tool payloads;
//! see licenses/openai-codex.
use grapt_codemode::{GuestFailure, HostFailure};
use grapt_types::{ToolExecution, ToolFailureReason};
use serde::{Deserialize, Serialize};

const PREFIX: &str = "CodeMode cause: ";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cause {
    reason: GuestFailure,
    nested_failure: Option<HostFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mcp: Option<super::mcp_failure::Diagnostic>,
}

pub(in crate::dogfood) fn owns(
    tool: &str,
    execution: ToolExecution,
    permission: Option<&str>,
) -> bool {
    tool == super::EXECUTE_TOOL
        && execution == ToolExecution::Local
        && permission.is_none_or(|tool| tool == super::EXECUTE_TOOL)
}

pub(in crate::dogfood) fn capture(
    reason: GuestFailure,
    nested_failure: Option<HostFailure>,
    detail: Option<&str>,
) -> String {
    let diagnostic = match detail.and_then(grapt_codemode::bounded_detail) {
        Some(detail) => format!("CodeMode execution failed: {reason:?}: {detail}"),
        None => format!("CodeMode execution failed: {reason:?}"),
    };
    match serde_json::to_string(&Cause {
        reason,
        nested_failure,
        mcp: nested_failure
            .and_then(|reason| super::mcp_failure::guest_diagnostic(detail.unwrap_or(""), reason)),
    }) {
        Ok(cause) => format!("{PREFIX}{cause}\n{diagnostic}"),
        Err(_) => diagnostic,
    }
}

pub(in crate::dogfood) fn public(reason: ToolFailureReason, detail: &str) -> Option<String> {
    let category = category(reason, detail)?;
    let cause = detail
        .strip_prefix(PREFIX)
        .and_then(|s| s.split('\n').next())
        .filter(|line| line.len() <= 2048)
        .and_then(|line| serde_json::from_str::<Cause>(line).ok());
    Some(
        match cause
            .filter(|c| c.reason == GuestFailure::GuestError && c.nested_failure.is_some())
            .and_then(|c| c.mcp)
            .filter(super::mcp_failure::Diagnostic::valid)
        {
            Some(diagnostic) => diagnostic.render(category),
            None => category.to_owned(),
        },
    )
}

fn category(reason: ToolFailureReason, detail: &str) -> Option<&'static str> {
    use GuestFailure as G;
    use HostFailure as H;
    use ToolFailureReason as T;
    match (reason, detail) {
        (T::EffectUnknown, "CodeMode execution timed out") => return Some("CodeMode [timeout]: execution exceeded 30 seconds; split the work and verify any tool effects before retrying."),
        (T::EffectUnknown, "CodeMode worker settlement is unknown" | "CodeMode host exchange failed") => return Some("CodeMode [result_unknown]: the worker outcome is unknown; verify tool effects before retrying."),
        (T::CaptureFailed, "CodeMode result exceeded its output bound") => return Some("CodeMode [output_limit]: return fewer fields or a smaller result."),
        (T::InfrastructureFailed, _)
        | (
            T::EffectFailed,
            "CodeMode worker could not start"
            | "CodeMode worker exited unsuccessfully"
            | "CodeMode host exchange failed",
        ) => return Some("CodeMode [worker_failure]: the local worker failed; retry a small execution and report a repeated failure."),
        (T::EffectFailed, _) => {}
        (T::ApprovalUnavailable, _) => return Some("CodeMode [approval_unavailable]: approval unavailable (headless); attach an approval client before retrying the nested tool."),
        (T::AuthorityUnavailable, _) => return Some("CodeMode [permission_refused]: the Session's permission policy refused this execution; do not repeat it unless the policy changes."),
        (T::EffectUnknown, _) => return Some("CodeMode [result_unknown]: tool effects are unknown; verify the current state before retrying."),
        _ => return Some("CodeMode [cause_unavailable]: no public typed cause was retained; inspect this Session's failed call with history before retrying."),
    }
    let cause = detail
        .strip_prefix(PREFIX)
        .and_then(|text| text.split('\n').next())
        .filter(|line| line.len() <= 2048)
        .and_then(|line| serde_json::from_str::<Cause>(line).ok())
        .filter(|cause| cause.nested_failure.is_none() || cause.reason == G::GuestError)
        .or_else(|| {
            (!detail.starts_with(PREFIX))
                .then(|| legacy(detail))
                .flatten()
        });
    let Some(cause) = cause else {
        return Some("CodeMode [cause_unavailable]: no public typed cause was retained; inspect this Session's failed call with history before retrying.");
    };
    Some(match (cause.reason, cause.nested_failure) {
        (_, Some(H::ProducerReconnected)) => r#"CodeMode [producer_reconnected]: StateEngine was updated. Its MCP child reconnected; this request was not repeated. If this was a message send, it may or may not have been recorded. Before resending, call stateengine_msg_list({"from":"<your agent_id>","select":"newest","limit":20}) using your agent_id from stateengine_resume; inspect the matching outbox entry. Verify other effects before retrying."#,
        (_, Some(H::ProducerReconnectFailed)) => r#"CodeMode [producer_reconnect_failed]: StateEngine was updated. Its MCP child could not reconnect under this request's recovery limits; this request was not repeated. If this was a message send, it may or may not have been recorded. Before resending, call stateengine_msg_list({"from":"<your agent_id>","select":"newest","limit":20}) using your agent_id from stateengine_resume; inspect the matching outbox entry. Verify other effects before retrying."#,
        (_, Some(H::ProducerWriteRetryRefused)) => r#"CodeMode [producer_write_retry_refused]: StateEngine still refused after one keyed send retry. No further retry was made. If this was a message send, it may or may not have been recorded. Before resending, call stateengine_msg_list({"from":"<your agent_id>","select":"newest","limit":20}) using your agent_id from stateengine_resume; inspect the matching outbox entry. Verify other effects before retrying."#,
        (_, Some(H::ProducerReadRetryRefused)) => "CodeMode [producer_read_retry_refused]: StateEngine was updated; its replacement MCP child still refused after one read retry. No further retry was made; check the MCP connection before continuing.",
        (_, Some(H::ProducerVersionRefused)) => "CodeMode [producer_version_refused]: StateEngine refused this MCP client's build; reconnect its MCP connection before deliberately retrying. This call was not automatically repeated.",
        (_, Some(H::ConnectionReplaced)) => "CodeMode [connection_replaced]: the approved MCP connection changed; discover the tool again before deliberately retrying.",
        (_, Some(H::InvalidPlacement)) => "CodeMode [invalid_placement]: the MCP Session workspace placement is invalid; use a Session bound to an absolute workspace and report a persistent failure.",
        (_, Some(H::McpUnavailable)) => "CodeMode [mcp_unavailable]: the tool's MCP server is not connected at this Session location; check the connection before retrying.",
        (_, Some(H::McpProtocol)) => "CodeMode [mcp_protocol]: the MCP exchange was refused or malformed; inspect the connection and verify any effects before retrying.",
        (_, Some(H::McpToolRefused)) => "CodeMode [mcp_tool_refused]: the MCP tool reported a refusal; catch the individual call to inspect its bounded reason before deciding whether to retry.",
        (_, Some(H::IdentityArguments)) => "CodeMode [identity_arguments]: StateEngine refused identity arguments; use the current Session's identity and inspect the tool's schema with catalog.describe.",
        (_, Some(H::MessageKind)) => "CodeMode [message_kind]: StateEngine refused the message kind; inspect the allowed kind values with catalog.describe before sending again.",
        (_, Some(H::AuthorityReference)) => "CodeMode [authority_reference]: the recipient cannot read an authority reference; use a shared readable reference or have the coordinator share it before sending again.",
        (_, Some(H::InvalidArguments)) => "CodeMode [invalid_arguments]: nested tool arguments were refused; use catalog.describe(name) and pass an object matching its schema.",
        (_, Some(H::StaleCatalog)) => "CodeMode [stale_catalog]: the tool catalog changed; discover and describe the tool again before calling it.",
        (_, Some(H::AuthorityUnavailable)) => "CodeMode [permission_refused]: the nested tool was refused by the Session's permission policy; do not repeat it unless the policy changes.",
        (_, Some(H::ApprovalUnavailable)) => "CodeMode [approval_unavailable]: approval unavailable (headless); attach an approval client before retrying the nested tool.",
        (_, Some(H::ResultUnknown)) => "CodeMode [result_unknown]: the nested tool may have run; verify its effect before considering another call.",
        (_, Some(H::Capacity)) => "CodeMode [capacity]: reduce concurrent tool calls and split the execution.",
        (_, Some(H::Cancelled)) => "CodeMode [cancelled]: the nested tool was cancelled; check its state before continuing.",
        (_, Some(H::EffectFailed)) => "CodeMode [nested_tool_failed]: a nested tool reported failure; catch that call inside execute to inspect its refusal before retrying.",
        (G::InvalidSource, None) => "CodeMode [invalid_source]: JavaScript could not be parsed; correct its syntax and use await tools[name](arguments).",
        (G::TypeError, None) => "CodeMode [type_error]: JavaScript TypeError; check the exact tools[name] function with catalog.search and catalog.describe, and pass its declared argument object.",
        (G::ReferenceError, None) => "CodeMode [reference_error]: JavaScript ReferenceError; check variable names. Only tools and catalog are available; imports, filesystem, network and retained globals are unavailable.",
        (G::RangeError, None) => "CodeMode [range_error]: JavaScript RangeError; reduce recursion and allocation and split the work.",
        (G::GuestError, None) => "CodeMode [javascript_error]: JavaScript threw an exception; check variable names, discover exact tool names with catalog.search, and inspect arguments with catalog.describe.",
        (G::Interrupted | G::JobLimit, None) => "CodeMode [execution_limit]: JavaScript exceeded its execution limit; reduce loops and split the work.",
        (G::OutputLimit, None) => "CodeMode [output_limit]: return fewer fields or a smaller result.",
        (G::SerializationError, None) => "CodeMode [serialization_error]: return a JSON value; avoid cycles and unsupported values.",
        (G::UnresolvedPromise, None) => "CodeMode [unresolved_promise]: await tool calls; timers, network and retained background work are unavailable.",
        (G::HostProtocol, None) => "CodeMode [host_protocol]: the worker exchange failed; retry a small execution and report a repeated failure.",
        (G::ApprovalUnavailable, None) => "CodeMode [approval_unavailable]: approval unavailable (headless); attach an approval client before retrying the nested tool.",
    })
}

// Historic worker captures have no typed header. Recognize only the bounded
// product-owned wrapper and emit the same fixed categories, never its payload.
fn legacy(detail: &str) -> Option<Cause> {
    use GuestFailure as G;
    use HostFailure as H;
    let detail = detail.strip_prefix("CodeMode execution failed: ")?;
    let (reason, body) = detail.split_once(": ").unwrap_or((detail, ""));
    let mut reason = match reason {
        "GuestError" => G::GuestError,
        "InvalidSource" => G::InvalidSource,
        "Interrupted" => G::Interrupted,
        "JobLimit" => G::JobLimit,
        "OutputLimit" => G::OutputLimit,
        "SerializationError" => G::SerializationError,
        "UnresolvedPromise" => G::UnresolvedPromise,
        "HostProtocol" => G::HostProtocol,
        _ => return None,
    };
    let mut nested_failure = None;
    if reason == G::GuestError {
        for (prefix, class) in [
            ("TypeError:", G::TypeError),
            ("ReferenceError:", G::ReferenceError),
            ("RangeError:", G::RangeError),
        ] {
            if body.starts_with(prefix) {
                reason = class;
            }
        }
        if let Some(call) = body.strip_prefix("tool ") {
            let (name, tail) = call.split_once(" failed (")?;
            if name.len() > 256
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return None;
            }
            let (status, diagnostic) = tail.split_once(')')?;
            let status: H =
                serde_json::from_value(serde_json::Value::String(status.to_owned())).ok()?;
            let diagnostic = diagnostic.strip_prefix(": ").unwrap_or(diagnostic);
            let mut class =
                super::mcp_failure::server_refusal(name.starts_with("stateengine_"), diagnostic);
            if diagnostic.starts_with("ConnectionReplaced") {
                class = H::ConnectionReplaced;
            } else if let Some(protocol) = diagnostic
                .strip_prefix("Protocol(")
                .and_then(|text| text.strip_suffix(')'))
            {
                class = serde_json::from_str::<String>(protocol)
                    .ok()
                    .map(|text| {
                        super::mcp_failure::server_refusal(name.starts_with("stateengine_"), &text)
                    })
                    .filter(|class| *class != H::McpToolRefused)
                    .unwrap_or(H::McpProtocol);
            } else if diagnostic.contains("absolute")
                && (diagnostic.contains("location") || diagnostic.contains("placement"))
            {
                class = H::InvalidPlacement;
            }
            nested_failure = Some(if class == H::McpToolRefused {
                status
            } else {
                class
            });
        }
    }
    Some(Cause {
        reason,
        nested_failure,
        mcp: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_32_historical_census_shapes_now_have_safe_causes() {
        // Sanitized class/count table only; no historical arguments or bodies.
        let cases = [
            (16, "CodeMode execution failed: GuestError: tool stateengine_stateengine_msg_send failed (effect_failed): Error: mixed-engine-versions: PRIVATE", "[producer_version_refused]"),
            (2, "CodeMode execution failed: GuestError: tool stateengine_stateengine_tools failed (effect_failed): Error: identity agent_id PRIVATE", "[identity_arguments]"),
            (2, "CodeMode execution failed: GuestError: tool stateengine_stateengine_msg_send failed (effect_failed): Error: msg_send kind PRIVATE", "[message_kind]"),
            (1, "CodeMode execution failed: GuestError: tool stateengine_stateengine_msg_send failed (effect_failed): Error: authority refs unreadable PRIVATE", "[authority_reference]"),
            (3, "CodeMode execution failed: GuestError: tool stateengine_stateengine_msg_read failed (effect_failed): ConnectionReplaced { approved: PRIVATE }", "[connection_replaced]"),
            (1, "CodeMode execution failed: GuestError: tool stateengine_stateengine_msg_list failed (result_unknown): Protocol(\"PRIVATE\")", "[mcp_protocol]"),
            (2, "CodeMode execution failed: GuestError: tool stateengine_stateengine_msg_list failed (effect_failed): location directory must be absolute PRIVATE", "[invalid_placement]"),
            (3, "CodeMode execution failed: GuestError: TypeError: tools.PRIVATE is not a function", "[type_error]"),
            (1, "CodeMode execution failed: GuestError: TypeError: PRIVATE", "[type_error]"),
        ];
        let mut count = 1; // One host exchange failure, separately typed below.
        for (rows, diagnostic, expected) in cases {
            let text = public(ToolFailureReason::EffectFailed, diagnostic)
                .unwrap_or_else(|| panic!("historical shape lost its cause"));
            assert!(text.contains(expected), "{expected}: {text}");
            assert!(!text.contains("PRIVATE"));
            count += rows;
        }
        assert!(public(
            ToolFailureReason::EffectUnknown,
            "CodeMode host exchange failed"
        )
        .is_some_and(|text| text.contains("[result_unknown]")));
        assert_eq!(count, 32);
    }

    #[test]
    fn fixed_public_classes_never_echo_guest_or_mcp_payloads() {
        let secret = "Authorization: Bearer PRIVATE_SECRET秘密".repeat(100);
        for nested in [
            None,
            Some(HostFailure::InvalidArguments),
            Some(HostFailure::AuthorityUnavailable),
            Some(HostFailure::EffectFailed),
            Some(HostFailure::ResultUnknown),
            Some(HostFailure::ProducerReconnected),
            Some(HostFailure::ProducerReconnectFailed),
            Some(HostFailure::ProducerReadRetryRefused),
            Some(HostFailure::ProducerWriteRetryRefused),
        ] {
            let captured = capture(GuestFailure::GuestError, nested, Some(&secret));
            let public = public(ToolFailureReason::EffectFailed, &captured)
                .unwrap_or_else(|| panic!("typed cause lost"));
            assert!(public.len() <= 512);
            assert!(!public.contains("PRIVATE_SECRET"));
            assert!(!public.contains("秘密"));
            assert!(public.starts_with("CodeMode ["));
        }
    }

    #[test]
    fn legacy_text_and_foreign_execution_do_not_gain_typed_provenance() {
        for detail in ["tool echo failed (invalid_arguments): SECRET",
            "CodeMode cause: {\"reason\":\"guest_error\",\"nested_failure\":null,\"secret\":\"PRIVATE\"}",
            "CodeMode cause: {\"reason\":\"invalid_source\",\"nested_failure\":\"effect_failed\"}"] {
            assert!(public(ToolFailureReason::EffectFailed, detail).is_some_and(|text| text.contains("[cause_unavailable]")));
        }
        let old = public(
            ToolFailureReason::EffectFailed,
            "CodeMode execution failed: GuestError: PRIVATE",
        );
        assert!(old
            .is_some_and(|text| text.contains("[javascript_error]") && !text.contains("PRIVATE")));
        assert!(owns("execute", ToolExecution::Local, None));
        assert!(owns("execute", ToolExecution::Local, Some("execute")));
        assert!(!owns("execute", ToolExecution::ProviderHosted, None));
        assert!(!owns("foreign_execute", ToolExecution::Local, None));
        assert!(!owns("execute", ToolExecution::Local, Some("read")));
    }

    #[test]
    fn unknown_effect_and_argument_refusals_give_different_next_actions() {
        let invalid = capture(
            GuestFailure::GuestError,
            Some(HostFailure::InvalidArguments),
            None,
        );
        assert!(public(ToolFailureReason::EffectFailed, &invalid)
            .is_some_and(|text| text.contains("catalog.describe")));
        let unknown = capture(
            GuestFailure::GuestError,
            Some(HostFailure::ResultUnknown),
            None,
        );
        assert!(public(ToolFailureReason::EffectFailed, &unknown)
            .is_some_and(|text| text.contains("verify its effect")));
        assert!(public(ToolFailureReason::EffectUnknown, &invalid)
            .is_some_and(|text| text.contains("[result_unknown]")));
    }
}

//! The native Messages consumer retains destination and authentication custody.
use super::*;
use crate::{
    CodexInputItem, CodexToolText, CredentialOwner, CredentialRenewalFuture,
    CredentialRenewalRefusal, CredentialRevision, CurrentCredential, EndpointConfig,
    MessagesAuthMode, ProviderCredential, ProviderReplay, ProviderRequest, ToolName,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn failure_of(fragments: &[AttemptFragment]) -> Option<crate::ProviderFailureClass> {
    fragments.iter().find_map(|fragment| match fragment {
        AttemptFragment::ProviderFailure { class, .. } => Some(class.clone()),
        _ => None,
    })
}

fn credential(base: &str, mode: MessagesAuthMode) -> TestResult<ProviderCredential> {
    Ok(ProviderCredential::inject(
        grapt_types::AccountId::new("acct_fixture").ok_or("account")?,
        grapt_types::CredentialVersion::new("7").ok_or("version")?,
        mode.placement(),
        grapt_types::Secret::new("synthetic-messages-token".into()),
    )?
    .bind_messages_destination(base, mode)?)
}

fn call(base: &str, approved: &str, mode: MessagesAuthMode) -> TestResult<AttemptInput> {
    let mut call = input(
        "att_messages_bound",
        base,
        ProtocolFamily::AnthropicMessages,
        anthropic_request(),
    );
    call.endpoint = EndpointConfig::new()
        .with_attempt_override(base)
        .with_messages_auth(mode)
        .resolve(ProtocolFamily::AnthropicMessages)?
        .bind(ProtocolFamily::AnthropicMessages, route());
    call.lease = credential(approved, mode)?.lease()?;
    Ok(call)
}

#[tokio::test]
async fn messages_foreign_destination_and_auth_modes_refuse_before_sending() -> TestResult {
    let fixture = serve(200, &[], Vec::new()).await;
    let foreign = serve(200, &[], Vec::new()).await;
    let approved = format!("{}/approved/v1", fixture.base);
    for destination in [format!("{}/other/v1", fixture.base), foreign.base.clone()] {
        assert!(crate::AttemptOpener::route(
            ProtocolFamily::AnthropicMessages,
            deadlines(),
            limits(1),
            EndpointConfig::new()
                .with_attempt_override(&destination)
                .with_messages_auth(MessagesAuthMode::Bearer),
            route(),
            "secode-fixture",
            credential(&approved, MessagesAuthMode::Bearer)?,
        )
        .is_err());
        assert!(adapter(1)
            .open(call(&destination, &approved, MessagesAuthMode::Bearer)?)
            .await
            .is_err());
    }
    for endpoint_mode in [MessagesAuthMode::ApiKey, MessagesAuthMode::Bearer] {
        let credential_mode = if endpoint_mode == MessagesAuthMode::Bearer {
            MessagesAuthMode::ApiKey
        } else {
            MessagesAuthMode::Bearer
        };
        let mut input = call(&approved, &approved, endpoint_mode)?;
        input.lease = credential(&approved, credential_mode)?.lease()?;
        assert!(adapter(1).open(input).await.is_err());
        assert!(crate::AttemptOpener::route(
            ProtocolFamily::AnthropicMessages,
            deadlines(),
            limits(1),
            EndpointConfig::new()
                .with_attempt_override(&approved)
                .with_messages_auth(endpoint_mode),
            route(),
            "secode-fixture",
            credential(&approved, credential_mode)?,
        )
        .is_err());
    }
    assert!(EndpointConfig::new()
        .with_messages_auth(MessagesAuthMode::Bearer)
        .resolve(ProtocolFamily::AnthropicMessages)
        .is_err());
    assert!(EndpointConfig::new()
        .with_attempt_override(&approved)
        .with_messages_auth(MessagesAuthMode::Bearer)
        .resolve(ProtocolFamily::OpenAiResponses)
        .is_err());
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    assert_eq!(foreign.requests.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn messages_both_auth_modes_keep_native_terminal_and_signed_tool_replay() -> TestResult {
    for mode in [MessagesAuthMode::ApiKey, MessagesAuthMode::Bearer] {
        let fixture = serve_sequence(200, &[], vec![vec![sse(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"inspect"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SYNTHETIC_MESSAGES_SIGNATURE"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_messages","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"fixture.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#,
            r#"{"type":"message_stop"}"#,
        ])], vec![sse(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#,
            r#"{"type":"message_stop"}"#,
        ])]]).await;
        let base = format!("{}/approved/v1", fixture.base);
        let provider = adapter(1);
        let first = drain(provider.open(call(&base, &base, mode)?).await?).await;
        assert_eq!(finish_of(&first), Some(FinishReason::ToolCalls));
        assert!(!format!("{first:?}").contains("SYNTHETIC_MESSAGES_SIGNATURE"));
        let bytes = first
            .iter()
            .find_map(|fragment| match fragment {
                AttemptFragment::ReasoningClosed { state, .. } => state.capture_bytes(),
                _ => None,
            })
            .ok_or("signed thinking state")?;
        let mut followup = call(&base, &base, mode)?;
        let ProviderRequest::AnthropicMessages(request) = &mut followup.request else {
            return Err("Messages request".into());
        };
        let replay = ProviderReplay::restore(
            request.model.provider().clone(),
            request.model.model().clone(),
            request.model.variant().cloned(),
            std::str::from_utf8(bytes)?,
        )?;
        let tool_call = grapt_types::ToolCallId::new("call_messages").ok_or("tool call")?;
        request.messages.extend([
            CodexInputItem::ProviderReplay(replay),
            CodexInputItem::ToolCall {
                call: tool_call.clone(),
                tool: ToolName::new("read").ok_or("tool")?,
                arguments: CodexToolText::try_new(r#"{"path":"fixture.txt"}"#.into())?,
            },
            CodexInputItem::ToolOutput {
                call: tool_call,
                output: CodexToolText::try_new("fixture contents".into())?,
                is_error: false,
            },
        ]);
        let final_reply = drain(provider.open(followup).await?).await;
        assert_eq!(finish_of(&final_reply), Some(FinishReason::Stop));
        assert_eq!(text_of(&final_reply), "done");
        assert_eq!(provider.active_len(), 0);
        let head = fixture.head.lock().map_err(|_| "head lock")?.clone();
        let bearer = mode == MessagesAuthMode::Bearer;
        assert_eq!(
            head.matches("authorization: bearer synthetic-messages-token")
                .count(),
            usize::from(bearer)
        );
        assert_eq!(
            head.matches("x-api-key: synthetic-messages-token").count(),
            usize::from(!bearer)
        );
        assert!(head.contains("anthropic-version: 2023-06-01"));
        assert!(head.starts_with("post /approved/v1/messages "));
        let raw = fixture.body.lock().map_err(|_| "body lock")?.clone();
        let body: serde_json::Value = serde_json::from_str(&raw)?;
        assert_eq!(
            body["messages"][1]["content"][0],
            serde_json::json!({
                "type":"thinking", "thinking":"inspect", "signature":"SYNTHETIC_MESSAGES_SIGNATURE",
            })
        );
        assert_eq!(body["messages"][1]["content"][1]["id"], "call_messages");
        assert_eq!(
            body["messages"][2]["content"][0]["tool_use_id"],
            "call_messages"
        );
        assert_eq!(
            body["messages"][2]["content"][0]["content"],
            "fixture contents"
        );
        assert!(!raw.contains("synthetic-messages-token"));
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    }
    Ok(())
}

struct Renewal {
    base: String,
    mode: MessagesAuthMode,
    keep_binding: bool,
}
impl CredentialOwner for Renewal {
    fn current(&self) -> CredentialRenewalFuture<'_, CurrentCredential> {
        Box::pin(async move {
            let mut current = ProviderCredential::inject(
                grapt_types::AccountId::new("acct_fixture")
                    .ok_or(CredentialRenewalRefusal::Unavailable)?,
                grapt_types::CredentialVersion::new("7")
                    .ok_or(CredentialRenewalRefusal::Unavailable)?,
                self.mode.placement(),
                grapt_types::Secret::new("synthetic-renewed-token".into()),
            )
            .map_err(|_| CredentialRenewalRefusal::Unavailable)?;
            current = if self.keep_binding {
                current.bind_messages_destination(&self.base, self.mode)
            } else {
                current.bind_destination(&self.base)
            }
            .map_err(|_| CredentialRenewalRefusal::Unavailable)?;
            Ok(CurrentCredential {
                credential: current,
                revision: CredentialRevision::new(1),
            })
        })
    }
    fn recover(&self, _: CredentialRevision) -> CredentialRenewalFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }
    fn accepted(&self, _: CredentialRevision) -> CredentialRenewalFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn messages_renewal_cannot_drop_or_change_the_captured_base_and_auth_mode() -> TestResult {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Change {
        Preserved,
        Path,
        Auth,
        Binding,
        Origin,
    }
    for change in [
        Change::Preserved,
        Change::Path,
        Change::Auth,
        Change::Binding,
        Change::Origin,
    ] {
        let fixture = serve(
            200,
            &[],
            vec![sse(&[
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
                r#"{"type":"message_stop"}"#,
            ])],
        )
        .await;
        let base = format!("{}/approved/v1", fixture.base);
        let owner = Renewal {
            base: if change == Change::Path {
                format!("{}/foreign/v1", fixture.base)
            } else if change == Change::Origin {
                "https://foreign.invalid/v1".into()
            } else {
                base.clone()
            },
            mode: if change == Change::Auth {
                MessagesAuthMode::ApiKey
            } else {
                MessagesAuthMode::Bearer
            },
            keep_binding: change != Change::Binding,
        };
        let provider = adapter(1).with_credential_owner(Arc::new(owner));
        let fragments = drain(
            provider
                .open(call(&base, &base, MessagesAuthMode::Bearer)?)
                .await?,
        )
        .await;
        assert_eq!(
            finish_of(&fragments),
            (change == Change::Preserved).then_some(FinishReason::Stop),
            "{change:?}"
        );
        assert_eq!(
            failure_of(&fragments),
            (change != Change::Preserved).then_some(crate::ProviderFailureClass::Authentication),
            "{change:?}"
        );
        assert_eq!(
            fixture.requests.load(Ordering::SeqCst),
            usize::from(change == Change::Preserved),
            "{change:?}"
        );
        assert_eq!(provider.active_len(), 0);
        if change == Change::Preserved {
            let head = fixture.head.lock().map_err(|_| "head lock")?.clone();
            assert_eq!(
                head.matches("authorization: bearer synthetic-renewed-token")
                    .count(),
                1
            );
            assert!(!head.contains("synthetic-messages-token"));
        }
    }
    Ok(())
}

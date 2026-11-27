use super::*;

pub(super) struct EffectFailure {
    pub reason: ToolFailureReason,
    pub detail: String,
}

impl From<String> for EffectFailure {
    fn from(detail: String) -> Self {
        Self {
            reason: ToolFailureReason::EffectFailed,
            detail,
        }
    }
}
impl From<&str> for EffectFailure {
    fn from(detail: &str) -> Self {
        detail.to_owned().into()
    }
}

fn refusal(
    error: crate::dogfood::mcp::McpCallRefusal,
    bound_stateengine: bool,
    machine: bool,
) -> EffectFailure {
    use crate::dogfood::mcp::McpCallRefusal;
    let reason = match &error {
        McpCallRefusal::TimedOut
        | McpCallRefusal::Disconnected(_)
        | McpCallRefusal::Protocol(_)
        | McpCallRefusal::ProducerVersionChanged { .. }
        | McpCallRefusal::ProducerReadRetryRefused
        | McpCallRefusal::ProducerWriteRetryRefused => ToolFailureReason::EffectUnknown,
        _ => ToolFailureReason::EffectFailed,
    };
    let cause = match &error {
        McpCallRefusal::ProducerVersionChanged { recovered: true } => {
            grapt_codemode::HostFailure::ProducerReconnected
        }
        McpCallRefusal::ProducerVersionChanged { recovered: false } => {
            grapt_codemode::HostFailure::ProducerReconnectFailed
        }
        McpCallRefusal::ProducerReadRetryRefused => {
            grapt_codemode::HostFailure::ProducerReadRetryRefused
        }
        McpCallRefusal::ProducerWriteRetryRefused => {
            grapt_codemode::HostFailure::ProducerWriteRetryRefused
        }
        McpCallRefusal::ConnectionReplaced { .. } => {
            grapt_codemode::HostFailure::ConnectionReplaced
        }
        McpCallRefusal::NoSuchServer(_)
        | McpCallRefusal::NoSuchLocation(_)
        | McpCallRefusal::NotConnected(_) => grapt_codemode::HostFailure::McpUnavailable,
        McpCallRefusal::TimedOut | McpCallRefusal::Disconnected(_) => {
            grapt_codemode::HostFailure::ResultUnknown
        }
        McpCallRefusal::Protocol(detail) => {
            match super::mcp_failure::server_refusal(bound_stateengine, detail) {
                grapt_codemode::HostFailure::McpToolRefused => {
                    grapt_codemode::HostFailure::McpProtocol
                }
                cause => cause,
            }
        }
        McpCallRefusal::Busy { .. } => grapt_codemode::HostFailure::Capacity,
        McpCallRefusal::Session(_) => grapt_codemode::HostFailure::McpToolRefused,
    };
    let detail = match error {
        McpCallRefusal::ProducerVersionChanged { recovered: true } =>
            r#"StateEngine was updated. Its MCP child reconnected; this request was not repeated. If this was a message send, it may or may not have been recorded. Before resending, call stateengine_msg_list({"from":"<your agent_id>","select":"newest","limit":20}) using your agent_id from stateengine_resume; inspect the matching outbox entry. Verify other effects before retrying."#.into(),
        McpCallRefusal::ProducerVersionChanged { recovered: false } =>
            r#"StateEngine was updated. Its MCP child could not reconnect under this request's recovery limits; this request was not repeated. If this was a message send, it may or may not have been recorded. Before resending, call stateengine_msg_list({"from":"<your agent_id>","select":"newest","limit":20}) using your agent_id from stateengine_resume; inspect the matching outbox entry. Verify other effects before retrying."#.into(),
        McpCallRefusal::ProducerReadRetryRefused =>
            "StateEngine was updated. Its replacement MCP child still refused after one read retry; no further retry was made.".into(),
        McpCallRefusal::ProducerWriteRetryRefused =>
            r#"StateEngine still refused after one keyed send retry. No further retry was made. If this was a message send, it may or may not have been recorded. Before resending, call stateengine_msg_list({"from":"<your agent_id>","select":"newest","limit":20}) using your agent_id from stateengine_resume; inspect the matching outbox entry. Verify other effects before retrying."#.into(),
        // Keep the server representation intact until private-value redaction.
        McpCallRefusal::Protocol(detail) if machine => format!("MCP protocol: {detail}"),
        McpCallRefusal::Disconnected(detail) if machine => format!("MCP disconnected: {detail}"),
        McpCallRefusal::Session(detail) if machine => format!("MCP session refused: {detail}"),
        McpCallRefusal::NotConnected(grapt_mcp::session::Unusable::Failed(detail)) if machine => format!("MCP not connected: {detail}"),
        other => format!("{other:?}"),
    };
    EffectFailure {
        reason,
        detail: super::mcp_failure::capture(cause, &detail, machine),
    }
}

pub(super) struct McpCallContext<'a> {
    pub attempt: Option<&'a grapt_types::PhysicalAttemptId>,
    pub service: Option<&'a Path>,
    pub session: &'a SessionId,
    pub tool: &'a str,
    pub custody: &'a Mutex<crate::dogfood::context::SessionCapabilities>,
    pub machine: Option<&'a BoundedArtifactWriter<FilesystemArtifactStore>>,
}

/// Dispatch one approved MCP call, and turn its outcome into a tool terminal.
///
/// The call id is the SESSION's tool-call id, so the connection's ledger and
/// this execution agree on what "already ran" means. That is what makes a
/// retry after an ambiguous outcome refuse rather than run the tool twice.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) async fn run_mcp(
    mcp: &crate::dogfood::mcp::McpRegistry,
    location: &str,
    route: &crate::dogfood::mcp::McpRoute,
    call: &ToolCallId,
    arguments: serde_json::Value,
    forms: &crate::dogfood::elicitation::FormProducer,
    context: McpCallContext<'_>,
) -> Result<Vec<u8>, EffectFailure> {
    let McpCallContext {
        attempt,
        service,
        session,
        tool,
        custody,
        machine,
    } = context;
    // THE SERVER'S OWN BUDGET, asked for rather than assumed. A fixed constant
    // here ignored the `timeout.execution` a user configured, which is half of
    // the reason that field did nothing.
    let admitted = tokio::time::Instant::now();
    let budget = mcp
        .execution_budget(location, &route.server)
        .await
        .unwrap_or_else(|| match route.launch.declaration.as_ref() {
            crate::dogfood::mcp::Declaration::Local { timeouts, .. }
            | crate::dogfood::mcp::Declaration::Remote { timeouts, .. } => timeouts.execution,
            crate::dogfood::mcp::Declaration::Disabled => MCP_CALL_BUDGET,
        });
    let (metadata, placement, progress_call) = {
        let inbox = forms
            .inbox
            .lock()
            .map_err(|_| "StateEngine binding lock poisoned")?;
        crate::dogfood::session_config::validate_registry_key(&inbox, session, location)?;
        // The registry key isolates the connection generation; forms bind the
        // Session's real placement, never that opaque lookup identity.
        let placement = grapt_session::SessionStore::new(inbox.store().connection())
            .record(session)
            .map_err(|_| "MCP Session placement is unavailable")?
            .placement;
        let custody = custody
            .lock()
            .map_err(|_| "StateEngine capability lock poisoned")?;
        (
            crate::dogfood::context::call_metadata(&inbox, &custody, session, &route.server)?,
            placement,
            crate::dogfood::continuation::progress::owner(inbox.store(), session, call)
                .ok()
                .flatten(),
        )
    };
    let producer = forms;
    let mut forms = forms
        .connect(placement.directory(), &route.server)
        .map_err(|detail| EffectFailure {
            reason: ToolFailureReason::EffectFailed,
            detail: super::mcp_failure::capture(
                grapt_codemode::HostFailure::InvalidPlacement,
                &detail,
                machine.is_some(),
            ),
        })?;
    if let Some((display_call, message)) = progress_call.as_ref() {
        forms.observe_progress(session, message, display_call, call, tool);
    }
    let bound_stateengine = route.server == "stateengine" && metadata.is_some();
    let private = super::mcp_failure::private_values(&arguments, metadata.as_ref());
    let mut outcome = mcp
        .call_with(
            location,
            &route.server,
            &route.tool,
            // The connection this call was APPROVED against. A replacement is
            // refused rather than substituted.
            &route.launch,
            call.as_str(),
            arguments.clone(),
            budget.saturating_sub(admitted.elapsed()),
            Some(&mut forms),
            attempt,
            metadata.clone(),
        )
        .await;
    // NoSuchLocation is pre-dispatch: no exchange has begun. Re-admit once
    // through ordinary status admission; all other outcomes remain unreplayed.
    if matches!(
        outcome,
        Err(crate::dogfood::mcp::McpCallRefusal::NoSuchLocation(_))
    ) {
        outcome = match super::mcp_recovery::readmit(
            mcp,
            location,
            route,
            producer,
            session,
            service,
            budget.saturating_sub(admitted.elapsed()),
        )
        .await
        {
            Ok(human_paused) => match admitted.checked_add(human_paused) {
                Some(admitted) => {
                    mcp.call_with(
                        location,
                        &route.server,
                        &route.tool,
                        &route.launch,
                        call.as_str(),
                        arguments,
                        budget.saturating_sub(admitted.elapsed()),
                        Some(&mut forms),
                        attempt,
                        metadata,
                    )
                    .await
                }
                None => Err(crate::dogfood::mcp::McpCallRefusal::TimedOut),
            },
            Err(error) => Err(error),
        };
    }
    forms.suspend().map_err(|error| EffectFailure {
        reason: ToolFailureReason::EffectUnknown,
        detail: format!(
            "MCP form settlement failed: {error}; call refusal: {:?}",
            outcome.as_ref().err()
        ),
    })?;
    let outcome = outcome.map_err(|error| {
        let mut failure = refusal(error, bound_stateengine, machine.is_some());
        if machine.is_some() {
            failure.detail = super::mcp_failure::routed(&failure.detail, route, private.as_deref());
        }
        failure
    })?;
    // A tool that ran and reported failure is a FAILED effect carrying the
    // server's own message, not a successful one with sad text: the model must
    // be able to tell the two apart, and so must a receipt.
    if outcome.is_error() {
        let cause = super::mcp_failure::server_refusal(bound_stateengine, outcome.text());
        let detail = super::mcp_failure::capture(cause, outcome.text(), machine.is_some());
        return Err(if machine.is_some() {
            super::mcp_failure::routed(&detail, route, private.as_deref())
        } else {
            detail
        }
        .into());
    }
    if machine.is_some() {
        validate_output(route.output_schema.as_deref(), outcome.structured())?;
    }
    match machine {
        Some(artifacts) => machine_result(&outcome, artifacts).map_err(Into::into),
        None => Ok(outcome.text().as_bytes().to_vec()),
    }
}

fn validate_output(schema: Option<&str>, value: Option<&serde_json::Value>) -> Result<(), String> {
    if let Some(schema) = schema {
        let value = value.ok_or("MCP declared output is absent")?;
        let encoded = grapt_codemode::bounded_json(value, grapt_codemode::ARGUMENT_BYTES)
            .map_err(|_| "MCP declared output exceeds its bound")?;
        super::schema::validate(schema, &encoded)
            .map_err(|_| "MCP output does not satisfy its declared schema")?;
    }
    Ok(())
}

fn binary_blocks(outcome: &grapt_mcp::ToolCallOutcome) -> Result<Vec<(Vec<u8>, String)>, String> {
    use base64::Engine as _;
    let mut files = Vec::new();
    let mut binary_bytes = 0usize;
    for block in outcome.content() {
        let (encoded, media) = match block.get("type").and_then(serde_json::Value::as_str) {
            Some("image" | "audio") => (block.get("data"), block.get("mimeType")),
            Some("resource") => match block.get("resource") {
                Some(resource) if resource.get("blob").is_some() => {
                    (resource.get("blob"), resource.get("mimeType"))
                }
                Some(resource)
                    if resource
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .is_some() =>
                {
                    continue
                }
                _ => return Err("MCP embedded resource is malformed or unsupported".into()),
            },
            Some("text") => continue,
            _ => return Err("CodeMode MCP content type is unsupported".into()),
        };
        let encoded = encoded
            .and_then(serde_json::Value::as_str)
            .ok_or("MCP binary data is malformed")?;
        let media = media
            .and_then(serde_json::Value::as_str)
            .ok_or("MCP binary media type is malformed")?;
        if media.is_empty() || media.len() > 256 || files.len() >= 32 {
            return Err("MCP binary metadata exceeds its bound".into());
        }
        if encoded.len() > 87_384 {
            return Err("MCP binary data exceeds its encoded bound".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "MCP binary data is not valid base64")?;
        binary_bytes = binary_bytes.saturating_add(bytes.len());
        if bytes.len() > 64 * 1024 || binary_bytes > 512 * 1024 {
            return Err("MCP binary data exceeds its capture bound".into());
        }
        files.push((bytes, media.to_owned()));
    }
    Ok(files)
}

fn machine_result(
    outcome: &grapt_mcp::ToolCallOutcome,
    artifacts: &BoundedArtifactWriter<FilesystemArtifactStore>,
) -> Result<Vec<u8>, String> {
    let mut files = Vec::new();
    for (bytes, media) in binary_blocks(outcome)? {
        let captured = artifacts
            .capture_sensitive(std::io::Cursor::new(bytes))
            .map_err(|_| "MCP binary capture failed")?;
        if !captured.disposition().is_complete() {
            return Err("MCP binary capture is truncated".into());
        }
        let reference = captured.reference();
        files.push(
            serde_json::json!({"id":reference.id().as_str(),"digest":reference.digest().as_str(),
            "length":reference.length(),"mimeType":media,"sensitivity":"secret-bearing"}),
        );
    }
    let value = outcome.structured().cloned().unwrap_or_else(|| {
        let text = outcome
            .content()
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(serde_json::Value::as_str) == Some("text") {
                    block.get("text").and_then(serde_json::Value::as_str)
                } else {
                    block.get("resource")?.get("text")?.as_str()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(text)
        }
    });
    grapt_codemode::bounded_json(
        &serde_json::json!({"value":value,"artifacts":files}),
        grapt_codemode::ARGUMENT_BYTES,
    )
    .map_err(|_| "MCP machine output exceeds its bound".into())
}

/// How long one MCP tool call may take when the registry holds no live server
/// to ask.
///
/// Deliberately finite: a server that never answers must not hold a Step open
/// forever. A LIVE server's own configured `timeout.execution` takes
/// precedence over this; it is reached only when the connection is already
/// gone, in which case the call is about to be refused anyway.
const MCP_CALL_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_capture_bounds_and_malformed_results_refuse_before_capture(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use base64::Engine as _;
        let block = |bytes: usize| {
            serde_json::json!({"type":"image","mimeType":"image/png",
            "data":base64::engine::general_purpose::STANDARD.encode(vec![0;bytes])})
        };
        let decode = |blocks: Vec<serde_json::Value>| -> Result<_, Box<dyn std::error::Error>> {
            Ok(grapt_mcp::accept_call(
                grapt_mcp::McpBounds::conservative()?,
                &serde_json::json!({"content":blocks,"isError":false}),
            )?)
        };
        assert_eq!(
            binary_blocks(&decode(vec![block(64 * 1024)])?)?[0].0.len(),
            64 * 1024
        );
        for blocks in [
            vec![block(64 * 1024 + 1)],
            vec![block(1); 33],
            vec![block(64 * 1024); 9],
            vec![serde_json::json!({"type":"image","mimeType":"image/png","data":"%%%"})],
            vec![serde_json::json!({"type":"image","mimeType":"","data":"AA=="})],
        ] {
            assert!(binary_blocks(&decode(blocks)?).is_err());
        }
        Ok(())
    }
    #[test]
    fn uncertain_mcp_effects_keep_their_typed_classification() {
        use crate::dogfood::mcp::McpCallRefusal;
        for error in [
            McpCallRefusal::TimedOut,
            McpCallRefusal::Disconnected("closed".into()),
            McpCallRefusal::Protocol("invalid reply".into()),
            McpCallRefusal::ProducerVersionChanged { recovered: true },
            McpCallRefusal::ProducerVersionChanged { recovered: false },
            McpCallRefusal::ProducerReadRetryRefused,
            McpCallRefusal::ProducerWriteRetryRefused,
        ] {
            assert_eq!(
                refusal(error, false, false).reason,
                ToolFailureReason::EffectUnknown
            );
        }
        for error in [
            McpCallRefusal::Busy { waiting: 1 },
            McpCallRefusal::Session("refused before send".into()),
            McpCallRefusal::ConnectionReplaced {
                approved: "a".into(),
                current: "b".into(),
            },
        ] {
            assert_eq!(
                refusal(error, false, false).reason,
                ToolFailureReason::EffectFailed
            );
        }
    }
    #[test]
    fn producer_version_recovery_names_its_outcome_and_never_claims_replay_safety() {
        use crate::dogfood::mcp::McpCallRefusal;
        let recovered = refusal(
            McpCallRefusal::ProducerVersionChanged { recovered: true },
            true,
            true,
        );
        assert!(recovered.detail.contains("reconnected"));
        assert!(recovered.detail.contains("this request was not repeated"));
        let failed = refusal(
            McpCallRefusal::ProducerVersionChanged { recovered: false },
            true,
            true,
        );
        assert!(failed.detail.contains("could not reconnect"));
        assert!(failed.detail.contains("this request was not repeated"));
        let retried = refusal(McpCallRefusal::ProducerReadRetryRefused, true, true);
        assert!(retried
            .detail
            .contains("still refused after one read retry"));
        assert!(!retried.detail.contains("not repeated"));
        let cause = super::super::mcp_failure::nested(&retried.detail)
            .unwrap_or_else(|| panic!("read retry cause absent"));
        assert_eq!(cause, grapt_codemode::HostFailure::ProducerReadRetryRefused);
        let captured = super::super::codemode_failure::capture(
            grapt_codemode::GuestFailure::GuestError,
            Some(cause),
            Some("PRIVATE"),
        );
        let public =
            super::super::codemode_failure::public(ToolFailureReason::EffectFailed, &captured)
                .unwrap_or_else(|| panic!("read retry public cause absent"));
        assert!(public.contains("still refused after one read retry"));
        assert!(!public.contains("not repeated"));
        assert!(!public.contains("PRIVATE"));
        for (failure, cause, phrase) in [
            (
                recovered,
                grapt_codemode::HostFailure::ProducerReconnected,
                "child reconnected",
            ),
            (
                failed,
                grapt_codemode::HostFailure::ProducerReconnectFailed,
                "could not reconnect",
            ),
        ] {
            assert_eq!(
                super::super::mcp_failure::nested(&failure.detail),
                Some(cause)
            );
            let captured = super::super::codemode_failure::capture(
                grapt_codemode::GuestFailure::GuestError,
                Some(cause),
                Some("PRIVATE"),
            );
            let public =
                super::super::codemode_failure::public(ToolFailureReason::EffectFailed, &captured)
                    .unwrap_or_else(|| panic!("recovery cause absent"));
            assert!(public.contains(phrase));
            assert!(public.contains("this request was not repeated"));
            assert!(!public.contains("PRIVATE"));
        }
    }
    #[test]
    fn declared_output_cannot_disappear_or_change_type() {
        let schema =
            r#"{"type":"object","required":["sum"],"properties":{"sum":{"type":"integer"}}}"#;
        assert!(validate_output(None, None).is_ok());
        assert!(validate_output(Some(schema), Some(&serde_json::json!({"sum":7}))).is_ok());
        assert!(validate_output(Some(schema), None).is_err());
        assert!(validate_output(Some(schema), Some(&serde_json::json!({"sum":"SECRET"}))).is_err());
        assert!(validate_output(Some(schema), Some(&serde_json::Value::Null)).is_err());
    }
}

//! Exact-principal external continuity before a provider request is admitted.
use super::context::SessionCapabilities;
use super::mcp::{LaunchIdentity, McpRegistry, PublishedTools};
use grapt_api::u3a::ContinuityAttentionInfo;
use grapt_inbox::Inbox;
use grapt_instruction::InstructionService;
use grapt_provider::{CodexInputItem, InputMessage, MessageRole};
use grapt_types::{AgentId, ContextWindowId, IdAuthority, Secret, SessionId};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod diagnostic;
pub(super) mod packet;
mod protocol;
mod reply;
mod required_reads;

pub(in crate::dogfood) const BOUND_RESUME_GUIDANCE: &str =
    "Resume this Session's bound StateEngine continuity with stateengine_resume({}). Identity is supplied by the Session; omit agent_id and project_id even when the schema lists them.";

const MAX_CONTEXT: usize = 32_768;
const MAX_REPLY: usize = 16_384;
const MAX_CALLS: usize = 8;
// Windows row-A resume measured 8.24 s and 12.8-16.5 s under load on 2026-10-18.
// Below the Step's 60 s no-progress watchdog, so a hung call degrades the turn
// as an outage rather than tripping the watchdog that refuses it.
const CONTINUITY_CALL_BUDGET: Duration = Duration::from_secs(50);
/// The bound on one Step's native inbox preparation, below the same watchdog.
pub(super) const INBOX_PREPARATION_BUDGET: Duration = Duration::from_secs(50);

#[derive(Debug)]
pub(super) struct Checkpoint {
    binding: String,
    agent: AgentId,
    window: Option<ContextWindowId>,
    launch: LaunchIdentity,
    principal: protocol::Principal,
    cursor: Secret<String>,
    context: Secret<String>,
    packet_prefix_bytes: usize,
    packet: Option<Arc<packet::Verified>>,
}

pub(super) use grapt_session::execution::ContinuityFailure as ContinuityUnavailable;

/// The committed binding marks a Session that cannot admit a provider call
/// before StateEngine continuity. Configuration alone grants no such role.
pub(super) fn is_required(
    inbox: &Inbox,
    session: &SessionId,
) -> Result<bool, ContinuityUnavailable> {
    let entries = InstructionService::entries(inbox.store(), session)
        .map_err(|_| ContinuityUnavailable::Binding)?;
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.key.as_str() == "stateengine-identity")
    else {
        return Ok(false);
    };
    let value: Value =
        serde_json::from_str(&entry.value).map_err(|_| ContinuityUnavailable::Binding)?;
    Ok(value.get("scope").and_then(Value::as_str) != Some("incognito"))
}

pub(super) struct ResumeContext {
    pub(super) inbox: Arc<Mutex<Inbox>>,
    pub(super) custody: Arc<Mutex<SessionCapabilities>>,
    pub(super) mcp: Arc<McpRegistry>,
    pub(super) published: Arc<PublishedTools>,
    pub(super) session: SessionId,
    pub(super) agent: AgentId,
    pub(super) location: String,
    pub(super) ids: Arc<dyn IdAuthority + Send + Sync>,
    input: Option<CodexInputItem>,
    prepared: bool,
    /// The outage continuity last degraded on, logged only when it changes.
    degraded: Option<ContinuityUnavailable>,
    /// This Session's location, read once to scope its continuity level.
    placement: Option<grapt_types::LocationRef>,
    preparation_progress:
        Option<tokio::sync::watch::Sender<grapt_runtime::step::ContextPreparationStage>>,
}

impl ResumeContext {
    pub(super) fn new(
        coordinates: (SessionId, AgentId, String),
        owners: (
            Arc<Mutex<Inbox>>,
            Arc<Mutex<SessionCapabilities>>,
            Arc<McpRegistry>,
        ),
        published: Arc<PublishedTools>,
        ids: Arc<dyn IdAuthority + Send + Sync>,
    ) -> Self {
        Self {
            session: coordinates.0,
            agent: coordinates.1,
            location: coordinates.2,
            inbox: owners.0,
            custody: owners.1,
            mcp: owners.2,
            published,
            ids,
            input: None,
            prepared: false,
            degraded: None,
            placement: None,
            preparation_progress: None,
        }
    }

    pub(super) fn with_preparation_progress(
        mut self,
        progress: tokio::sync::watch::Sender<grapt_runtime::step::ContextPreparationStage>,
    ) -> Self {
        self.preparation_progress = Some(progress);
        self
    }

    pub(super) fn take_input(&mut self) -> Result<Option<CodexInputItem>, ContinuityUnavailable> {
        if !std::mem::take(&mut self.prepared) {
            return Err(ContinuityUnavailable::Incomplete);
        }
        Ok(self.input.take())
    }

    pub(super) async fn refresh(&mut self) -> Result<(), ContinuityUnavailable> {
        self.prepared = false;
        self.refresh_inner().await?;
        if self.degraded.take().is_some() {
            crate::diagnostics::report("stateengine-continuity state=recovered".to_owned());
        }
        self.attend(ContinuityAttentionInfo::Ok);
        self.prepared = true;
        Ok(())
    }

    /// Whether this failure may proceed without continuity: an outage, and no
    /// undelivered launch packet. The caller refuses every other failure.
    /// Proceed without continuity after any failure: this step's input is a
    /// notice saying why, and no unverified or partial context is used.
    pub(super) fn degrade(&mut self, failure: ContinuityUnavailable) {
        if self.degraded.replace(failure) != Some(failure) {
            crate::diagnostics::report(format!(
                "stateengine-continuity state=degraded failure={failure:?}"
            ));
        }
        self.input = Some(degraded_item(failure));
        self.prepared = true;
        if let Some(level) = super::continuity_attention::degraded(failure) {
            self.attend(level);
        }
    }

    /// Publish this Session's continuity level where the service watches it.
    fn attend(&mut self, level: ContinuityAttentionInfo) {
        let Some(attention) = self
            .custody
            .lock()
            .ok()
            .and_then(|custody| custody.attention.clone())
        else {
            return;
        };
        if self.placement.is_none() {
            self.placement = self.inbox.lock().ok().and_then(|inbox| {
                grapt_session::SessionStore::over(inbox.store())
                    .record(&self.session)
                    .ok()
                    .map(|record| record.placement)
            });
        }
        if let Some(location) = &self.placement {
            attention.set(&self.session, location, level);
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn refresh_inner(&mut self) -> Result<(), ContinuityUnavailable> {
        use ContinuityUnavailable as E;
        self.input = None;
        let (binding, project, external_agent, window, packet_source) = {
            let inbox = self.inbox.lock().map_err(|_| E::Binding)?;
            let entries = InstructionService::entries(inbox.store(), &self.session)
                .map_err(|_| E::Binding)?;
            let Some(entry) = entries
                .iter()
                .find(|entry| entry.key.as_str() == "stateengine-identity")
            else {
                return Ok(());
            };
            let value: Value = serde_json::from_str(&entry.value).map_err(|_| E::Binding)?;
            if value.get("scope").and_then(Value::as_str) == Some("incognito") {
                return Ok(());
            }
            let project = bounded_string(&value, "project_id", 256)?.to_owned();
            let external_agent = bounded_string(&value, "agent_id", 256)?.to_owned();
            let window = grapt_store::compaction::latest_descriptor(
                inbox.store().connection(),
                &self.session,
            )
            .map_err(|_| E::Binding)?
            .map(|(_, _, descriptor)| descriptor.target_window().clone());
            let packet_source = entries
                .iter()
                .find(|entry| entry.key.as_str() == "stateengine-packet")
                .map(|entry| entry.value.clone());
            (
                entry.value.clone(),
                project,
                external_agent,
                window,
                packet_source,
            )
        };
        let pointer = packet_source
            .as_deref()
            .map(packet::Pointer::parse)
            .transpose()?;
        let route = self.published.routes.iter().find(|(_, route)| {
            route.server == "stateengine" && route.tool.as_str() == "stateengine_resume"
        });
        let (name, route) = match route {
            Some(route) => route,
            None => {
                if self
                    .published
                    .blocked_servers
                    .iter()
                    .any(|name| name == "stateengine")
                {
                    let reason =
                        grapt_session::execution::StateEngineServerFailure::ConfigurationBlocked;
                    diagnostic::report(
                        &self.session,
                        "stateengine_resume",
                        "configuration-blocked",
                        reason.reason(),
                        &[],
                    );
                    return Err(E::ServerUnavailable(reason));
                }
                if let Some((reason, detail)) = self
                    .mcp
                    .server_unavailable(&self.location, "stateengine")
                    .await
                {
                    diagnostic::report(
                        &self.session,
                        "stateengine_resume",
                        "server-unavailable",
                        &detail,
                        &[],
                    );
                    return Err(E::ServerUnavailable(reason));
                }
                return Err(E::Contract);
            }
        };
        let definition = self
            .published
            .definitions
            .iter()
            .find(|definition| &definition.name == name)
            .ok_or(E::Contract)?;
        let schema: Value =
            serde_json::from_str(&definition.input_schema).map_err(|_| E::Contract)?;
        if !protocol::accepts_since(&schema)
            || schema
                .pointer("/properties/byte_budget/type")
                .and_then(Value::as_str)
                != Some("integer")
        {
            return Err(E::Contract);
        }
        let (capability, prior, packet) = {
            let mut held = self.custody.lock().map_err(|_| E::Capability)?;
            let capability = held
                .values
                .get(&self.session)
                .cloned()
                .ok_or(E::Capability)?;
            let prior = held.resumes.get(&self.session).cloned().filter(|old| {
                old.binding == binding
                    && old.agent == self.agent
                    && old.launch.matches_approval(&route.launch)
                    && old.packet.as_ref().map(|packet| &packet.pointer) == pointer.as_ref()
            });
            let packet = pointer
                .as_ref()
                .map(|pointer| held.packets.load(&self.session, &binding, pointer))
                .transpose()?;
            (capability, prior, packet)
        };
        if packet.as_ref().and_then(|packet| packet.id()).is_some()
            && schema
                .pointer("/properties/packet_id/type")
                .and_then(Value::as_str)
                != Some("string")
        {
            return Err(E::Contract);
        }
        let boot_fingerprint = packet
            .as_ref()
            .map(|packet| packet.identity_fingerprint(&project, &external_agent))
            .transpose()?
            .flatten();
        if boot_fingerprint.is_some_and(|expected| {
            prior
                .as_ref()
                .is_some_and(|prior| !prior.principal.matches_fingerprint(expected))
        }) {
            return Err(E::Principal);
        }
        if let Some(prior) = &prior {
            // A new connection keeps the verified principal and packet
            // redactions, but must refresh continuity before reusing context.
            if prior.window == window && prior.launch == route.launch {
                self.input = Some(context_item(prior.context.expose().clone()));
                return Ok(());
            }
        }
        let mut arguments = json!({"byte_budget": MAX_REPLY});
        if let Some(prior) = &prior {
            arguments["since"] = json!(prior.cursor.expose());
        } else if let Some(id) = packet.as_ref().and_then(|packet| packet.id()) {
            arguments["packet_id"] = json!(id);
        }
        let mut cursor: Option<Secret<String>>;
        let mut private_cursors: Vec<Secret<String>> = prior
            .iter()
            .map(|old| Secret::new(old.cursor.expose().clone()))
            .collect();
        let mut principal = prior
            .as_ref()
            .map(|prior| prior.principal.clone())
            .or_else(|| boot_fingerprint.map(protocol::Principal::from_fingerprint));
        let mut context = String::new();
        let mut calls = 0;
        let mut resumed_cold = false;
        loop {
            let requested_packet = arguments.get("packet_id").is_some();
            if requested_packet
                && arguments.get("packet_id").and_then(Value::as_str)
                    != packet.as_ref().and_then(|packet| packet.id())
            {
                return Err(E::PacketAcknowledgement);
            }
            let sent_cursor = arguments.get("since").is_some();
            let outcome = self
                .call_outcome("stateengine_resume", arguments, &capability, &mut calls)
                .await?;
            // A restarted server may no longer hold this cursor. Take the
            // recovery it names once, before any context has been accepted.
            if sent_cursor
                && !resumed_cold
                && context.is_empty()
                && reply::names_cold_resume(&outcome)
            {
                self.completed_preparation_call();
                resumed_cold = true;
                arguments = json!({"byte_budget": MAX_REPLY});
                continue;
            }
            let reply = reply::decode(&outcome)?;
            let current = protocol::Principal::read(&reply, &project, &external_agent)?;
            if principal.as_ref().is_some_and(|prior| prior != &current) {
                return Err(E::Principal);
            }
            principal = Some(current);
            self.completed_preparation_call();
            if requested_packet {
                packet
                    .as_ref()
                    .ok_or(E::PacketAcknowledgement)?
                    .require_acknowledgement(&reply, &project, &external_agent, &self.session)?;
            }
            cursor = Some(Secret::new(
                bounded_string(
                    &reply,
                    "cursor",
                    if principal.as_ref().is_some_and(protocol::Principal::is_rust) {
                        4096
                    } else {
                        1024
                    },
                )?
                .to_owned(),
            ));
            if let Some(current) = &cursor {
                private_cursors.push(Secret::new(current.expose().clone()));
            }
            if principal.as_ref().is_some_and(protocol::Principal::is_rust) {
                protocol::admit_rust(&reply)?;
                context.push_str(protocol::READ_GUIDANCE);
                // Rust recovery commands carry opaque read positions. Keep
                // those usable while still redacting the Session credential.
                append_context(&mut context, reply, capability.expose(), &[])?;
                break;
            }
            let status = reply
                .get("resume_status")
                .and_then(Value::as_str)
                .ok_or(E::Incomplete)?;
            let reads = reply_reads(&reply)?;
            if status == "ready" {
                ready_admitted(&reply, reads)?;
                append_context(
                    &mut context,
                    reply.clone(),
                    capability.expose(),
                    &private_cursors,
                )?;
                break;
            }
            needs_read_admitted(status, reads)?;
            let Some(reads) = reads else {
                return Err(E::Incomplete);
            };
            let mut continuation = None;
            for read in reads {
                let tool = bounded_string(read, "tool", 128)?;
                let args = read
                    .get("arguments")
                    .filter(|v| v.is_object())
                    .ok_or(E::Contract)?
                    .clone();
                if tool == "stateengine_resume" {
                    if args.get("since").and_then(Value::as_str)
                        != cursor.as_ref().map(|v| v.expose().as_str())
                    {
                        return Err(E::Principal);
                    }
                    continuation = Some(args);
                } else if tool == "stateengine_msg_list" {
                    required_reads::inbox_arguments(&args)?;
                    let read_reply = self.call(tool, args, &capability, &mut calls).await?;
                    append_context(
                        &mut context,
                        read_reply,
                        capability.expose(),
                        &private_cursors,
                    )?;
                } else {
                    return Err(E::Contract);
                }
            }
            arguments = continuation.unwrap_or_else(
                || json!({"since":cursor.as_ref().map(|v| v.expose()), "byte_budget":MAX_REPLY}),
            );
        }
        // Legacy packet context waits for the handshake's cursor redactions.
        // Identity-only boots bind that handshake; context comes from resume.
        let mut packet_prefix_bytes = 0;
        if let Some(packet) = packet.as_ref().filter(|packet| packet.id().is_some()) {
            // Keep earlier redactions across windows: raw verified bytes may
            // still contain a private cursor from an older handshake.
            let packet_context = if let Some(prior) = &prior {
                let prefix = prior
                    .context
                    .expose()
                    .get(..prior.packet_prefix_bytes)
                    .ok_or(E::Incomplete)?;
                serde_json::from_str(prefix).map_err(|_| E::Incomplete)?
            } else {
                json!({"launch_packet": packet.text()})
            };
            let mut prefix = String::new();
            append_context(
                &mut prefix,
                packet_context,
                capability.expose(),
                &private_cursors,
            )?;
            if prefix.len() + context.len() > MAX_CONTEXT {
                return Err(E::Bounds);
            }
            packet_prefix_bytes = prefix.len();
            prefix.push_str(&context);
            context = prefix;
        }
        let checkpoint = Checkpoint {
            binding: binding.clone(),
            agent: self.agent.clone(),
            window,
            launch: route.launch.clone(),
            principal: principal.ok_or(E::Principal)?,
            cursor: cursor.ok_or(E::Incomplete)?,
            context: Secret::new(context.clone()),
            packet_prefix_bytes,
            packet,
        };
        // Rebind during I/O invalidates this answer. Re-read the durable tuple,
        // then compare the process capability before publishing any context.
        let inbox = self.inbox.lock().map_err(|_| E::Binding)?;
        let entries =
            InstructionService::entries(inbox.store(), &self.session).map_err(|_| E::Binding)?;
        if !entries
            .iter()
            .any(|entry| entry.key.as_str() == "stateengine-identity" && entry.value == binding)
        {
            return Err(E::Rebound);
        }
        let current_packet = entries
            .iter()
            .find(|entry| entry.key.as_str() == "stateengine-packet")
            .map(|entry| entry.value.as_str());
        if current_packet != packet_source.as_deref() {
            return Err(E::Rebound);
        }
        let mut held = self.custody.lock().map_err(|_| E::Capability)?;
        if !held
            .values
            .get(&self.session)
            .is_some_and(|current| Arc::ptr_eq(current, &capability))
        {
            return Err(E::Rebound);
        }
        let retained_bytes: usize = held
            .resumes
            .iter()
            .filter(|(session, _)| *session != &self.session)
            .map(|(_, value)| value.context.expose().len())
            .sum();
        if retained_bytes + context.len() > 1_048_576 {
            return Err(E::Bounds);
        }
        held.resumes
            .insert(self.session.clone(), Arc::new(checkpoint));
        self.input = Some(context_item(context));
        Ok(())
    }

    async fn call(
        &self,
        tool: &str,
        args: Value,
        capability: &Secret<String>,
        calls: &mut usize,
    ) -> Result<Value, ContinuityUnavailable> {
        let reply = reply::decode(&self.call_outcome(tool, args, capability, calls).await?)?;
        self.completed_preparation_call();
        Ok(reply)
    }

    fn completed_preparation_call(&self) {
        if let Some(progress) = &self.preparation_progress {
            progress.send_replace(grapt_runtime::step::ContextPreparationStage::Continuity);
        }
    }

    async fn call_outcome(
        &self,
        tool: &str,
        args: Value,
        capability: &Secret<String>,
        calls: &mut usize,
    ) -> Result<grapt_mcp::ToolCallOutcome, ContinuityUnavailable> {
        use ContinuityUnavailable as E;
        if *calls >= MAX_CALLS {
            return Err(E::Bounds);
        }
        *calls += 1;
        let route = self
            .published
            .routes
            .values()
            .find(|route| route.server == "stateengine" && route.tool.as_str() == tool)
            .ok_or(E::Contract)?;
        let call = format!("call_resume_{}", self.ids.mint().as_str());
        let mut private: Vec<Secret<String>> = args
            .as_object()
            .into_iter()
            .flat_map(|arguments| arguments.values())
            .filter_map(Value::as_str)
            .map(|value| Secret::new(value.to_owned()))
            .collect();
        private.push(Secret::new(capability.expose().clone()));
        let outcome = self
            .mcp
            .call_with(
                &self.location,
                &route.server,
                &route.tool,
                &route.launch,
                &call,
                args,
                CONTINUITY_CALL_BUDGET,
                None,
                None,
                Some(json!({"stateengine.session-capability-v1":capability.expose()})),
            )
            .await
            .map_err(|error| {
                diagnostic::report_call(&self.session, tool, &error, &private);
                match error {
                    super::mcp::McpCallRefusal::TimedOut => E::TimedOut {
                        seconds: CONTINUITY_CALL_BUDGET.as_secs(),
                    },
                    // The native host refuses the capability before the tool.
                    super::mcp::McpCallRefusal::Protocol(detail)
                        if reply::refused_call(&detail) =>
                    {
                        E::Capability
                    }
                    _ => E::Transport,
                }
            })?;
        if outcome.is_error() {
            diagnostic::report(
                &self.session,
                tool,
                "server-refusal",
                outcome.text(),
                &private,
            );
        }
        Ok(outcome)
    }
}

fn bounded_string<'a>(
    value: &'a Value,
    key: &str,
    bound: usize,
) -> Result<&'a str, ContinuityUnavailable> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= bound)
        .ok_or(ContinuityUnavailable::Contract)
}
fn verify(
    value: &Value,
    project: &str,
    agent: &str,
    fingerprint: Option<&str>,
) -> Result<(), ContinuityUnavailable> {
    use ContinuityUnavailable as E;
    if value.get("schema_version").and_then(Value::as_u64) != Some(2) {
        return Err(E::Contract);
    }
    let principal = value.get("principal").ok_or(E::Principal)?;
    if principal.get("verified") != Some(&Value::Bool(true))
        || principal.get("project_id").and_then(Value::as_str) != Some(project)
        || principal.get("agent_id").and_then(Value::as_str) != Some(agent)
    {
        return Err(E::Principal);
    }
    let actual = bounded_string(principal, "identity_fingerprint", 128)?;
    if fingerprint.is_some_and(|expected| expected != actual) {
        return Err(E::Principal);
    }
    Ok(())
}
/// Schema v2 omits `required_reads` on a ready reply. An absent key is the
/// no-reads shape and the only tolerated absence; a present non-array value
/// is malformed protocol data and stays an Incomplete refusal, never a valid
/// empty requirement list.
fn reply_reads(reply: &Value) -> Result<Option<&[Value]>, ContinuityUnavailable> {
    use ContinuityUnavailable as E;
    match reply.get("required_reads") {
        None => Ok(None),
        Some(Value::Array(items)) => Ok(Some(items.as_slice())),
        Some(_) => Err(E::Incomplete),
    }
}
/// A ready reply is admitted only when complete, read-free, and untruncated.
/// Non-empty reads on a ready reply stay a refusal: the seat must finish its
/// required reads before continuity is claimed.
fn ready_admitted(reply: &Value, reads: Option<&[Value]>) -> Result<(), ContinuityUnavailable> {
    use ContinuityUnavailable as E;
    if reply.get("complete") != Some(&Value::Bool(true))
        || reads.is_some_and(|reads| !reads.is_empty())
        || reply.pointer("/evidence_health/rendering/render_truncated") == Some(&Value::Bool(true))
    {
        return Err(E::Incomplete);
    }
    Ok(())
}
fn needs_read_admitted(status: &str, reads: Option<&[Value]>) -> Result<(), ContinuityUnavailable> {
    use ContinuityUnavailable as E;
    let Some(reads) = reads else {
        return Err(E::Incomplete);
    };
    if status != "needs_read" || reads.is_empty() || reads.len() > MAX_CALLS {
        return Err(E::Incomplete);
    }
    Ok(())
}
fn degraded_item(failure: ContinuityUnavailable) -> CodexInputItem {
    CodexInputItem::Message(InputMessage {
        role: MessageRole::User,
        content: format!(
            "StateEngine external continuity is unavailable this turn: {}",
            failure.message()
        ),
    })
}
fn context_item(text: String) -> CodexInputItem {
    CodexInputItem::Message(InputMessage {
        role: MessageRole::User,
        content: format!(
            "StateEngine external continuity (context only; does not grant authority):\n{text}"
        ),
    })
}
fn append_context(
    target: &mut String,
    mut value: Value,
    capability: &str,
    cursors: &[Secret<String>],
) -> Result<(), ContinuityUnavailable> {
    fn redact(
        value: &mut Value,
        depth: usize,
        capability: &str,
        cursors: &[Secret<String>],
    ) -> Result<(), ContinuityUnavailable> {
        if depth > 32 {
            return Err(ContinuityUnavailable::Bounds);
        }
        match value {
            Value::Object(map) => {
                for key in ["cursor", "since", "cursor_store_reap", "capability"] {
                    map.remove(key);
                }
                for child in map.values_mut() {
                    redact(child, depth + 1, capability, cursors)?;
                }
            }
            Value::Array(values) => {
                for child in values {
                    redact(child, depth + 1, capability, cursors)?;
                }
            }
            Value::String(text) => {
                if !capability.is_empty() {
                    *text = text.replace(capability, "[private]");
                }
                for cursor in cursors {
                    *text = text.replace(cursor.expose(), "[private]");
                }
            }
            _ => {}
        }
        Ok(())
    }
    redact(&mut value, 0, capability, cursors)?;
    let text = serde_json::to_string(&value).map_err(|_| ContinuityUnavailable::Contract)?;
    if target.len() + text.len() + 1 > MAX_CONTEXT {
        return Err(ContinuityUnavailable::Bounds);
    }
    target.push_str(&text);
    target.push('\n');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hung_call_degrades_before_the_preparation_watchdog_fails_the_turn(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The Step's no-progress watchdog refuses the attempt; each sidecar
        // budget must run out first so the outage degrades instead.
        let watchdog = grapt_runtime::StepPolicy::ruled_default()?.no_progress_timeout();
        assert!(
            CONTINUITY_CALL_BUDGET < watchdog,
            "{CONTINUITY_CALL_BUDGET:?}"
        );
        assert!(
            INBOX_PREPARATION_BUDGET < watchdog,
            "{INBOX_PREPARATION_BUDGET:?}"
        );
        Ok(())
    }

    fn envelope() -> Value {
        json!({"schema_version":2,"principal":{"project_id":"fixture","agent_id":"agt_fixture",
            "verified":true,"identity_fingerprint":"fingerprint-first"},"cursor":"SCC_CURSOR"})
    }

    #[test]
    fn scc_resume_verifies_exact_principal_and_version_before_context() {
        let good = envelope();
        assert_eq!(verify(&good, "fixture", "agt_fixture", None), Ok(()));
        for pointer in [
            "/principal/project_id",
            "/principal/agent_id",
            "/principal/identity_fingerprint",
        ] {
            let mut bad = good.clone();
            let field = bad
                .pointer_mut(pointer)
                .unwrap_or_else(|| panic!("fixture field absent"));
            *field = json!("foreign");
            assert_eq!(
                verify(&bad, "fixture", "agt_fixture", Some("fingerprint-first")),
                Err(ContinuityUnavailable::Principal)
            );
        }
        let mut bad = good;
        bad["schema_version"] = json!(3);
        assert_eq!(
            verify(&bad, "fixture", "agt_fixture", None),
            Err(ContinuityUnavailable::Contract)
        );
        bad["schema_version"] = json!(2);
        bad["principal"]["verified"] = Value::Null;
        assert_eq!(
            verify(&bad, "fixture", "agt_fixture", None),
            Err(ContinuityUnavailable::Principal)
        );
    }

    #[test]
    fn scc_resume_context_excludes_private_handles_and_remains_bounded() -> Result<(), String> {
        let mut target = String::new();
        append_context(&mut target, json!({"cursor":"SCC_CURSOR", "nested":{"since":"SCC_CURSOR"},
            "context":"task SCC_CAPABILITY SCC_CURSOR", "cursor_store_reap":{"records":["private"]}}),
            "SCC_CAPABILITY", &[Secret::new("SCC_CURSOR".to_owned())]).map_err(|error| error.to_string())?;
        assert!(!target.contains("SCC_CURSOR"));
        assert!(!target.contains("SCC_CAPABILITY"));
        assert!(!target.contains("cursor_store_reap"));
        assert!(target.contains("task"));
        assert_eq!(
            append_context(
                &mut target,
                json!({"text":"x".repeat(MAX_CONTEXT)}),
                "secret",
                &[]
            ),
            Err(ContinuityUnavailable::Bounds)
        );
        let CodexInputItem::Message(item) = context_item(target) else {
            panic!("context message absent")
        };
        assert_eq!(item.role, MessageRole::User);
        Ok(())
    }

    #[test]
    fn scc_resume_schema_v2_ready_reply_without_required_reads_is_admitted() -> Result<(), String> {
        let reply: Value = serde_json::from_str(include_str!(
            "stateengine_resume/fixtures/resume-reply-v2-redacted-20261103.json"
        ))
        .map_err(|error| format!("recorded reply fixture is malformed: {error}"))?;
        assert_eq!(verify(&reply, "fixture", "agt_fixture", None), Ok(()));
        assert_eq!(
            reply.get("resume_status").and_then(Value::as_str),
            Some("ready")
        );
        let reads = reply_reads(&reply).map_err(|error| error.to_string())?;
        assert!(reads.is_none());
        ready_admitted(&reply, reads).map_err(|error| error.to_string())?;
        let mut context = String::new();
        append_context(
            &mut context,
            reply.clone(),
            "SCC_CAPABILITY",
            &[Secret::new("FIXTURE_CURSOR".to_owned())],
        )
        .map_err(|error| error.to_string())?;
        assert!(!context.contains("FIXTURE_CURSOR"));
        Ok(())
    }

    #[test]
    fn scc_resume_ready_reply_keeps_legacy_shape_and_stays_strict() {
        let absent = json!({"complete": true});
        assert_eq!(ready_admitted(&absent, None), Ok(()));
        let legacy = json!({"complete": true, "required_reads": []});
        let empty: &[Value] = &[];
        assert_eq!(reply_reads(&legacy), Ok(Some(empty)));
        assert_eq!(ready_admitted(&legacy, Some(empty)), Ok(()));
        for bad in [
            json!({"complete": true, "required_reads": [
                {"tool": "stateengine_msg_list", "arguments": {"unread_only": true}}
            ]}),
            json!({"complete": false, "required_reads": []}),
            json!({"complete": true, "required_reads": [],
                "evidence_health": {"rendering": {"render_truncated": true}}}),
        ] {
            assert_eq!(
                ready_admitted(&bad, reply_reads(&bad).ok().flatten()),
                Err(ContinuityUnavailable::Incomplete)
            );
        }
    }

    #[test]
    fn scc_resume_present_malformed_required_reads_stays_a_refusal() {
        for malformed in [
            json!({"complete": true, "required_reads": null}),
            json!({"complete": true, "required_reads": "reads"}),
            json!({"complete": true, "required_reads": 3}),
            json!({"complete": true, "required_reads": true}),
            json!({"complete": true, "required_reads": {"tool": "stateengine_msg_list"}}),
        ] {
            assert_eq!(
                reply_reads(&malformed),
                Err(ContinuityUnavailable::Incomplete)
            );
        }
    }

    #[test]
    fn scc_resume_needs_read_refuses_without_usable_reads() {
        assert_eq!(reply_reads(&json!({"complete": false})), Ok(None));
        assert_eq!(
            needs_read_admitted("needs_read", None),
            Err(ContinuityUnavailable::Incomplete)
        );
        let empty: &[Value] = &[];
        assert_eq!(
            needs_read_admitted("needs_read", Some(empty)),
            Err(ContinuityUnavailable::Incomplete)
        );
        let one_read =
            json!({"required_reads": [{"tool": "stateengine_msg_list", "arguments": {}}]});
        let reads = reply_reads(&one_read).ok().flatten();
        assert_eq!(needs_read_admitted("needs_read", reads), Ok(()));
        assert_eq!(
            needs_read_admitted("ready", reads),
            Err(ContinuityUnavailable::Incomplete)
        );
        assert_eq!(
            needs_read_admitted("paused", reads),
            Err(ContinuityUnavailable::Incomplete)
        );
        let many_reads = json!({"required_reads": (0..MAX_CALLS + 1)
            .map(|index| json!({"tool": "stateengine_msg_list", "arguments": {"limit": index + 1}}))
            .collect::<Vec<Value>>()});
        let too_many = reply_reads(&many_reads).ok().flatten();
        assert_eq!(
            needs_read_admitted("needs_read", too_many),
            Err(ContinuityUnavailable::Incomplete)
        );
    }
}

#[cfg(test)]
mod transport_tests;

#[cfg(test)]
mod packet_transport_tests;

#[cfg(test)]
mod reconnect_tests;

#[cfg(test)]
mod capability_refusal_tests;

//! Configured hooks share the existing command owner and durable notices.
//! V1 bindings receive metadata; v2 tool bindings additionally receive tool input.
//! Prompt, transcript, provider credentials and inherited seat identity stay absent.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use grapt_inbox::Inbox;
use grapt_session::notice::LifecycleHookScope;
use grapt_store::{OsRandomIds, SystemClock};
use grapt_types::{DeterministicAuthorities, SessionId, ToolCallId};

mod command;
pub(in crate::dogfood) use command::ConfiguredCommand;
mod config;
mod decision;
pub(in crate::dogfood) mod helper;
mod matcher;
pub(in crate::dogfood) use matcher::aliases as claude_tool_aliases;
pub(in crate::dogfood) mod plugin;
pub(in crate::dogfood) mod session_end;
mod waiting;
pub(in crate::dogfood) use decision::{HookError, HookResult, PermissionDecision};
pub(in crate::dogfood) mod plan;
use super::tools::shell_environment::Environment;
pub(in crate::dogfood) use config::Event;
pub(in crate::dogfood) use config::HookSettings;

const MAX_INPUT_BYTES: usize = 16 * 1024;
const EVENT_TIMEOUT: Duration = Duration::from_secs(60);
// Both terminal events must finish inside the runtime's 60-second shutdown
// allowance, leaving time for existing context-resource cleanup.
const TERMINAL_EVENT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub(in crate::dogfood) struct NoticeContext {
    pub inbox: Arc<Mutex<Inbox>>,
    pub ids: Arc<OsRandomIds>,
    pub clock: Arc<SystemClock>,
    pub scope: LifecycleHookScope,
}

#[derive(Debug, Clone)]
pub(in crate::dogfood) struct HookRuntime {
    settings: HookSettings,
    environment: Option<Environment>,
    workspace: PathBuf,
    notices: Option<NoticeContext>,
    process_owner: Option<Arc<super::tools::TerminalRegistry>>,
}
impl HookRuntime {
    pub(in crate::dogfood) fn load_ephemeral(
        workspace: &Path,
        service: &Path,
    ) -> Result<Self, String> {
        let settings = HookSettings::load(workspace, service)?;
        if Event::ALL
            .iter()
            .any(|event| !settings.commands(*event).is_empty())
        {
            return Err("ephemeral runs do not support configured lifecycle hooks".into());
        }
        Ok(Self {
            settings,
            environment: None,
            workspace: workspace.into(),
            notices: None,
            process_owner: None,
        })
    }

    pub(in crate::dogfood) fn load(
        workspace: &Path,
        service: &Path,
        credentials: &[String],
    ) -> Result<Self, String> {
        let settings = HookSettings::load(workspace, service)?;
        let environment = if Event::ALL
            .iter()
            .any(|event| !settings.commands(*event).is_empty())
        {
            Some(Environment::for_hooks(service, workspace, credentials)?)
        } else {
            None
        };
        Ok(Self {
            settings,
            environment,
            workspace: workspace.into(),
            notices: None,
            process_owner: None,
        })
    }
    pub(in crate::dogfood) fn with_notices(mut self, notices: NoticeContext) -> Self {
        self.notices = Some(notices);
        self
    }
    pub(in crate::dogfood) fn with_process_owner(
        mut self,
        terminals: Arc<super::tools::TerminalRegistry>,
    ) -> Self {
        self.process_owner = Some(terminals);
        self
    }
    pub(in crate::dogfood) async fn fire(
        &self,
        event: Event,
        session: &SessionId,
        tool: Option<(&str, &ToolCallId)>,
    ) -> Result<Vec<String>, String> {
        Ok(self.dispatch(event, session, tool, None, None).await?.notes)
    }

    pub(in crate::dogfood) async fn fire_tool(
        &self,
        event: Event,
        session: &SessionId,
        tool: (&str, &ToolCallId),
        input: &serde_json::Value,
        alias: Option<&str>,
    ) -> Result<HookResult, String> {
        self.dispatch(event, session, Some(tool), Some(input), alias)
            .await
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn dispatch(
        &self,
        event: Event,
        session: &SessionId,
        tool: Option<(&str, &ToolCallId)>,
        payload: Option<&serde_json::Value>,
        alias: Option<&str>,
    ) -> Result<HookResult, String> {
        let commands = self.settings.commands(event);
        let name = tool.map(|(name, _)| name);
        if !commands.iter().any(|hook| hook.matches(name, alias)) {
            return Ok(HookResult::default());
        }
        let environment = self
            .environment
            .as_ref()
            .ok_or("configured hook environment is absent")?;
        let deadline = if matches!(
            event,
            Event::Stop | Event::Notification | Event::SubagentStop | Event::SessionEnd
        ) {
            TERMINAL_EVENT_TIMEOUT
        } else {
            EVENT_TIMEOUT
        };
        // A failed hook stops the turn only when the user marked it required.
        // Tool-call events keep their own per-call outcome: their failure
        // refuses or annotates that one call and never fails the turn.
        let skippable = |hook: &config::HookCommand| !hook.required && !per_call(event);
        let mut warnings = Vec::new();
        // How many matching hooks ran to completion before any event timeout.
        let finished = std::sync::atomic::AtomicUsize::new(0);
        // Hooks that complete before an event timeout keep their result.
        let mut completed = HookResult::default();
        let outcome = tokio::time::timeout(deadline, async {
            let result = &mut completed;
            let mut bytes = 0usize;
            for hook in commands.iter().filter(|hook| hook.matches(name, alias)) {
                // Legacy bindings retain their metadata-only input contract.
                let input = metadata(
                    event,
                    session,
                    &self.workspace,
                    tool,
                    hook.matcher.as_ref().and(result.input.as_ref().or(payload)),
                );
                let output = match (input, &self.process_owner) {
                    (Err(reason), _) => Err(HookError::Failed(reason)),
                    (Ok(input), Some(terminals)) => {
                        let hook = hook.clone();
                        let environment = environment.clone();
                        terminals
                            .run_helper_owned(session.clone(), move |control| async move {
                                Ok(command::run_controlled(
                                    &hook,
                                    &environment,
                                    &input,
                                    Some(&control),
                                )
                                .await)
                            })
                            .await
                            .unwrap_or_else(|reason| Err(HookError::Failed(reason)))
                    }
                    (Ok(input), None) => command::run(hook, environment, &input).await,
                }
                .and_then(|output| conforms(event, hook, output).map_err(HookError::Failed));
                finished.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let output = match output {
                    Ok(output) => output,
                    Err(HookError::Failed(reason)) if skippable(hook) => {
                        warnings.push(format!(
                            "{} hook {} failed and was skipped: {reason}",
                            event.as_str(),
                            hook.name.as_str()
                        ));
                        continue;
                    }
                    Err(error) => {
                        return Err(format!(
                            "{} hook {}: {error}",
                            event.as_str(),
                            hook.name.as_str()
                        ))
                    }
                };
                if matches!(event, Event::PreToolUse | Event::PermissionRequest)
                    && hook.matcher.is_some()
                {
                    // Approval is bound to the arguments the approving hook saw.
                    // A later replacement cannot carry an earlier allow to a new call.
                    if output.input.is_some() && result.permission == PermissionDecision::Allow {
                        result.permission = PermissionDecision::Default;
                    }
                    if output.permission == PermissionDecision::Ask || hook.permits_allow {
                        result.permission = result.permission.max(output.permission);
                    }
                    if output.input.is_some() {
                        result.input = output.input;
                    }
                }
                for note in output.notes {
                    bytes = bytes.saturating_add(note.len());
                    if bytes > grapt_session::notice::MAX_HOOK_RESULT_BYTES / 2 {
                        return Err("combined hook annotations exceed 8192 bytes".into());
                    }
                    result.notes.push(note);
                }
            }
            Ok::<_, String>(())
        })
        .await;
        let result = match outcome {
            Ok(ran) => ran.map(|()| completed),
            Err(_) => {
                let reason = format!(
                    "{} hook event exceeded {} ms",
                    event.as_str(),
                    deadline.as_millis()
                );
                // Only a hook that never completed can be the cause.
                let done = finished.load(std::sync::atomic::Ordering::Relaxed);
                if commands
                    .iter()
                    .filter(|hook| hook.matches(name, alias))
                    .skip(done)
                    .any(|hook| !skippable(hook))
                {
                    Err(reason)
                } else {
                    warnings.push(format!("{reason}; its unfinished hooks were skipped"));
                    Ok(completed)
                }
            }
        };
        let text = match &result {
            Ok(output) => serde_json::to_string(&serde_json::json!({
                "decision":format!("{:?}", output.permission),
                "modifiedInput":output.input.is_some(), "notes":output.notes,
            }))
            .map_err(|_| "hook result encoding failed")?,
            Err(reason) => reason.clone(),
        };
        self.record(session, event, &bounded_notice(&text))?;
        for warning in &warnings {
            self.record(
                session,
                event,
                &bounded_notice(&format!("warning: {warning}")),
            )?;
        }
        result
    }

    pub(in crate::dogfood) fn rejected_input(
        &self,
        session: &SessionId,
        reason: &str,
    ) -> Result<(), String> {
        self.record(
            session,
            Event::PreToolUse,
            &bounded_notice(&format!("hook modified input rejected: {reason}")),
        )
    }
    fn record(&self, session: &SessionId, event: Event, text: &str) -> Result<(), String> {
        let Some(context) = &self.notices else {
            return Ok(());
        };
        {
            let mut inbox = match context.inbox.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            let authorities = DeterministicAuthorities::new(&*context.clock, &*context.ids);
            let scope = if event == Event::SessionEnd {
                &LifecycleHookScope::Session
            } else {
                &context.scope
            };
            match scope {
                LifecycleHookScope::Session => {
                    grapt_session::notice::record_session_lifecycle_hook(
                        inbox.store_mut(),
                        &authorities,
                        session,
                        event,
                        text,
                    )?;
                }
                LifecycleHookScope::Execution(execution) => {
                    grapt_session::notice::record_lifecycle_hook(
                        inbox.store_mut(),
                        &authorities,
                        session,
                        execution,
                        event,
                        text,
                    )?;
                }
                LifecycleHookScope::Input(input) => {
                    grapt_session::notice::record_input_lifecycle_hook(
                        inbox.store_mut(),
                        &authorities,
                        session,
                        input,
                        event,
                        text,
                    )?;
                }
            }
        }
        // Normal tool/permission lifecycle activity hydrates these durable receipts.
        // Ending the advisory stream here would drop its approver before authorization.
        Ok(())
    }
}

/// Events whose outcome belongs to one tool call: a failure refuses that call
/// before its effect, or annotates it after, and the turn goes on.
fn per_call(event: Event) -> bool {
    matches!(
        event,
        Event::PreToolUse | Event::PermissionRequest | Event::PostToolUse
    )
}

/// A hook's decision must fit its event and its owner's authority; one that
/// does not is a failure of that hook, never a decision to apply.
fn conforms(
    event: Event,
    hook: &config::HookCommand,
    output: HookResult,
) -> Result<HookResult, String> {
    if hook.matcher.is_some()
        && output
            .event
            .as_deref()
            .is_some_and(|name| name != event.as_str())
    {
        return Err("hook output names a different lifecycle event".into());
    }
    if event == Event::PermissionRequest
        && (output.input.is_some()
            || (output.permission != PermissionDecision::Default
                && output.event.as_deref() != Some("PermissionRequest")))
    {
        return Err("PermissionRequest requires decision.behavior and cannot replace input".into());
    }
    if matches!(event, Event::PreToolUse | Event::PermissionRequest)
        && hook.matcher.is_some()
        && output.input.is_some()
        && !hook.permits_allow
    {
        return Err("project hook cannot replace tool input after user enforcement".into());
    }
    Ok(output)
}

fn bounded_notice(text: &str) -> String {
    const SUFFIX: &str = " [hook detail truncated]";
    let limit = grapt_session::notice::MAX_HOOK_RESULT_BYTES;
    let text = text.replace('\0', "\u{fffd}");
    if text.len() <= limit {
        return text;
    }
    let mut end = limit - SUFFIX.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{SUFFIX}", &text[..end])
}

fn metadata(
    event: Event,
    session: &SessionId,
    workspace: &Path,
    tool: Option<(&str, &ToolCallId)>,
    payload: Option<&serde_json::Value>,
) -> Result<Vec<u8>, String> {
    let mut value = serde_json::json!({"hook_event_name":event.as_str(), "session_id":session.as_str(), "cwd":workspace});
    if let Some((name, call)) = tool {
        value["tool_name"] = name.into();
        value["tool_use_id"] = call.as_str().into();
    }
    if let Some(payload) = payload {
        value["tool_input"] = payload.clone();
    }
    let mut bytes = serde_json::to_vec(&value).map_err(|_| "hook input encoding failed")?;
    bytes.push(b'\n');
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("hook metadata exceeds 16384 bytes".into());
    }
    Ok(bytes)
}

pub(super) fn notice_row(
    notice: grapt_session::notice::LifecycleHookNotice,
) -> Result<(u64, String, String), String> {
    let suffix = notice
        .position
        .event
        .as_str()
        .strip_prefix("evt_")
        .ok_or("hook notice id has no prefix")?;
    let id = format!("msg_{suffix}");
    let mut metadata = serde_json::json!({"lifecycleHook":true,"event":notice.event.as_str()});
    match &notice.scope {
        LifecycleHookScope::Session => metadata["sessionOnly"] = true.into(),
        LifecycleHookScope::Execution(id) => metadata["executionID"] = id.as_str().into(),
        LifecycleHookScope::Input(id) => metadata["messageID"] = id.as_str().into(),
    }
    let dto: grapt_api::u3a::SessionMessageSystemDto = super::decode(serde_json::json!({
        "type":"system", "id":id, "time":{"created":notice.position.created_ms},
        "metadata": metadata,
        "text":format!("{} hook: {}", notice.event.as_str(), notice.text),
    }))?;
    let raw = serde_json::to_string(&dto).map_err(|e| e.to_string())?;
    Ok((notice.position.seq, id, raw))
}

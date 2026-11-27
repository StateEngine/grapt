use super::super::hooks::Event;
use super::*;
use grapt_types::OperationId;

impl GovernedToolExecutor<FilesystemArtifactStore> for ProjectToolExecutor {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn execute(
        &self,
        _capability: CapabilityId,
        call: ToolCallId,
        input: BoundedToolInput,
        _progress: BoundedProgressSink,
        cancellation: ToolCancellation,
        artifacts: BoundedArtifactWriter<FilesystemArtifactStore>,
    ) -> BoxFuture<'static, ToolExecutionTerminal> {
        let root = self.root.clone();
        let ephemeral = self.ephemeral;
        let checkpoint_root = self.checkpoint_root.clone();
        // Dispatch consumes the prepared effect, including its native pins.
        // The executor must not retain another copy through post-effect hooks.
        let Some(target) = lock(&self.target).take() else {
            return Box::pin(async move {
                match artifacts.capture(std::io::Cursor::new(
                    b"Prepared effect was already dispatched; no effect ran again".to_vec(),
                )) {
                    Ok(detail) => ToolExecutionTerminal::failed(
                        ToolFailureReason::AuthorityUnavailable,
                        Some(detail),
                    ),
                    Err(reason) => ToolExecutionTerminal::failed(reason, None),
                }
            });
        };
        let hook_alias = match &target {
            EffectTarget::Mcp { route, .. } => {
                Some(format!("mcp__{}__{}", route.server, route.tool.as_str()))
            }
            _ => None,
        };
        let mcp = Arc::clone(&self.mcp);
        let plugins = Arc::clone(&self.plugins);
        let plugin_generation = self.plugin_generation.as_ref().map(Arc::clone);
        let session_capabilities = Arc::clone(&self.session_capabilities);
        let location = self.location.clone();
        let forms = self.forms.clone();
        let question_owner = self.question_owner();
        let child = Arc::clone(&self.child_seam);
        let parent = self.parent.clone();
        let message = self.message.clone();
        let agent = self.agent.clone();
        let tool = self.tool.clone();
        let principal = self.principal.clone();
        let attempt = self.attempt.clone();
        let machine_output = self.machine_output;
        let hooks = self.hooks.as_ref().map(Arc::clone);
        let pre_annotations = self.pre_annotations.clone();
        let workers = Arc::clone(&self.workers);
        let terminals = Arc::clone(&self.terminals);
        let ids = Arc::clone(&self.ids);
        let artifact_store = Arc::clone(&self.artifact_store);
        let inbox = Arc::clone(&self.inbox);
        let credential_environment = self.credential_environment.clone();
        let observation = self.observation.clone();
        Box::pin(async move {
            if let Some(observation) = observation {
                observation.started();
            }
            let preserve_output = machine_output || matches!(&target, EffectTarget::Image(_));
            let sensitive_output = machine_output
                || matches!(
                    &target,
                    EffectTarget::CodeMode { .. }
                        | EffectTarget::IntegrationCommand { .. }
                        | EffectTarget::WebFetch(_)
                        | EffectTarget::Image(_)
                );
            let mut execution_receipt = match &target {
                EffectTarget::TerminalExec(spec) => Some(spec.environment.pending_receipt()),
                EffectTarget::Shell { environment, .. } => Some(environment.pending_receipt()),
                _ => None,
            };
            let mut terminal_delivery = None;
            let checkpoint_context =
                checkpoint_root
                    .as_deref()
                    .filter(|_| !ephemeral)
                    .map(|service| coding::checkpoints::FileContext {
                        service,
                        artifacts: &artifact_store,
                        inbox: &inbox,
                        clock: Arc::clone(&forms.clock) as Arc<dyn ClockAuthority + Send + Sync>,
                        ids: Arc::clone(&ids) as Arc<dyn IdAuthority + Send + Sync>,
                        session: &parent,
                        input: &message,
                        call: &call,
                    });
            // PreToolUse has already run before preparation and authorization.
            let mut annotations = pre_annotations;
            let formatter_service = plugins.service_root();
            let formatter = formatting::Context {
                root: &root,
                service: formatter_service,
                credentials: &credential_environment,
                process_owner: Some((&terminals, &parent)),
                settings: if matches!(
                    &target,
                    EffectTarget::Edit(_)
                        | EffectTarget::Coding(
                            coding::CodingTarget::Patch(_)
                                | coding::CodingTarget::Write(_)
                                | coding::CodingTarget::Overwrite { .. }
                        )
                ) {
                    formatting::Settings::load(&root, formatter_service)
                } else {
                    Ok(formatting::Settings::default())
                },
                cancellation: &cancellation,
            };
            let diagnostics = diagnostics::Context {
                root: &root,
                service: formatter_service,
                credentials: &credential_environment,
                process_owner: Some((&terminals, &parent)),
                settings: if matches!(
                    &target,
                    EffectTarget::Edit(_)
                        | EffectTarget::Coding(
                            coding::CodingTarget::Patch(_)
                                | coding::CodingTarget::Write(_)
                                | coding::CodingTarget::Overwrite { .. }
                        )
                ) {
                    diagnostics::Settings::load(&root, formatter_service)
                } else {
                    Ok(diagnostics::Settings::default())
                },
                cancellation: &cancellation,
            };
            let result = {
                let result = match target {
                    EffectTarget::WebFetch(url) => web::run(url, &cancellation)
                        .await
                        .map_err(mcp_call::EffectFailure::from),
                    EffectTarget::AgentWait { timeout_ms } => {
                        agent_wait::run(&child, &parent, timeout_ms, &cancellation)
                            .await
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::AgentList { after } => {
                        agents::run_list(&child, &parent, after.as_ref())
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::Ask(args) => {
                        ask::run(forms.clone(), question_owner, args, hooks.as_deref(), &call)
                            .await
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::Plan => Ok(plan::ACKNOWLEDGEMENT.as_bytes().to_vec()),
                    EffectTarget::CodeMode { request, nested } => {
                        codemode::run(request, nested, call.clone(), workers, ids, artifact_store)
                            .await
                    }
                    EffectTarget::Read(expected) => {
                        run_read(&root, &expected, &input).map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::Image(expected) => {
                        image::run(&root, &expected, &input).map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::Edit(expected) => {
                        let output = match effects::prepare_edit(&root, &expected, &input) {
                            Ok(file) => {
                                coding::checkpoints::run_file_formatted(
                                    checkpoint_context,
                                    &root,
                                    file,
                                    formatter,
                                    diagnostics,
                                )
                                .await
                            }
                            Err(reason) => Err(reason.into()),
                        };
                        if ephemeral {
                            output.and_then(coding::checkpoints::without_undo)
                        } else {
                            output
                        }
                    }
                    EffectTarget::Coding(expected) => {
                        let mutation = matches!(
                            expected,
                            coding::CodingTarget::Patch(_)
                                | coding::CodingTarget::Write(_)
                                | coding::CodingTarget::Overwrite { .. }
                        );
                        let output = match &expected {
                            coding::CodingTarget::Patch(plan) => {
                                coding::checkpoints::run_formatted(
                                    checkpoint_context,
                                    &root,
                                    plan,
                                    &input,
                                    formatter,
                                    diagnostics,
                                )
                                .await
                            }
                            coding::CodingTarget::Write(_)
                            | coding::CodingTarget::Overwrite { .. } => {
                                match coding::file_effects::prepare_write(&root, &expected, &input)
                                {
                                    Ok(file) => {
                                        coding::checkpoints::run_file_formatted(
                                            checkpoint_context,
                                            &root,
                                            file,
                                            formatter,
                                            diagnostics,
                                        )
                                        .await
                                    }
                                    Err(reason) => Err(reason.into()),
                                }
                            }
                            _ => coding::run(&root, &expected, &input)
                                .map_err(mcp_call::EffectFailure::from),
                        };
                        if ephemeral && mutation {
                            output.and_then(coding::checkpoints::without_undo)
                        } else {
                            output
                        }
                    }
                    EffectTarget::TerminalExec(mut spec) => {
                        if !ephemeral && spec.environment.sandbox().is_some() {
                            crate::dogfood::sandbox_worktree_config::before_command(
                                &root,
                                &inbox,
                                &*forms.clock,
                                &*ids,
                                &forms.hub,
                                &parent,
                            )
                            .await;
                        }
                        spec.origin = Some(terminal::recovery::Origin::local(
                            forms.clock.now().as_millis(),
                        ));
                        let operation = OperationId::new(format!("op_{}", ids.mint().as_str()));
                        match operation {
                            Some(operation) => terminals.exec(&parent, operation, spec).await.map(
                                |(bytes, delivery)| {
                                    terminal_delivery = Some(delivery);
                                    bytes
                                },
                            ),
                            None => Err(mcp_call::EffectFailure::from(
                                "terminal operation identity was invalid".to_owned(),
                            )),
                        }
                    }
                    EffectTarget::TerminalStdin(spec) => {
                        if !ephemeral && terminals.contained_input(&parent, &spec) {
                            crate::dogfood::sandbox_worktree_config::before_command(
                                &root,
                                &inbox,
                                &*forms.clock,
                                &*ids,
                                &forms.hub,
                                &parent,
                            )
                            .await;
                        }
                        terminals
                            .stdin(&parent, &spec)
                            .await
                            .map(|(bytes, delivery)| {
                                terminal_delivery = Some(delivery);
                                bytes
                            })
                    }
                    EffectTarget::Shell {
                        command,
                        program,
                        environment,
                    } => {
                        let requested = environment.sandbox().is_some();
                        if !ephemeral && requested {
                            crate::dogfood::sandbox_worktree_config::before_command(
                                &root,
                                &inbox,
                                &*forms.clock,
                                &*ids,
                                &forms.hub,
                                &parent,
                            )
                            .await;
                        }
                        let intended = environment.intended();
                        let shell_root = root.clone();
                        let shell_input = input.clone();
                        let shell_credentials = credential_environment.clone();
                        let result = terminals
                            .run_shell_owned(
                                parent.clone(),
                                call.clone(),
                                if cfg!(windows) {
                                    u64::try_from(forms.clock.now().as_millis()).ok()
                                } else {
                                    None
                                },
                                cancellation.clone(),
                                move |control| async move {
                                    effects::run_shell_cancellable(
                                        &shell_root,
                                        &command,
                                        &program,
                                        &environment,
                                        &shell_input,
                                        &shell_credentials,
                                        control,
                                    )
                                    .await
                                },
                            )
                            .await;
                        match result {
                            Ok(outcome) => {
                                execution_receipt =
                                    Some(sandbox::Receipt::started(outcome.enforcement.clone()));
                                if machine_output {
                                    outcome.tool_command_text()
                                } else {
                                    outcome.model_output()
                                }
                                .map_err(mcp_call::EffectFailure::from)
                            }
                            Err(detail) => {
                                execution_receipt =
                                    Some(sandbox::Receipt::parse(&detail).unwrap_or_else(|| {
                                        sandbox::Receipt {
                                            enforcement: if requested {
                                                sandbox::degraded(detail.clone())
                                            } else {
                                                intended.clone()
                                            },
                                            ran: None,
                                        }
                                    }));
                                Err(mcp_call::EffectFailure::from(detail))
                            }
                        }
                    }

                    EffectTarget::Mcp { route, arguments } => {
                        run_mcp(
                            &mcp,
                            &location,
                            &route,
                            &call,
                            arguments,
                            &forms,
                            mcp_call::McpCallContext {
                                attempt: attempt.as_ref(),
                                service: checkpoint_root.as_deref(),
                                session: &parent,
                                tool: tool.as_str(),
                                custody: &session_capabilities,
                                machine: machine_output.then_some(&artifacts),
                            },
                        )
                        .await
                    }
                    EffectTarget::Plugin { route, arguments } => {
                        run_plugin_tool(
                            &plugins,
                            route,
                            arguments,
                            PluginToolContext {
                                call: &call,
                                session: &parent,
                                agent: &agent,
                                message: &message,
                                principal: &principal,
                            },
                        )
                        .await
                    }
                    EffectTarget::IntegrationCommand { argv } => {
                        let root = root.clone();
                        terminals
                            .run_helper_owned(parent.clone(), move |control| {
                                effects::integration::run(root, argv, control)
                            })
                            .await
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::Subagent { task } => run_subagent(
                        &child,
                        &parent,
                        &agent,
                        &principal,
                        &call,
                        &task,
                        &cancellation,
                    )
                    .await
                    .map_err(mcp_call::EffectFailure::from),
                    EffectTarget::AgentSpawn(task) => {
                        agent_spawn::run(&child, &parent, &agent, &principal, &call, &task)
                            .await
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::AgentInterrupt { child: target } => {
                        agent_interrupt::run(&child, &parent, &target)
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::AgentFollowup { target, text } => {
                        agent_followup::run(&child, &parent, &target, &principal, &call, &text)
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::AgentMessage { target, text } => {
                        agent_message::run(&child, &parent, &target, &call, &text)
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::Skill(request) => skill::run(&plugins, &root, request)
                        .await
                        .map_err(mcp_call::EffectFailure::from),
                    EffectTarget::ReadHistory(request) => {
                        read_history::run(&child, &parent, request)
                            .map_err(mcp_call::EffectFailure::from)
                    }
                    EffectTarget::History {
                        call,
                        cursor,
                        offset,
                    } => crate::dogfood::continuation::history(
                        &inbox,
                        &artifact_store,
                        &parent,
                        call.as_ref(),
                        cursor,
                        offset,
                    )
                    .map_err(mcp_call::EffectFailure::from),
                };
                if matches!(
                    tool.as_str(),
                    terminal::EXEC_COMMAND | terminal::WRITE_STDIN | sandbox_access::TOOL
                ) {
                    let raw = match &result {
                        Ok(output) => std::str::from_utf8(output).ok(),
                        Err(failure) => Some(failure.detail.as_str()),
                    };
                    if let Some(receipt) = raw.and_then(sandbox::Receipt::parse) {
                        execution_receipt = Some(receipt);
                    }
                }
                if let Some(runtime) = hooks.as_deref() {
                    let output = match serde_json::from_slice::<serde_json::Value>(input.as_bytes())
                    {
                        Ok(value) => {
                            runtime
                                .fire_tool(
                                    Event::PostToolUse,
                                    &parent,
                                    (&tool, &call),
                                    &value,
                                    hook_alias.as_deref(),
                                )
                                .await
                        }
                        Err(_) => Err("prepared tool input could not be decoded".into()),
                    };
                    match output {
                        Ok(output) => annotations.extend(output.notes),
                        Err(reason) => annotations.push(format!(
                            "PostToolUse hook failed after dispatch; the effect already ran \
                                 (do not retry solely for this hook failure): {reason}"
                        )),
                    }
                }
                result
            };
            let result = match plugin_generation.as_deref() {
                Some(generation) => {
                    let input_value = serde_json::from_slice::<serde_json::Value>(input.as_bytes())
                        .unwrap_or(serde_json::Value::Null);
                    match result {
                        Ok(output) => {
                            let encoded = serde_json::from_slice::<serde_json::Value>(&output).ok();
                            let result_value = encoded.clone().unwrap_or_else(
                                || serde_json::json!({"output":String::from_utf8_lossy(&output)}),
                            );
                            let event = serde_json::json!({
                                "tool": tool,
                                "sessionID": parent.as_str(),
                                "agent": agent.as_str(),
                                "messageID": message.as_str(),
                                "callID": call.as_str(),
                                "input": input_value,
                                "status": "completed",
                                "result": result_value,
                            });
                            match plugins
                                .apply_callbacks(
                                    generation,
                                    "tool",
                                    "hook.execute.after",
                                    event,
                                    hook_authority(&principal, &parent, &agent, &message, &call),
                                )
                                .await
                            {
                                Ok(Some(event)) => {
                                    let value = event.get("result").cloned().unwrap_or_default();
                                    if encoded.is_some() {
                                        serde_json::to_vec(&value).map_err(|error| {
                                            mcp_call::EffectFailure::from(format!(
                                                "plugin tool after hook returned an invalid result: {error}"
                                            ))
                                        })
                                    } else if let Some(output) =
                                        value.get("output").and_then(serde_json::Value::as_str)
                                    {
                                        Ok(output.as_bytes().to_vec())
                                    } else {
                                        serde_json::to_vec(&value).map_err(|error| {
                                            mcp_call::EffectFailure::from(format!(
                                                "plugin tool after hook returned an invalid result: {error}"
                                            ))
                                        })
                                    }
                                }
                                Ok(None) => Ok(output),
                                Err(error) => Err(mcp_call::EffectFailure::from(format!(
                                    "plugin tool after hook failed: {error}"
                                ))),
                            }
                        }
                        Err(mut failure) => {
                            let event = serde_json::json!({
                                "tool": tool,
                                "sessionID": parent.as_str(),
                                "agent": agent.as_str(),
                                "messageID": message.as_str(),
                                "callID": call.as_str(),
                                "input": input_value,
                                "status": "error",
                                "error": {"message":failure.detail},
                            });
                            match plugins
                                .apply_callbacks(
                                    generation,
                                    "tool",
                                    "hook.execute.after",
                                    event,
                                    hook_authority(&principal, &parent, &agent, &message, &call),
                                )
                                .await
                            {
                                Ok(Some(event)) => {
                                    if let Some(detail) = event
                                        .get("error")
                                        .and_then(|error| error.get("message"))
                                        .and_then(serde_json::Value::as_str)
                                    {
                                        failure.detail = detail.to_owned();
                                    }
                                    Err(failure)
                                }
                                Ok(None) => Err(failure),
                                Err(error) => Err(mcp_call::EffectFailure::from(format!(
                                    "plugin tool after hook failed: {error}"
                                ))),
                            }
                        }
                    }
                }
                None => result,
            };
            let result = annotate_result(result, &annotations, preserve_output);
            // Enforcement belongs to the process owner, outside plugin result edits.
            let result = match execution_receipt {
                Some(receipt) => result
                    .and_then(|output| {
                        receipt
                            .output(output, tool == SHELL_TOOL, machine_output)
                            .map_err(mcp_call::EffectFailure::from)
                    })
                    .map_err(|mut failure| {
                        failure.detail = receipt.error(&failure.detail);
                        failure
                    }),
                None => result,
            };
            match result {
                Ok(output) => match if sensitive_output {
                    artifacts.capture_sensitive(std::io::Cursor::new(output))
                } else {
                    artifacts.capture(std::io::Cursor::new(output))
                } {
                    Ok(output) => {
                        if let Some(delivery) = terminal_delivery {
                            delivery.captured();
                        }
                        ToolExecutionTerminal::completed(Some(output))
                    }
                    Err(reason) => ToolExecutionTerminal::failed(reason, None),
                },
                Err(mut failure) => {
                    failure.detail = native_failure::format(
                        &tool,
                        false,
                        &failure.detail,
                        &credential_environment,
                    );
                    match if sensitive_output {
                        artifacts
                            .capture_sensitive(std::io::Cursor::new(failure.detail.into_bytes()))
                    } else {
                        artifacts.capture(std::io::Cursor::new(failure.detail.into_bytes()))
                    } {
                        Ok(detail) => ToolExecutionTerminal::failed(failure.reason, Some(detail)),
                        Err(reason) => ToolExecutionTerminal::failed(
                            if failure.reason == ToolFailureReason::EffectUnknown {
                                failure.reason
                            } else {
                                reason
                            },
                            None,
                        ),
                    }
                }
            }
        })
    }
}

/// Model-facing outputs can carry hook context. Machine consumers must receive
/// exactly the effect's bytes: in particular, do not add keys to CodeMode's
/// nested JSON envelope or append text to plugin credential/interpolation data.
/// Image receipts also have a strict schema and byte-prefix recognition.
fn annotate_result(
    result: Result<Vec<u8>, mcp_call::EffectFailure>,
    annotations: &[String],
    preserve_output: bool,
) -> Result<Vec<u8>, mcp_call::EffectFailure> {
    if annotations.is_empty() {
        return result;
    }
    match result {
        Ok(output) if preserve_output => Ok(output),
        Ok(mut output) => {
            if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&output) {
                match value.as_object_mut() {
                    Some(object) if !object.contains_key("hookAnnotations") => {
                        object.insert("hookAnnotations".into(), serde_json::json!(annotations));
                    }
                    _ => {
                        value = serde_json::json!({
                            "output": value,
                            "hookAnnotations": annotations,
                        });
                    }
                }
                Ok(value.to_string().into_bytes())
            } else {
                append_annotations(&mut output, annotations);
                Ok(output)
            }
        }
        Err(mut failure) => {
            let mut detail = failure.detail.into_bytes();
            append_annotations(&mut detail, annotations);
            failure.detail = String::from_utf8_lossy(&detail).into_owned();
            Err(failure)
        }
    }
}

fn append_annotations(output: &mut Vec<u8>, annotations: &[String]) {
    output.extend_from_slice(b"\n\n[Lifecycle hook annotations]\n");
    output.extend_from_slice(serde_json::json!(annotations).to_string().as_bytes());
}

/// Grant authority for tool hooks run on behalf of one tool call.
pub(super) fn hook_authority(
    principal: &SealedPrincipal,
    session: &SessionId,
    agent: &AgentId,
    message: &MessageId,
    call: &ToolCallId,
) -> grapt_plugin::CallAuthority {
    grapt_plugin::CallAuthority::Scoped {
        scope: Arc::new(super::super::plugins::PluginCallScope {
            principal: principal.clone(),
            session: session.clone(),
            agent: agent.clone(),
            message: Some(message.clone()),
            operations: super::super::plugins::SESSION_CALL_OPERATIONS,
        }),
        call: call.as_str().to_owned(),
    }
}

struct PluginToolContext<'a> {
    call: &'a ToolCallId,
    session: &'a SessionId,
    agent: &'a AgentId,
    message: &'a MessageId,
    principal: &'a SealedPrincipal,
}

/// Cancels one plugin call if its future is dropped before it settles, so an
/// interrupted or abandoned tool call cannot keep running in the bridge.
struct PluginCallGuard {
    plugins: Arc<super::super::plugins::PluginRuntime>,
    generation: grapt_plugin::PluginGenerationId,
    plugin: String,
    call: String,
    armed: bool,
}

impl Drop for PluginCallGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let plugins = Arc::clone(&self.plugins);
        let (generation, plugin, call) = (
            self.generation,
            std::mem::take(&mut self.plugin),
            std::mem::take(&mut self.call),
        );
        handle.spawn(async move {
            let _ = plugins.cancel_call(generation, plugin, call).await;
        });
    }
}

async fn run_plugin_tool(
    plugins: &Arc<super::super::plugins::PluginRuntime>,
    route: grapt_plugin::PluginToolRoute,
    arguments: serde_json::Value,
    context: PluginToolContext<'_>,
) -> Result<Vec<u8>, mcp_call::EffectFailure> {
    let mut guard = PluginCallGuard {
        plugins: Arc::clone(plugins),
        generation: route.generation,
        plugin: route.plugin_id.clone(),
        call: context.call.as_str().to_owned(),
        armed: true,
    };
    let invocation = plugins.invoke(
        route,
        arguments,
        serde_json::json!({
            "sessionID": context.session.as_str(),
            "agent": context.agent.as_str(),
            "messageID": context.message.as_str(),
            "callID": context.call.as_str(),
        }),
        super::super::plugins::PluginCallScope {
            principal: context.principal.clone(),
            session: context.session.clone(),
            agent: context.agent.clone(),
            message: Some(context.message.clone()),
            operations: super::super::plugins::SESSION_CALL_OPERATIONS,
        },
        context.call.as_str().to_owned(),
    );
    // An interrupt drops this future mid-await and the guard then cancels the
    // bridge call. No product owner signals `ToolCancellation`, so the guard is
    // the cancellation path.
    let result = invocation.await;
    guard.armed = false;
    result.map_err(mcp_call::EffectFailure::from)
}

#[cfg(test)]
mod lifecycle_output_tests {
    use super::*;

    #[test]
    fn lifecycle_annotations_preserve_json_values_and_existing_keys() -> Result<(), String> {
        let annotations = vec!["policy context".to_owned()];
        for value in [
            serde_json::json!({"sum": 7}),
            serde_json::json!([1, 2]),
            serde_json::json!(null),
            serde_json::json!({"hookAnnotations": ["original"], "sum": 7}),
        ] {
            let encoded = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
            let output = annotate_result(Ok(encoded), &annotations, false)
                .map_err(|failure| failure.detail)?;
            let annotated: serde_json::Value =
                serde_json::from_slice(&output).map_err(|error| error.to_string())?;
            assert_eq!(annotated["hookAnnotations"], serde_json::json!(annotations));
            if value.is_object() && value.get("hookAnnotations").is_none() {
                assert_eq!(annotated["sum"], value["sum"]);
            } else {
                assert_eq!(annotated["output"], value);
            }
        }
        Ok(())
    }

    #[test]
    fn lifecycle_machine_outputs_are_byte_exact() -> Result<(), String> {
        for original in [
            br#"{ "value": {"sum":7}, "artifacts": [] }"#.to_vec(),
            br#"{"kind":"secode-image-v1","data":"pixels"}"#.to_vec(),
            b"secret credential\n".to_vec(),
            vec![0, 255, 1],
        ] {
            let output =
                annotate_result(Ok(original.clone()), &["policy context".to_owned()], true)
                    .map_err(|failure| failure.detail)?;
            assert_eq!(output, original);
        }
        Ok(())
    }

    #[test]
    fn lifecycle_failure_annotations_preserve_uncertainty_and_original_detail() {
        let failure = mcp_call::EffectFailure {
            reason: ToolFailureReason::EffectUnknown,
            detail: "dispatch result unknown".to_owned(),
        };
        let Err(failure) = annotate_result(
            Err(failure),
            &["PostToolUse hook failed after dispatch".to_owned()],
            true,
        ) else {
            panic!("an uncertain effect became successful");
        };
        assert_eq!(failure.reason, ToolFailureReason::EffectUnknown);
        assert!(failure.detail.starts_with("dispatch result unknown\n"));
        assert!(failure
            .detail
            .contains("PostToolUse hook failed after dispatch"));
    }
}

use super::*;

impl PermissionToolRunner {
    pub(super) fn early_failure(
        &self,
        tool: &str,
        reason: ToolFailureReason,
        detail: String,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let detail = if reason == ToolFailureReason::EffectFailed {
            native_failure::format(tool, true, &detail, &self.credential_environment)
        } else {
            detail
        };
        failed_with_detail(Arc::clone(&self.artifacts), reason, detail)
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::dogfood) fn new(
        root: PathBuf,
        session: SessionId,
        message: MessageId,
        agent: AgentId,
        principal: SealedPrincipal,
        project: ProjectId,
        permissions: Arc<Mutex<PermissionHttpState>>,
        authority: Arc<ToolPermissionAuthority>,
        inbox: Arc<Mutex<Inbox>>,
        ids: Arc<OsRandomIds>,
        clock: Arc<SystemClock>,
        artifacts: Arc<FilesystemArtifactStore>,
        hub: Arc<DogfoodEventHub>,
        published: Arc<super::super::mcp::PublishedTools>,
        mcp: Arc<super::super::mcp::McpRegistry>,
        plugins: Arc<super::super::plugins::PluginRuntime>,
        session_capabilities: Arc<Mutex<super::super::context::SessionCapabilities>>,
        location: String,
        approval_location: grapt_types::LocationRef,
        child_seam: super::super::subagent::ChildSeam,
        worker_registry: Arc<WorkerRegistry>,
        terminals: Arc<TerminalRegistry>,
        policy: super::super::permission_preset::ResolvedPolicy,
        credential_environment: Vec<String>,
    ) -> Self {
        Self {
            shadow: Arc::new(shadow::Ledger::disabled()),
            approval_location,
            ephemeral: false,
            credential_environment,
            hooks: None,
            checkpoint_root: None,
            terminals,
            workers: Arc::new(codemode::Workers::new(worker_registry, session.clone())),
            root,
            session,
            message,
            agent,
            principal,
            project,
            permissions,
            authority,
            inbox,
            ids,
            clock,
            artifacts,
            hub,
            decision_waits: Arc::new(crate::dogfood::human_input::Waits::default()),
            state_feedback: None,
            published,
            mcp,
            plugin_generation: plugins.generation(),
            plugins,
            session_capabilities,
            location,
            child_seam,
            policy,
        }
    }

    /// Restrict this runner to foreground tools without harness persistence.
    ///
    /// The caller must supply a volatile inbox and artifact store and select
    /// volatile composition before starting plugins, MCP, or lifecycle hooks.
    /// This is not an OS/provider retention guarantee: coding/shell effects are
    /// intentional user effects, including atomic file-replacement staging.
    /// Automatic checkpoint capture and file undo are unavailable. HTTP/slash
    /// undo routes must also be disabled by the caller.
    pub(in crate::dogfood) fn with_ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        if ephemeral {
            self.shadow = Arc::new(shadow::Ledger::disabled());
            self.hooks = None;
            self.checkpoint_root = None;
            self.plugin_generation = None;
        }
        self
    }

    pub(in crate::dogfood) fn with_shadow_ledger(mut self, ledger: Arc<shadow::Ledger>) -> Self {
        if !self.ephemeral {
            self.shadow = ledger;
        }
        self
    }

    pub(in crate::dogfood) fn with_status_feedback(
        mut self,
        feedback: crate::dogfood::session_status::RetryFeedback,
    ) -> Self {
        feedback.attach_input_waits(Arc::clone(&self.decision_waits));
        self.state_feedback = Some(feedback);
        self
    }

    pub(in crate::dogfood) fn with_hooks(
        mut self,
        runtime: Arc<super::super::hooks::HookRuntime>,
    ) -> Self {
        if !self.ephemeral {
            self.hooks = Some(runtime);
        }
        self
    }

    /// Enable automatic before-images on the live patch path. Fixture runners
    /// that invoke effects directly need no service profile.
    pub(in crate::dogfood) fn with_checkpoints(mut self, service_root: PathBuf) -> Self {
        if !self.ephemeral {
            self.checkpoint_root = Some(service_root);
        }
        self
    }
}

impl PermissionToolRunner {
    /// Run a host operation that originated inside a plugin callback.
    ///
    /// The outer callback already owns the plugin supervisor exchange, so
    /// attempting `hook.execute.before` again would wait on that same exchange
    /// until its timeout. The operation still enters the ordinary prepared
    /// permission and governed-effect path; only the reentrant plugin hook pass
    /// is omitted.
    pub(in crate::dogfood) fn run_plugin_host_effect(
        &self,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        if self.ephemeral {
            return self.early_failure(
                &tool,
                ToolFailureReason::EffectFailed,
                "plugin host effects are unavailable in ephemeral sessions; no effect ran".into(),
            );
        }
        let runner = self.clone();
        Box::pin(async move {
            let observation =
                runner
                    .shadow
                    .dispatch(&runner.session, None, shadow::Origin::PluginHost, &tool);
            let outcome = runner
                .run_prepared(
                    None,
                    call,
                    tool,
                    input,
                    ToolRunOptions {
                        machine_output: true,
                        plugin_hooks: false,
                        hook: super::super::hooks::HookResult::default(),
                        observation: observation.clone(),
                    },
                    None,
                )
                .await;
            if let Some(observation) = observation {
                observation.finish(&outcome);
            }
            outcome
        })
    }

    pub(super) fn run_for_output(
        &self,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
        origin: shadow::Origin,
        nested: Option<grapt_runtime::NestedTools>,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        // Refuse before callbacks, preparation, permissions, or external work.
        if self.ephemeral {
            if let Err(detail) = ephemeral::ensure_supported(&tool) {
                return self.early_failure(&tool, ToolFailureReason::EffectFailed, detail.into());
            }
        }
        let runner = self.clone();
        Box::pin(async move {
            let observation =
                runner
                    .shadow
                    .dispatch(&runner.session, attempt.as_ref(), origin, &tool);
            let outcome = runner
                .apply_input_callbacks(
                    attempt,
                    call,
                    tool,
                    input,
                    ToolRunOptions {
                        machine_output: matches!(origin, shadow::Origin::Nested),
                        plugin_hooks: true,
                        hook: super::super::hooks::HookResult::default(),
                        observation: observation.clone(),
                    },
                    nested,
                )
                .await;
            if let Some(observation) = observation {
                observation.finish(&outcome);
            }
            outcome
        })
    }

    fn apply_input_callbacks(
        &self,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
        options: ToolRunOptions,
        nested: Option<grapt_runtime::NestedTools>,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let runner = self.clone();
        Box::pin(async move {
            let input_value = match serde_json::from_slice::<serde_json::Value>(input.as_bytes()) {
                Ok(value) => value,
                Err(error) => {
                    return runner
                        .early_failure(
                            &tool,
                            ToolFailureReason::EffectFailed,
                            format!("tool input is invalid JSON: {error}"),
                        )
                        .await;
                }
            };
            let event = serde_json::json!({
                "tool": tool,
                "sessionID": runner.session.as_str(),
                "agent": runner.agent.as_str(),
                "messageID": runner.message.as_str(),
                "callID": call.as_str(),
                "input": input_value,
            });
            let input = match runner.plugin_generation.as_deref() {
                Some(generation) => {
                    match runner
                        .plugins
                        .apply_callbacks(
                            (generation, "tool", "hook.execute.before"),
                            event,
                            super::executor::hook_authority(
                                &runner.principal,
                                &runner.session,
                                &runner.agent,
                                &runner.message,
                                &call,
                            ),
                            crate::dogfood::plugins::OnCallbackFailure::Refuse,
                        )
                        .await
                    {
                        Ok(Some(event)) => {
                            let Some(value) = event.get("input") else {
                                return runner
                                    .early_failure(
                                        &tool,
                                        ToolFailureReason::EffectFailed,
                                        "plugin tool before hook removed the input".to_owned(),
                                    )
                                    .await;
                            };
                            let bytes = match serde_json::to_vec(value) {
                                Ok(bytes) => bytes,
                                Err(error) => {
                                    return runner.early_failure(&tool,
                                    ToolFailureReason::EffectFailed,
                                    format!("plugin tool before hook returned invalid input: {error}"),
                                )
                                .await;
                                }
                            };
                            match BoundedToolInput::new(bytes) {
                                Ok(input) => input,
                                Err(error) => {
                                    return runner.early_failure(&tool,
                                    ToolFailureReason::EffectFailed,
                                    format!("plugin tool before hook exceeded the input bound: {error}"),
                                )
                                .await;
                                }
                            }
                        }
                        Ok(None) => input,
                        Err(error) => {
                            return runner
                                .early_failure(
                                    &tool,
                                    ToolFailureReason::EffectFailed,
                                    format!("plugin tool before hook failed: {error}"),
                                )
                                .await;
                        }
                    }
                }
                None => input,
            };
            runner
                .run_prepared(attempt, call, tool, input, options, nested)
                .await
        })
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn prepare_and_authorize(
        &self,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
        options: ToolRunOptions,
        nested: Option<grapt_runtime::NestedTools>,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        if !self.policy.permits_agent_tool(&tool) {
            return self.early_failure(
                &tool,
                ToolFailureReason::AuthorityUnavailable,
                "tool is excluded by this agent or an ancestor's tool list".to_owned(),
            );
        }
        if self.ephemeral {
            if let Err(detail) = ephemeral::ensure_supported(&tool) {
                return self.early_failure(&tool, ToolFailureReason::EffectFailed, detail.into());
            }
        }
        // Every published MCP or plugin tool keeps its original schema on
        // ordinary model calls as well as CodeMode/machine-output calls.
        if let Err(error) = schema::validate_published(&self.published, &tool, input.as_bytes()) {
            if options.hook.input.is_some() {
                return self.rejected_hook_input(&tool, error);
            }
            return self.early_failure(
                &tool,
                ToolFailureReason::EffectFailed,
                format!("tool input does not match its published schema: {error}"),
            );
        }
        let preparation = if self.ephemeral {
            ephemeral::prepare(
                &self.root,
                self.plugins.service_root(),
                &tool,
                &input,
                &self.credential_environment,
                &self.session,
                lock(&self.permissions).msys_session_granted(&self.session),
            )
        } else if tool == terminal::WRITE_STDIN {
            self.terminals
                .prepare_stdin(&self.root, &self.session, &input)
        } else if tool == EXECUTE_TOOL {
            codemode::prepare(&self.published, &input, nested)
        } else if matches!(
            tool.as_str(),
            SHELL_TOOL | terminal::EXEC_COMMAND | sandbox_access::TOOL
        ) {
            shell_environment::prepare_session(
                &self.root,
                self.plugins.service_root(),
                &tool,
                &input,
                &self.credential_environment,
                &self.session,
                lock(&self.permissions).msys_session_granted(&self.session),
            )
        } else {
            prepare_effect_with_plugins(
                &self.root,
                &tool,
                &input,
                &self.published,
                &self.plugins,
                self.plugin_generation.as_deref(),
                &self.credential_environment,
            )
        };
        let mut prepared = match preparation {
            Ok(prepared) => prepared,
            Err(error) => {
                if options.hook.input.is_some() {
                    return self.rejected_hook_input(&tool, &error);
                }
                return self.early_failure(
                    &tool,
                    ToolFailureReason::EffectFailed,
                    if sandbox::receipt(&error).is_some() {
                        error
                    } else {
                        format!("tool preparation failed: {error}")
                    },
                );
            }
        };
        if let EffectTarget::TerminalExec(spec) = &mut prepared.target {
            if let Err(error) = self.terminals.prepare_grants(&self.project, spec) {
                return self.early_failure(&tool, ToolFailureReason::AuthorityUnavailable, error);
            }
        }
        let runner = self.clone();
        Box::pin(async move {
            let mut plan = match runner.permission_plan(&prepared, &input, options.hook.permission)
            {
                Ok(plan) => plan,
                Err(error) => {
                    return runner
                        .early_failure(&tool, error.reason, error.detail)
                        .await
                }
            };
            let mut options = options;
            if let Some(hooks) = &runner.hooks {
                let decision = async {
                    if !runner.needs_permission_hook(&plan, &prepared)? {
                        return Ok(super::super::hooks::HookResult::default());
                    }
                    let value = serde_json::from_slice(input.as_bytes())
                        .map_err(|_| "permission hook input is invalid JSON")?;
                    let alias = runner
                        .published
                        .routes
                        .get(&tool)
                        .map(|route| format!("mcp__{}__{}", route.server, route.tool.as_str()));
                    hooks
                        .fire_tool(
                            super::super::hooks::Event::PermissionRequest,
                            &runner.session,
                            (&tool, &call),
                            &value,
                            alias.as_deref(),
                        )
                        .await
                }
                .await;
                match decision {
                    Ok(output) => {
                        hook_decision::layers(&mut plan.layers, &plan.checks, output.permission);
                        sandbox_access::consent_layers(&mut plan.layers, &plan.checks);
                        options.hook.notes.extend(output.notes);
                    }
                    Err(reason) => {
                        return runner
                            .early_failure(
                                &tool,
                                ToolFailureReason::EffectFailed,
                                format!("PermissionRequest hook refused; no effect ran: {reason}"),
                            )
                            .await
                    }
                }
            }
            runner
                .authorize_prepared(attempt, call, tool, input, options, prepared, plan)
                .await
        })
    }

    fn project_executor(
        &self,
        target: EffectTarget,
        tool: String,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        options: ToolRunOptions,
        approvals: grapt_api::ApprovalWatch,
    ) -> ProjectToolExecutor {
        ProjectToolExecutor {
            observation: options.observation,
            pre_annotations: options.hook.notes,
            credential_environment: self.credential_environment.clone(),
            machine_output: options.machine_output,
            ephemeral: self.ephemeral,
            hooks: self.hooks.as_ref().map(Arc::clone),
            checkpoint_root: self.checkpoint_root.clone(),
            workers: Arc::clone(&self.workers),
            terminals: Arc::clone(&self.terminals),
            ids: Arc::clone(&self.ids),
            artifact_store: Arc::clone(&self.artifacts),
            inbox: Arc::clone(&self.inbox),
            mcp: Arc::clone(&self.mcp),
            plugins: Arc::clone(&self.plugins),
            plugin_generation: options
                .plugin_hooks
                .then(|| self.plugin_generation.as_ref().map(Arc::clone))
                .flatten(),
            session_capabilities: Arc::clone(&self.session_capabilities),
            location: self.location.clone(),
            root: self.root.clone(),
            target: Mutex::new(Some(target)),
            child_seam: Arc::clone(&self.child_seam),
            parent: self.session.clone(),
            message: self.message.clone(),
            agent: self.agent.clone(),
            tool,
            principal: self.principal.clone(),
            forms: super::super::elicitation::FormProducer {
                inbox: Arc::clone(&self.inbox),
                ids: Arc::clone(&self.ids),
                clock: Arc::clone(&self.clock),
                hub: Arc::clone(&self.hub),
                hooks: self
                    .hooks
                    .as_ref()
                    .map(|hooks| (self.session.clone(), Arc::clone(hooks))),
            },
            attempt,
            approvals,
            decision_waits: Arc::clone(&self.decision_waits),
            state_feedback: self.state_feedback.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn authorize_prepared(
        &self,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
        options: ToolRunOptions,
        prepared: PreparedEffect,
        plan: permission_hooks::PermissionPlan,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        if let Some(observation) = &options.observation {
            observation.prepared(&prepared.target);
        }
        let refused = match &prepared.target {
            EffectTarget::Shell { environment, .. }
                if !environment.msys_executables().is_empty() =>
            {
                Some(environment.pending_receipt())
            }
            EffectTarget::TerminalExec(spec) if !spec.environment.msys_executables().is_empty() => {
                Some(spec.environment.pending_receipt())
            }
            _ => None,
        };
        let permission: PermissionId = match format!("per_{}", self.ids.mint().as_str()).parse() {
            Ok(permission) => permission,
            Err(_) => {
                return self.early_failure(
                    &tool,
                    ToolFailureReason::AuthorityUnavailable,
                    refusal_detail(refused.as_ref(), "tool permission identity was invalid"),
                )
            }
        };
        let mut approvals = match self.hub.watch_approvals(
            &self.principal,
            &self.session,
            &permission,
            self.approval_location.clone(),
        ) {
            Ok(watch) => watch,
            Err(error) => {
                return self.early_failure(
                    &tool,
                    ToolFailureReason::AuthorityUnavailable,
                    refusal_detail(refused.as_ref(), &error.to_string()),
                )
            }
        };
        let (pending, mut guard) = match self.begin(&call, &prepared, permission, &plan) {
            Ok(pending) => pending,
            Err(error) => {
                return self.early_failure(
                    &tool,
                    error.reason,
                    refusal_detail(refused.as_ref(), &error.detail),
                )
            }
        };
        let waiting_hooks = self.hooks.as_ref().map(Arc::clone);
        let hook_session = self.session.clone();
        let authorization_observation = options.observation.clone();
        let governed = GovernedToolRunner::new(
            Arc::new(self.project_executor(
                prepared.target,
                tool,
                attempt,
                options,
                approvals.clone(),
            )),
            Arc::clone(&self.artifacts),
        );
        let (progress, _drain) = BoundedProgressSink::channel();
        let (cancel, cancellation) = ToolCancellation::channel();
        let failure_artifacts = Arc::clone(&self.artifacts);
        Box::pin(async move {
            // The sender is held for the effect's lifetime so the channel reads
            // as open rather than closed. No product owner sends on it: an
            // interrupt drops this future, and each effect's drop guard is what
            // cancels its external work.
            let _cancel = cancel;
            let waiting = pending.awaits_decision();
            if waiting {
                if let Some(hooks) = waiting_hooks {
                    if let Err(reason) = hooks
                        .fire(
                            super::super::hooks::Event::WaitingForInput,
                            &hook_session,
                            None,
                        )
                        .await
                    {
                        return captured_failure(
                            &failure_artifacts,
                            ToolFailureReason::EffectFailed,
                            &refusal_detail(
                                refused.as_ref(),
                                &format!("permission wait hook failed; no effect ran: {reason}"),
                            ),
                        );
                    }
                }
            }
            let authorization = pending.consume_and_run(move |capability| {
                if let Some(observation) = authorization_observation {
                    observation.authorized();
                }
                governed.run_effect(capability, call, input, progress, cancellation)
            });
            tokio::pin!(authorization);
            let authorized = tokio::select! {
                biased;
                authorized = &mut authorization => authorized,
                availability = approvals.unavailable(), if waiting => {
                    match guard.cancel(grapt_permission::CancelReason::ApprovalUnavailable) {
                        Ok(false) => authorization.await, // An already-committed reply wins.
                        Ok(true) => {
                            let (reason, detail) = if availability.is_ok() {
                                (ToolFailureReason::ApprovalUnavailable,
                                 "approval connection lost: no matching approver remains; no effect ran")
                            } else {
                                (ToolFailureReason::AuthorityUnavailable,
                                 "approval availability could not be read; no effect ran")
                            };
                            return captured_failure(&failure_artifacts, reason,
                                &refusal_detail(refused.as_ref(), detail));
                        }
                        Err(error) => return captured_failure(&failure_artifacts, ToolFailureReason::AuthorityUnavailable,
                            &refusal_detail(refused.as_ref(), &format!("approval unavailable; pending permission settlement failed: {error}; no effect ran"))),
                    }
                }
            };
            guard.finish();
            drop(approvals);
            settle_authorized_effect(authorized, &failure_artifacts, refused.as_ref()).await
        })
    }
}

pub(super) async fn settle_authorized_effect(
    authorized: Result<BoxFuture<'static, ToolTerminalOutcome>, ToolAuthorizationError>,
    artifacts: &FilesystemArtifactStore,
    refused: Option<&sandbox::Receipt>,
) -> ToolTerminalOutcome {
    match authorized {
        Ok(effect) => effect.await,
        Err(ToolAuthorizationError::ApprovalUnavailable(_)) => captured_failure(
            artifacts, ToolFailureReason::ApprovalUnavailable,
            &refusal_detail(refused, "approval unavailable (headless): no matching approver is attached; no effect ran"),
        ),
        Err(ToolAuthorizationError::Rejected(_)) => captured_failure(
            artifacts,
            ToolFailureReason::AuthorityUnavailable,
            &refusal_detail(refused, "the permission request was rejected; do not repeat the call unless the decision changes"),
        ),
        Err(error) => captured_failure(
            artifacts,
            ToolFailureReason::AuthorityUnavailable,
            &refusal_detail(refused, &format!("tool authorization failed: {error}")),
        ),
    }
}

pub(super) fn refusal_detail(receipt: Option<&sandbox::Receipt>, detail: &str) -> String {
    receipt.map_or_else(
        || detail.to_owned(),
        |receipt| native_failure::format(SHELL_TOOL, false, &receipt.error(detail), &[]),
    )
}

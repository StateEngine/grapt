use super::*;

impl LiveLocation {
    pub(super) fn shell_description(&self, workspace: &Path) -> Result<String, String> {
        let credentials = self
            .provider_snapshot()?
            .targets
            .0
            .credential_environment_names()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let environment = super::super::tools::shell_environment::Environment::read(
            &self.service_root,
            workspace,
            &credentials,
        )?;
        Ok(super::super::tools::exec_description(&environment))
    }

    pub(super) fn load_hooks(
        &self,
        workspace: &Path,
    ) -> Result<super::super::hooks::HookRuntime, String> {
        if self.ephemeral {
            return super::super::hooks::HookRuntime::load_ephemeral(workspace, &self.service_root);
        }
        let credentials: Vec<String> = self
            .provider_snapshot()?
            .targets
            .0
            .credential_environment_names()
            .map(str::to_owned)
            .collect();
        super::super::hooks::HookRuntime::load(workspace, &self.service_root, &credentials)
            .map(|hooks| hooks.with_process_owner(Arc::clone(&self.terminals)))
    }
}

impl LiveExecutor {
    pub(in crate::dogfood) async fn waiting_for_input(
        &self,
        session: &SessionId,
    ) -> Result<(), String> {
        let record = {
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                .get(session)
                .map_err(|error| error.to_string())?
        };
        let root = std::fs::canonicalize(record.placement.directory())
            .map_err(|error| format!("Session hook directory is unreadable: {error}"))?;
        let runtime =
            self.location
                .load_hooks(&root)?
                .with_notices(super::super::hooks::NoticeContext {
                    inbox: Arc::clone(&self.inbox),
                    ids: Arc::clone(&self.ids),
                    clock: Arc::clone(&self.clock),
                    scope: grapt_session::notice::LifecycleHookScope::Session,
                });
        runtime.waiting_for_input(session, None).await
    }

    fn plugin_hooks(
        &self,
        root: &Path,
        input: &MessageId,
    ) -> Result<Arc<super::super::hooks::HookRuntime>, String> {
        Ok(Arc::new(self.location.load_hooks(root)?.with_notices(
            super::super::hooks::NoticeContext {
                inbox: Arc::clone(&self.inbox),
                ids: Arc::clone(&self.ids),
                clock: Arc::clone(&self.clock),
                scope: grapt_session::notice::LifecycleHookScope::Input(input.clone()),
            },
        )))
    }
    /// Execute one command-template shell interpolation through the same
    /// permission authority and bounded effect adapter used by provider tool
    /// calls. The caller supplies the eventual prompt message as the durable
    /// permission source; no plugin process receives child-process authority.
    pub(in crate::dogfood) async fn plugin_shell_interpolation(
        &self,
        session: SessionId,
        message: MessageId,
        agent: AgentId,
        principal: SealedPrincipal,
        command: String,
    ) -> Result<String, String> {
        self.require_persistent_plugins()?;
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        let record = SessionPorts::new(Arc::clone(&self.inbox), &authorities)
            .get(&session)
            .map_err(|error| error.to_string())?;
        let root = std::fs::canonicalize(record.placement.directory())
            .map_err(|error| format!("Session tool directory is unreadable: {error}"))?;
        if !root.is_dir() {
            return Err("Session tool directory is not a directory".to_owned());
        }
        let hooks = self.plugin_hooks(&root, &message)?;
        let input = BoundedToolInput::new(
            serde_json::to_vec(&serde_json::json!({"command":command}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let call = ToolCallId::new(format!("call_{}", self.ids.mint().as_str()))
            .ok_or_else(|| "shell interpolation call identity is invalid".to_owned())?;
        let policy = super::super::permission_preset::effective(
            &self.location.service_root,
            &record,
            |id| {
                SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                    .get(id)
                    .map_err(|error| error.to_string())
            },
        )?;
        let runner = PermissionToolRunner::new(
            root,
            session.clone(),
            message,
            agent,
            principal,
            record.project,
            Arc::clone(&self.location.permissions),
            Arc::clone(&self.location.tool_permissions),
            Arc::clone(&self.inbox),
            Arc::clone(&self.ids),
            Arc::clone(&self.clock),
            Arc::clone(&self.location.tool_artifacts),
            Arc::clone(&self.hub),
            self.published.get(&session),
            Arc::clone(&self.mcp),
            Arc::clone(&self.plugins),
            Arc::clone(&self.location.session_capabilities),
            registry_location(&record.placement),
            record.placement.clone(),
            Arc::clone(&self.location.child_seam),
            Arc::clone(&self.location.codemode_workers),
            Arc::clone(&self.location.terminals),
            policy,
            self.location
                .provider_snapshot()?
                .targets
                .0
                .credential_environment_names()
                .map(str::to_owned)
                .collect(),
        )
        .with_hooks(hooks)
        .with_shadow_ledger(Arc::clone(&self.location.shadow));
        match runner
            .run_plugin_host_effect(call, super::super::tools::SHELL_TOOL.to_owned(), input)
            .await
        {
            ToolTerminalOutcome::Completed {
                output: Some(output),
            } => super::super::continuation::read_text(
                &self.location.tool_artifacts,
                output.reference(),
                output.disposition(),
                "command interpolation output",
            )
            .and_then(|text| super::super::tools::sandbox::command_text(text.as_str())),
            ToolTerminalOutcome::Completed { output: None } => Ok(String::new()),
            ToolTerminalOutcome::Failed { reason, detail } => {
                let detail = detail
                    .as_ref()
                    .map(|output| {
                        super::super::continuation::read_text(
                            &self.location.tool_artifacts,
                            output.reference(),
                            output.disposition(),
                            "command interpolation failure",
                        )
                        .map(|text| text.as_str().to_owned())
                    })
                    .transpose()?
                    .unwrap_or_default();
                Err(format!("shell interpolation failed: {reason:?}: {detail}"))
            }
            ToolTerminalOutcome::Cancelled { by } => {
                Err(format!("shell interpolation cancelled by {by:?}"))
            }
            ToolTerminalOutcome::ResultMissing => {
                Err("shell interpolation result is missing".to_owned())
            }
        }
    }

    /// Run one plugin integration command method's exact argv as a permission-
    /// governed, durably settled tool effect attributed to the requesting
    /// plugin call. The captured credential stays in the product artifact
    /// store; callers receive only the trimmed text or a reason without the
    /// command's output.
    pub(in crate::dogfood) async fn plugin_integration_command(
        &self,
        session: SessionId,
        message: MessageId,
        agent: AgentId,
        principal: SealedPrincipal,
        argv: Vec<String>,
    ) -> Result<String, String> {
        self.require_persistent_plugins()?;
        let record = {
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                .get(&session)
                .map_err(|error| error.to_string())?
        };
        let root = std::fs::canonicalize(record.placement.directory())
            .map_err(|error| format!("Session tool directory is unreadable: {error}"))?;
        let hooks = self.plugin_hooks(&root, &message)?;
        let input = BoundedToolInput::new(
            serde_json::to_vec(&serde_json::json!({"argv": argv}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let call = ToolCallId::new(format!("call_{}", self.ids.mint().as_str()))
            .ok_or_else(|| "integration command call identity is invalid".to_owned())?;
        let policy = {
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            super::super::permission_preset::effective(
                &self.location.service_root,
                &record,
                |id| {
                    SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                        .get(id)
                        .map_err(|error| error.to_string())
                },
            )?
        };
        let runner = PermissionToolRunner::new(
            root,
            session.clone(),
            message,
            agent,
            principal,
            record.project,
            Arc::clone(&self.location.permissions),
            Arc::clone(&self.location.tool_permissions),
            Arc::clone(&self.inbox),
            Arc::clone(&self.ids),
            Arc::clone(&self.clock),
            Arc::clone(&self.location.tool_artifacts),
            Arc::clone(&self.hub),
            self.published.get(&session),
            Arc::clone(&self.mcp),
            Arc::clone(&self.plugins),
            Arc::clone(&self.location.session_capabilities),
            registry_location(&record.placement),
            record.placement.clone(),
            Arc::clone(&self.location.child_seam),
            Arc::clone(&self.location.codemode_workers),
            Arc::clone(&self.location.terminals),
            policy,
            self.location
                .provider_snapshot()?
                .targets
                .0
                .credential_environment_names()
                .map(str::to_owned)
                .collect(),
        )
        .with_hooks(hooks)
        .with_shadow_ledger(Arc::clone(&self.location.shadow));
        match runner
            .run_plugin_host_effect(
                call,
                super::super::tools::INTEGRATION_COMMAND_TOOL.to_owned(),
                input,
            )
            .await
        {
            ToolTerminalOutcome::Completed {
                output: Some(output),
            } => super::super::continuation::read_text(
                &self.location.tool_artifacts,
                output.reference(),
                output.disposition(),
                "integration command credential",
            )
            .map(|text| text.as_str().to_owned()),
            ToolTerminalOutcome::Completed { output: None } => {
                Err("integration command produced no credential".to_owned())
            }
            ToolTerminalOutcome::Failed { reason, .. } => Err(format!(
                "integration command failed ({reason:?}); its output is retained in the Session tool history"
            )),
            ToolTerminalOutcome::Cancelled { by } => {
                Err(format!("integration command cancelled by {by:?}"))
            }
            ToolTerminalOutcome::ResultMissing => {
                Err("integration command result is missing".to_owned())
            }
        }
    }

    fn require_persistent_plugins(&self) -> Result<(), String> {
        if self.location.ephemeral {
            return Err("ephemeral sessions do not support plugin execution".to_owned());
        }
        Ok(())
    }
}

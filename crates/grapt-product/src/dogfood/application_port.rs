impl DogfoodApplicationPort for DogfoodApplication {
    fn session_rename(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionRenameRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(session_title::manual(
            self,
            session_id.into_inner(),
            request,
        ))
    }
    fn session_agent_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionAgentSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::agent(self, session_id.into_inner(), request))
    }

    fn session_model_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::model(self, session_id.into_inner(), request))
    }
    fn session_preset_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<grapt_api::u3a::SessionPresetResponseDto, SessionLookupApplicationError>
    {
        ready(selection::preset_get(self, session_id.into_inner()))
    }

    fn session_preset_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionPresetSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::preset(self, session_id.into_inner(), request))
    }

    fn session_subagent_model_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionSubagentModelResponseDto,
        SessionLookupApplicationError,
    > {
        ready(selection::subagent_get(self, session_id.into_inner()))
    }
    fn session_subagent_model_set(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::subagent_set(
            self,
            session_id.into_inner(),
            Some(request),
        ))
    }
    fn session_subagent_model_clear(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::subagent_set(self, session_id.into_inner(), None))
    }
    fn sandbox_grant_list(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SandboxGrantListResponseDto,
        grapt_api::u3a::PermissionApplicationError,
    > {
        ready(permission::grants::list(self, session.into_inner()))
    }
    fn sandbox_grant_revoke(
        &self,
        principal: SealedPrincipal,
        session: WireId<SessionId>,
        request: grapt_api::u3a::SandboxGrantDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::SandboxGrantRevokeResponseDto,
        grapt_api::u3a::PermissionApplicationError,
    > {
        permission::grants::revoke(self, principal, session.into_inner(), request)
    }
    fn permission_saved_list(
        &self,
        _principal: SealedPrincipal,
        project: Option<WireId<ProjectId>>,
    ) -> ApplicationFuture<grapt_api::u3a::PermissionSavedListResponseDto, InvalidRequestErrorDto>
    {
        ready(permission::saved(self, project.map(WireId::into_inner)))
    }
    fn session_form_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<grapt_api::u3a::SessionFormListResponseDto, SessionLookupApplicationError>
    {
        ready((|| {
            let session = session_id.into_inner();
            context::require_session(self, &session)?;
            let held = self.lock_inbox();
            let forms = grapt_form::catalog::pending_forms_for(held.store(), Some(&session))
                .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
            decode(json!({"data": forms.into_iter().map(|form| form.info).collect::<Vec<_>>()}))
                .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))
        })())
    }

    fn global_form_get(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::FormResponseDto,
        grapt_api::u3a::FormLookupApplicationError,
    > {
        Box::pin(global_form::get(self.clone(), form_id.into_inner(), query))
    }
    fn global_form_state(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::FormStateResponseDto,
        grapt_api::u3a::FormLookupApplicationError,
    > {
        Box::pin(global_form::state(
            self.clone(),
            form_id.into_inner(),
            query,
        ))
    }
    fn global_form_reply(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, grapt_api::u3a::FormReplyApplicationError> {
        Box::pin(global_form::reply(
            self.clone(),
            form_id.into_inner(),
            query,
            request,
        ))
    }
    fn global_form_cancel(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, grapt_api::u3a::FormCancelApplicationError> {
        Box::pin(global_form::cancel(
            self.clone(),
            form_id.into_inner(),
            query,
        ))
    }

    fn session_form_create(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::FormCreateRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::FormResponseDto, FormCreateApplicationError> {
        form::create_and_notify(self, session_id.into_inner(), request)
    }

    fn session_form_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<grapt_api::u3a::FormResponseDto, FormLookupApplicationError> {
        ready(form::get(
            self,
            session_id.into_inner(),
            form_id.into_inner(),
        ))
    }

    fn session_form_state(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<grapt_api::u3a::FormStateResponseDto, FormLookupApplicationError> {
        ready(form::state(
            self,
            session_id.into_inner(),
            form_id.into_inner(),
        ))
    }

    fn session_form_reply(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
        request: grapt_api::u3a::FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, FormReplyApplicationError> {
        ready(form::reply(
            self,
            session_id.into_inner(),
            form_id.into_inner(),
            request,
        ))
    }

    fn session_form_cancel(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<NoContent, FormCancelApplicationError> {
        ready(form::cancel(
            self,
            session_id.into_inner(),
            form_id.into_inner(),
        ))
    }

    fn session_pending_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionPendingListResponseDto,
        SessionLookupApplicationError,
    > {
        ready(self.pending_list(session_id))
    }

    fn shell_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::ShellListResponseDto, InvalidRequestErrorDto> {
        let application = self.clone();
        Box::pin(async move {
            let location = locations::resolve(&application.catalog_snapshot(), &query)
                .await
                .map_err(invalid)?;
            // The route stays for the desktop renderer's route contract, which
            // lists GET /api/shell. Nothing populates a durable shell inventory,
            // so the answer is an empty scoped list and no store is read.
            let data: Vec<grapt_api::u3a::ShellInfoDto> = Vec::new();
            decode(json!({"location": location.wire(), "data":data})).map_err(invalid)
        })
    }
    fn session_capability_put(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionCapabilityPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(context::capability(
            self,
            principal,
            session_id.into_inner(),
            request,
        ))
    }
    fn instruction_entry_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<grapt_api::u3a::InstructionEntryListDto, SessionLookupApplicationError>
    {
        ready(context::entries(self, session_id.into_inner()))
    }
    fn instruction_inspect(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<grapt_api::u3a::InstructionInspectionDto, SessionLookupApplicationError>
    {
        ready(context::inspect(self, session_id.into_inner()))
    }
    fn instruction_entry_put(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        key: grapt_api::u3a::InstructionEntryKeyDto,
        request: grapt_api::u3a::InstructionEntryPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(context::put(self, session_id.into_inner(), key, request))
    }
    fn instruction_entry_remove(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        key: grapt_api::u3a::InstructionEntryKeyDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(context::remove(self, session_id.into_inner(), key))
    }
    fn integration_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::IntegrationListResponseDto, InvalidRequestErrorDto> {
        connections::integrations(self, query)
    }

    fn plugin_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<PluginListResponseDto, InvalidRequestErrorDto> {
        plugins::list(self, query)
    }

    fn workspace_diff(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::WorkspaceDiffResponseDto, InvalidRequestErrorDto> {
        workspace_diff::response(Arc::clone(&self.catalog_snapshot()), query)
    }

    fn workspace_worktree(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::WorkspaceWorktreeRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::WorkspaceWorktreeResponseDto, InvalidRequestErrorDto>
    {
        workspace_worktree::response(self.clone(), query, request)
    }

    fn hook_import(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::HookImportRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::HookImportResponseDto, InvalidRequestErrorDto> {
        hook_import::request(self.clone(), query, request)
    }

    fn mcp_inspect(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpInspectionResponseDto, InvalidRequestErrorDto> {
        connections::inspect(self, query)
    }

    fn mcp_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpListResponseDto, InvalidRequestErrorDto> {
        connections::response(self.clone(), query, connections::Kind::McpServers)
    }

    fn mcp_configuration_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpConfigurationListResponseDto, InvalidRequestErrorDto>
    {
        connections::configuration(self, query)
    }

    fn mcp_add(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::McpAddRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        connections::add_runtime(self.clone(), server, query, request)
    }

    fn mcp_approval(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpApprovalPreviewDto, InvalidRequestErrorDto> {
        mcp_config::preview(self.clone(), server, query)
    }

    fn mcp_configuration(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::McpConfigurationRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        mcp_config::request(self.clone(), server, query, request)
    }

    fn mcp_reconnect(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        connections::reconnect(self.clone(), server, query)
    }

    fn mcp_auth(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::McpAuthRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpAuthResponseDto, InvalidRequestErrorDto> {
        mcp::auth_api::respond(self.clone(), server, query, request)
    }

    fn mcp_content(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::McpContentRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpContentResponseDto, InvalidRequestErrorDto> {
        mcp_content::respond(self.clone(), server, query, request)
    }

    fn mcp_resources(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpResourcesResponseDto, InvalidRequestErrorDto> {
        mcp_content::all_resources(self.clone(), query)
    }

    fn question_request_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::QuestionListResponseDto, InvalidRequestErrorDto> {
        let catalog = Arc::clone(&self.catalog_snapshot());
        Box::pin(async move {
            let location = locations::resolve(&catalog, &query)
                .await
                .map_err(invalid)?;
            // Rust emits forms, not legacy question events. Do not turn forms
            // into questions or hide an invalid/unreadable location as empty.
            decode(json!({"location": location.wire(), "data": []})).map_err(invalid)
        })
    }

    fn form_request_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::FormListResponseDto, InvalidRequestErrorDto> {
        form_catalog::list(self.clone(), query)
    }

    fn command_inspect(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::CommandInspectionResponseDto, InvalidRequestErrorDto>
    {
        catalogs::command_response(self.clone(), query, true)
    }

    fn command_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::CommandListResponseDto, InvalidRequestErrorDto> {
        catalogs::command_response(self.clone(), query, false)
    }

    fn skill_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::SkillListResponseDto, InvalidRequestErrorDto> {
        catalogs::transformed_response(
            Arc::clone(&self.catalog_snapshot()),
            Arc::clone(&self.plugins),
            query,
            catalogs::Kind::Skills,
        )
    }

    fn reference_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::ReferenceListResponseDto, InvalidRequestErrorDto> {
        catalogs::transformed_response(
            Arc::clone(&self.catalog_snapshot()),
            Arc::clone(&self.plugins),
            query,
            catalogs::Kind::References,
        )
    }

    fn location_get(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<LocationInfoDto, InvalidRequestErrorDto> {
        let catalog = Arc::clone(&self.catalog_snapshot());
        Box::pin(async move {
            let resolved = locations::resolve(&catalog, &query)
                .await
                .map_err(invalid)?;
            decode(resolved.wire()).map_err(invalid)
        })
    }

    fn project_list(
        &self,
        _principal: SealedPrincipal,
    ) -> ApplicationFuture<ProjectListResponseDto, InvalidRequestErrorDto> {
        ready(projects::list(self).map_err(invalid))
    }

    fn project_current(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<ProjectCurrentDto, InvalidRequestErrorDto> {
        let application = self.clone();
        Box::pin(async move {
            let resolved = locations::resolve_for(&application, &query)
                .await
                .map_err(invalid)?;
            decode(
                json!({"id": resolved.project.as_str(), "directory": resolved.project_directory}),
            )
            .map_err(invalid)
        })
    }

    fn provider_list(
        &self,
        _principal: SealedPrincipal,
        query: ProviderLocationQueryDto,
    ) -> ApplicationFuture<ProviderListResponseDto, CatalogApplicationError> {
        let application = self.clone();
        let plugins = Arc::clone(&self.plugins);
        let draft = match plugins::catalog_draft(self) {
            Ok(draft) => draft,
            Err(error) => {
                return ready(Err(CatalogApplicationError::InvalidRequest(invalid(error))));
            }
        };
        Box::pin(async move {
            let query = serde_json::to_value(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let query = decode(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let location = locations::resolve_for(&application, &query)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let transformed = plugins
                .transform_current("catalog", draft)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let data = transformed["providers"]
                .as_array()
                .ok_or_else(|| {
                    CatalogApplicationError::InvalidRequest(invalid(
                        "plugin catalog transform removed the provider list",
                    ))
                })?
                .iter()
                .map(|record| record["provider"].clone())
                .collect::<Vec<_>>();
            decode(json!({"location":location.wire(),"data":data}))
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
        })
    }

    fn model_list(
        &self,
        _principal: SealedPrincipal,
        query: ModelLocationQueryDto,
    ) -> ApplicationFuture<ModelListResponseDto, CatalogApplicationError> {
        let plugins = Arc::clone(&self.plugins);
        let draft = match plugins::catalog_draft(self) {
            Ok(draft) => draft,
            Err(error) => {
                return ready(Err(CatalogApplicationError::InvalidRequest(invalid(error))));
            }
        };
        let application = self.clone();
        Box::pin(async move {
            let query = serde_json::to_value(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let query = decode(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let location = locations::resolve_for(&application, &query)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let transformed = plugins
                .transform_current("catalog", draft)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let data = transformed["providers"]
                .as_array()
                .ok_or_else(|| {
                    CatalogApplicationError::InvalidRequest(invalid(
                        "plugin catalog transform removed the provider list",
                    ))
                })?
                .iter()
                .flat_map(|record| record["models"].as_array().into_iter().flatten().cloned())
                .collect::<Vec<_>>();
            decode(json!({"location":location.wire(),"data":data}))
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
        })
    }

    fn model_default(
        &self,
        _principal: SealedPrincipal,
        query: ModelDefaultLocationQueryDto,
    ) -> ApplicationFuture<ModelDefaultResponseDto, CatalogApplicationError> {
        let plugins = Arc::clone(&self.plugins);
        let draft = match plugins::catalog_draft(self) {
            Ok(draft) => draft,
            Err(error) => {
                return ready(Err(CatalogApplicationError::InvalidRequest(invalid(error))));
            }
        };
        let application = self.clone();
        Box::pin(async move {
            let query = serde_json::to_value(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let query = decode(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let location = locations::resolve_for(&application, &query)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let transformed = plugins
                .transform_current("catalog", draft)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let selected = transformed.get("default").cloned().unwrap_or(Value::Null);
            let provider = selected.get("providerID").and_then(Value::as_str);
            let model = selected.get("modelID").and_then(Value::as_str);
            let data = transformed["providers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|record| record["provider"]["id"].as_str() == provider)
                .flat_map(|record| record["models"].as_array().into_iter().flatten())
                .find(|item| item["id"].as_str() == model)
                .cloned()
                .unwrap_or(Value::Null);
            decode(json!({"location":location.wire(),"data":data}))
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
        })
    }

    fn agent_list(
        &self,
        _principal: SealedPrincipal,
        query: AgentLocationQueryDto,
    ) -> ApplicationFuture<AgentListResponseDto, InvalidRequestErrorDto> {
        let plugins = Arc::clone(&self.plugins);
        let application = self.clone();
        Box::pin(async move {
            let query = serde_json::to_value(query).map_err(invalid)?;
            let query = decode(query).map_err(invalid)?;
            let location = locations::resolve_for(&application, &query)
                .await
                .map_err(invalid)?;
            let agents = selection::agent_inventory(
                &application.catalog_snapshot(),
                plugins.service_root(),
                Path::new(&location.directory),
            )
            .map_err(invalid)?;
            let transformed = plugins
                .transform_current("agent", json!({"items":agents,"default":null}))
                .await
                .map_err(invalid)?;
            let data = transformed
                .get("items")
                .cloned()
                .ok_or_else(|| invalid("plugin agent transform removed the item list"))?;
            decode(json!({"location":location.wire(),"data":data})).map_err(invalid)
        })
    }

    fn session_list(
        &self,
        _principal: SealedPrincipal,
        query: SessionListQueryDto,
    ) -> ApplicationFuture<SessionsResponseDto, SessionListApplicationError> {
        self.read_projection(
            move |application| list::response(application, query),
            |_| SessionListApplicationError::Unavailable,
        )
    }

    fn session_active(
        &self,
        _principal: SealedPrincipal,
    ) -> ApplicationFuture<SessionActiveResponseDto, grapt_api::u3a::SessionActiveApplicationError>
    {
        self.read_projection(
            // Both the projection's own refusal and the reader wait go through one
            // classifier, so a busy wait cannot become a bad request on either path.
            |application| {
                application
                    .active_session_response()
                    .map_err(|detail| active_read_failure(&detail))
            },
            |detail| active_read_failure(&detail),
        )
    }

    fn session_create(
        &self,
        _principal: SealedPrincipal,
        request: SessionCreateRequestDto,
        launch: Option<grapt_types::SessionLaunch>,
    ) -> ApplicationFuture<SessionResponseDto, InvalidRequestErrorDto> {
        let application = self.clone();
        Box::pin(async move {
            let requested_id = request.id().as_value().map(|id| id.as_inner().clone());
            let request = serde_json::to_value(request).map_err(invalid)?;
            let prepared = application
                .prepare_create(&request, requested_id, launch)
                .await?;
            let created = {
                let authorities =
                    DeterministicAuthorities::new(&*application.clock, &*application.ids);
                SessionPorts::new(Arc::clone(&application.inbox), &authorities)
                    .create(&prepared.seed)
                    .map_err(invalid)?
            };
            let record = application.get_record(&created.session).map_err(invalid)?;
            sandbox_worktree_config::observe(
                &application,
                &created.session,
                Path::new(record.placement.directory()),
            )
            .await;
            application.session_response(&record)
        })
    }

    fn session_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
        self.read_session(session_id.into_inner())
    }

    fn session_sandbox_worktree_config_get(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SandboxWorktreeConfigResponseDto,
        SessionLookupApplicationError,
    > {
        sandbox_worktree_config::get(self.clone(), session.into_inner())
    }

    fn session_sandbox_worktree_config_create(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
        request: grapt_api::u3a::SandboxWorktreeConfigRequestDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::SandboxWorktreeConfigResponseDto,
        SessionLookupApplicationError,
    > {
        sandbox_worktree_config::create(self.clone(), session.into_inner(), request)
    }

    fn session_delete(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(context::delete_session(self, session_id.into_inner()))
    }

    fn side_question(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SideQuestionRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::SideQuestionResponseDto, SessionLookupApplicationError>
    {
        self.answer_side_question(session_id.into_inner(), request)
    }

    fn session_fork(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionForkRequestDto,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
        ready(self.fork_session_request(session_id.into_inner(), request))
    }

    fn session_messages(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        query: MessageListQueryDto,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
        self.session_messages_view(
            principal,
            session_id,
            query,
            grapt_api::u3a::MessageListView::Durable,
        )
    }

    fn session_messages_view(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        query: MessageListQueryDto,
        view: grapt_api::u3a::MessageListView,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
        self.read_projection(
            move |application| {
                history::messages_get(application, &session_id.into_inner(), query, view)
            },
            |_| SessionMessagesApplicationError::Unavailable,
        )
    }

    fn session_message(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        message_id: WireId<MessageId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionMessageResponseDto,
        grapt_api::u3a::SessionMessageApplicationError,
    > {
        self.read_projection(
            move |application| {
                history::message_get(
                    application,
                    &session_id.into_inner(),
                    &message_id.into_inner(),
                )
            },
            |_| grapt_api::u3a::SessionMessageApplicationError::Unavailable,
        )
    }

    fn session_prompt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PromptRequestDto,
    ) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError> {
        self.submit_prompt(principal, session_id, request)
    }

    fn session_interrupt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: InterruptRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        self.interrupt_with_pending(principal, session_id, request)
    }

    fn session_compact(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::CompactRequestDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::CompactResponseDto,
        grapt_api::u3a::SessionCompactApplicationError,
    > {
        ready(self.submit_compact(session_id, request))
    }

    fn permission_list(
        &self,
        principal: SealedPrincipal,
        query: PermissionLocationQueryDto,
    ) -> ApplicationFuture<PermissionGlobalListResponseDto, PermissionApplicationError> {
        permission::list(self, principal, query)
    }

    fn session_permission_create(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PermissionCreateRequestDto,
    ) -> ApplicationFuture<PermissionCreateResponseDto, PermissionApplicationError> {
        permission::create(self, principal, session_id, request)
    }

    fn session_permission_list(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<PermissionListResponseDto, PermissionApplicationError> {
        permission::session_list(self, principal, session_id)
    }

    fn session_permission_get(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request_id: WireId<PermissionId>,
    ) -> ApplicationFuture<PermissionResponseDto, PermissionApplicationError> {
        permission::get(self, principal, session_id, request_id)
    }

    fn session_permission_reply(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request_id: WireId<PermissionId>,
        request: PermissionReplyRequestDto,
    ) -> ApplicationFuture<NoContent, PermissionApplicationError> {
        permission::reply(self, principal, session_id, request_id, request)
    }
}

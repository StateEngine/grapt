use grapt_api::u3a::{
    AgentListResponseDto, AgentLocationQueryDto, ApplicationFuture, ApplicationPort,
    CatalogApplicationError, CommandInspectionResponseDto, CommandListResponseDto,
    CompactRequestDto, CompactResponseDto, FormCancelApplicationError, FormCreateApplicationError,
    FormCreateRequestDto, FormListResponseDto, FormLookupApplicationError,
    FormReplyApplicationError, FormReplyRequestDto, FormResponseDto, FormStateResponseDto,
    HookImportRequestDto, HookImportResponseDto, InstructionEntryKeyDto, InstructionEntryListDto,
    InstructionEntryPutDto, InstructionInspectionDto, IntegrationListResponseDto,
    InterruptRequestDto, InvalidRequestErrorDto, LocationInfoDto, McpAddRequestDto,
    McpApprovalPreviewDto, McpAuthRequestDto, McpAuthResponseDto, McpConfigurationListResponseDto,
    McpConfigurationRequestDto, McpContentRequestDto, McpContentResponseDto,
    McpInspectionResponseDto, McpListResponseDto, McpResourcesResponseDto, MessageListQueryDto,
    MessageListView, ModelDefaultLocationQueryDto, ModelDefaultResponseDto, ModelListResponseDto,
    ModelLocationQueryDto, NoContent, PermissionApplicationError, PermissionCreateRequestDto,
    PermissionCreateResponseDto, PermissionGlobalListResponseDto, PermissionListResponseDto,
    PermissionLocationQueryDto, PermissionReplyRequestDto, PermissionResponseDto,
    PermissionSavedListResponseDto, PluginListResponseDto, ProjectCurrentDto,
    ProjectCurrentLocationQueryDto, ProjectListResponseDto, PromptRequestDto, PromptResponseDto,
    ProviderListResponseDto, ProviderLocationQueryDto, QuestionListResponseDto,
    ReferenceListResponseDto, SandboxGrantDto, SandboxGrantListResponseDto,
    SandboxGrantRevokeResponseDto, SandboxWorktreeConfigRequestDto,
    SandboxWorktreeConfigResponseDto, SessionActiveApplicationError, SessionActiveResponseDto,
    SessionAgentSelectDto, SessionCapabilityPutDto, SessionCompactApplicationError,
    SessionCreateRequestDto, SessionForkRequestDto, SessionFormListResponseDto,
    SessionListApplicationError, SessionListQueryDto, SessionLookupApplicationError,
    SessionMessageApplicationError, SessionMessageResponseDto, SessionMessagesApplicationError,
    SessionMessagesResponseDto, SessionModelSelectDto, SessionPendingListResponseDto,
    SessionPresetResponseDto, SessionPresetSelectDto, SessionPromptApplicationError,
    SessionRenameRequestDto, SessionResponseDto, SessionSubagentModelResponseDto,
    SessionsResponseDto, ShellListResponseDto, SideQuestionRequestDto, SideQuestionResponseDto,
    SkillListResponseDto, WireId, WorkspaceDiffResponseDto, WorkspaceWorktreeRequestDto,
    WorkspaceWorktreeResponseDto,
};
use grapt_api::SealedPrincipal;
use grapt_types::{FormId, MessageId, PermissionId, ProjectId, SessionId, SessionLaunch};

use crate::dogfood::ServerApplication;

mod catalog;
mod connections;
mod forms;
mod permissions;
mod sessions;

impl ApplicationPort for ServerApplication {
    fn session_rename(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SessionRenameRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_rename(self, session_id, request)
    }

    fn session_agent_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SessionAgentSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_agent_select(self, session_id, request)
    }

    fn session_model_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_model_select(self, session_id, request)
    }

    fn session_preset_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<SessionPresetResponseDto, SessionLookupApplicationError> {
        sessions::session_preset_get(self, session_id)
    }

    fn session_preset_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SessionPresetSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_preset_select(self, session_id, request)
    }

    fn session_subagent_model_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<SessionSubagentModelResponseDto, SessionLookupApplicationError> {
        sessions::session_subagent_model_get(self, session_id)
    }

    fn session_subagent_model_set(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_subagent_model_set(self, session_id, Some(request))
    }

    fn session_subagent_model_clear(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_subagent_model_set(self, session_id, None)
    }

    fn sandbox_grant_list(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
    ) -> ApplicationFuture<SandboxGrantListResponseDto, PermissionApplicationError> {
        permissions::sandbox_grant_list(self, session)
    }

    fn sandbox_grant_revoke(
        &self,
        principal: SealedPrincipal,
        session: WireId<SessionId>,
        request: SandboxGrantDto,
    ) -> ApplicationFuture<SandboxGrantRevokeResponseDto, PermissionApplicationError> {
        permissions::sandbox_grant_revoke(self, principal, session, request)
    }

    fn permission_saved_list(
        &self,
        _principal: SealedPrincipal,
        project: Option<WireId<ProjectId>>,
    ) -> ApplicationFuture<PermissionSavedListResponseDto, InvalidRequestErrorDto> {
        permissions::permission_saved_list(self, project)
    }

    fn session_form_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<SessionFormListResponseDto, SessionLookupApplicationError> {
        forms::session_form_list(self, session_id)
    }

    fn global_form_get(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<FormResponseDto, FormLookupApplicationError> {
        forms::global_form_get(self, form_id, query)
    }

    fn global_form_state(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<FormStateResponseDto, FormLookupApplicationError> {
        forms::global_form_state(self, form_id, query)
    }

    fn global_form_reply(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<FormId>,
        query: ProjectCurrentLocationQueryDto,
        request: FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, FormReplyApplicationError> {
        forms::global_form_reply(self, form_id, query, request)
    }

    fn global_form_cancel(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, FormCancelApplicationError> {
        forms::global_form_cancel(self, form_id, query)
    }

    fn session_form_create(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: FormCreateRequestDto,
    ) -> ApplicationFuture<FormResponseDto, FormCreateApplicationError> {
        forms::session_form_create(self, session_id, request)
    }

    fn session_form_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<FormResponseDto, FormLookupApplicationError> {
        forms::session_form_get(self, session_id, form_id)
    }

    fn session_form_state(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<FormStateResponseDto, FormLookupApplicationError> {
        forms::session_form_state(self, session_id, form_id)
    }

    fn session_form_reply(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
        request: FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, FormReplyApplicationError> {
        forms::session_form_reply(self, session_id, form_id, request)
    }

    fn session_form_cancel(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<NoContent, FormCancelApplicationError> {
        forms::session_form_cancel(self, session_id, form_id)
    }

    fn session_pending_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<SessionPendingListResponseDto, SessionLookupApplicationError> {
        sessions::session_pending_list(self, session_id)
    }

    fn shell_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<ShellListResponseDto, InvalidRequestErrorDto> {
        catalog::shell_list(self, query)
    }

    fn session_capability_put(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SessionCapabilityPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_capability_put(self, principal, session_id, request)
    }

    fn instruction_entry_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<InstructionEntryListDto, SessionLookupApplicationError> {
        sessions::instruction_entry_list(self, session_id)
    }

    fn instruction_inspect(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<InstructionInspectionDto, SessionLookupApplicationError> {
        sessions::instruction_inspect(self, session_id)
    }

    fn instruction_entry_put(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        key: InstructionEntryKeyDto,
        request: InstructionEntryPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::instruction_entry_put(self, session_id, key, request)
    }

    fn instruction_entry_remove(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        key: InstructionEntryKeyDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::instruction_entry_remove(self, session_id, key)
    }

    fn integration_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<IntegrationListResponseDto, InvalidRequestErrorDto> {
        connections::integration_list(self, query)
    }

    fn plugin_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<PluginListResponseDto, InvalidRequestErrorDto> {
        catalog::plugin_list(self, query)
    }

    fn workspace_diff(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<WorkspaceDiffResponseDto, InvalidRequestErrorDto> {
        catalog::workspace_diff(self, query)
    }

    fn workspace_worktree(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: WorkspaceWorktreeRequestDto,
    ) -> ApplicationFuture<WorkspaceWorktreeResponseDto, InvalidRequestErrorDto> {
        catalog::workspace_worktree(self, query, request)
    }

    fn hook_import(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: HookImportRequestDto,
    ) -> ApplicationFuture<HookImportResponseDto, InvalidRequestErrorDto> {
        catalog::hook_import(self, query, request)
    }

    fn mcp_inspect(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<McpInspectionResponseDto, InvalidRequestErrorDto> {
        connections::mcp_inspect(self, query)
    }

    fn mcp_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<McpListResponseDto, InvalidRequestErrorDto> {
        connections::mcp_list(self, query)
    }

    fn mcp_configuration_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<McpConfigurationListResponseDto, InvalidRequestErrorDto> {
        connections::mcp_configuration_list(self, query)
    }

    fn mcp_add(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: McpAddRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        connections::mcp_add(self, server, query, request)
    }

    fn mcp_approval(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<McpApprovalPreviewDto, InvalidRequestErrorDto> {
        connections::mcp_approval(self, server, query)
    }

    fn mcp_configuration(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: McpConfigurationRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        connections::mcp_configuration(self, server, query, request)
    }

    fn mcp_reconnect(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        connections::mcp_reconnect(self, server, query)
    }

    fn mcp_auth(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: McpAuthRequestDto,
    ) -> ApplicationFuture<McpAuthResponseDto, InvalidRequestErrorDto> {
        connections::mcp_auth(self, server, query, request)
    }

    fn mcp_content(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: McpContentRequestDto,
    ) -> ApplicationFuture<McpContentResponseDto, InvalidRequestErrorDto> {
        connections::mcp_content(self, server, query, request)
    }

    fn mcp_resources(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<McpResourcesResponseDto, InvalidRequestErrorDto> {
        connections::mcp_resources(self, query)
    }

    fn question_request_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<QuestionListResponseDto, InvalidRequestErrorDto> {
        catalog::question_request_list(self, query)
    }

    fn form_request_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<FormListResponseDto, InvalidRequestErrorDto> {
        catalog::form_request_list(self, query)
    }

    fn command_inspect(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<CommandInspectionResponseDto, InvalidRequestErrorDto> {
        catalog::command_inspect(self, query)
    }

    fn command_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<CommandListResponseDto, InvalidRequestErrorDto> {
        catalog::command_list(self, query)
    }

    fn skill_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<SkillListResponseDto, InvalidRequestErrorDto> {
        catalog::skill_list(self, query)
    }

    fn reference_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<ReferenceListResponseDto, InvalidRequestErrorDto> {
        catalog::reference_list(self, query)
    }

    fn location_get(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<LocationInfoDto, InvalidRequestErrorDto> {
        catalog::location_get(self, query)
    }

    fn project_list(
        &self,
        _principal: SealedPrincipal,
    ) -> ApplicationFuture<ProjectListResponseDto, InvalidRequestErrorDto> {
        catalog::project_list(self)
    }

    fn project_current(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<ProjectCurrentDto, InvalidRequestErrorDto> {
        catalog::project_current(self, query)
    }

    fn provider_list(
        &self,
        _principal: SealedPrincipal,
        query: ProviderLocationQueryDto,
    ) -> ApplicationFuture<ProviderListResponseDto, CatalogApplicationError> {
        catalog::provider_list(self, query)
    }

    fn model_list(
        &self,
        _principal: SealedPrincipal,
        query: ModelLocationQueryDto,
    ) -> ApplicationFuture<ModelListResponseDto, CatalogApplicationError> {
        catalog::model_list(self, query)
    }

    fn model_default(
        &self,
        _principal: SealedPrincipal,
        query: ModelDefaultLocationQueryDto,
    ) -> ApplicationFuture<ModelDefaultResponseDto, CatalogApplicationError> {
        catalog::model_default(self, query)
    }

    fn agent_list(
        &self,
        _principal: SealedPrincipal,
        query: AgentLocationQueryDto,
    ) -> ApplicationFuture<AgentListResponseDto, InvalidRequestErrorDto> {
        catalog::agent_list(self, query)
    }

    fn session_list(
        &self,
        _principal: SealedPrincipal,
        query: SessionListQueryDto,
    ) -> ApplicationFuture<SessionsResponseDto, SessionListApplicationError> {
        sessions::session_list(self, query)
    }

    fn session_active(
        &self,
        _principal: SealedPrincipal,
    ) -> ApplicationFuture<SessionActiveResponseDto, SessionActiveApplicationError> {
        sessions::session_active(self)
    }

    fn session_create(
        &self,
        _principal: SealedPrincipal,
        request: SessionCreateRequestDto,
        launch: Option<SessionLaunch>,
    ) -> ApplicationFuture<SessionResponseDto, InvalidRequestErrorDto> {
        sessions::session_create(self, request, launch)
    }

    fn session_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
        sessions::session_get(self, session_id)
    }

    fn session_sandbox_worktree_config_get(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
    ) -> ApplicationFuture<SandboxWorktreeConfigResponseDto, SessionLookupApplicationError> {
        sessions::session_sandbox_worktree_config_get(self, session)
    }

    fn session_sandbox_worktree_config_create(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
        request: SandboxWorktreeConfigRequestDto,
    ) -> ApplicationFuture<SandboxWorktreeConfigResponseDto, SessionLookupApplicationError> {
        sessions::session_sandbox_worktree_config_create(self, session, request)
    }

    fn session_delete(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_delete(self, session_id)
    }

    fn side_question(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SideQuestionRequestDto,
    ) -> ApplicationFuture<SideQuestionResponseDto, SessionLookupApplicationError> {
        sessions::side_question(self, session_id, request)
    }

    fn session_fork(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SessionForkRequestDto,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
        sessions::session_fork(self, session_id, request)
    }

    fn session_messages(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        query: MessageListQueryDto,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
        self.session_messages_view(principal, session_id, query, MessageListView::Durable)
    }

    fn session_messages_view(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        query: MessageListQueryDto,
        view: MessageListView,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
        sessions::session_messages_view(self, session_id, query, view)
    }

    fn session_message(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        message_id: WireId<MessageId>,
    ) -> ApplicationFuture<SessionMessageResponseDto, SessionMessageApplicationError> {
        sessions::session_message(self, session_id, message_id)
    }

    fn session_prompt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PromptRequestDto,
    ) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError> {
        sessions::session_prompt(self, principal, session_id, request)
    }

    fn session_interrupt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: InterruptRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        sessions::session_interrupt(self, principal, session_id, request)
    }

    fn session_compact(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: CompactRequestDto,
    ) -> ApplicationFuture<CompactResponseDto, SessionCompactApplicationError> {
        sessions::session_compact(self, session_id, request)
    }

    fn permission_list(
        &self,
        principal: SealedPrincipal,
        query: PermissionLocationQueryDto,
    ) -> ApplicationFuture<PermissionGlobalListResponseDto, PermissionApplicationError> {
        permissions::permission_list(self, principal, query)
    }

    fn session_permission_create(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PermissionCreateRequestDto,
    ) -> ApplicationFuture<PermissionCreateResponseDto, PermissionApplicationError> {
        permissions::session_permission_create(self, principal, session_id, request)
    }

    fn session_permission_list(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<PermissionListResponseDto, PermissionApplicationError> {
        permissions::session_permission_list(self, principal, session_id)
    }

    fn session_permission_get(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request_id: WireId<PermissionId>,
    ) -> ApplicationFuture<PermissionResponseDto, PermissionApplicationError> {
        permissions::session_permission_get(self, principal, session_id, request_id)
    }

    fn session_permission_reply(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request_id: WireId<PermissionId>,
        request: PermissionReplyRequestDto,
    ) -> ApplicationFuture<NoContent, PermissionApplicationError> {
        permissions::session_permission_reply(self, principal, session_id, request_id, request)
    }
}

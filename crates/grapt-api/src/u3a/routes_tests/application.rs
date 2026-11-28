//! Transport-only application fixture. Any dispatch is a witness failure.
use super::*;
use crate::u3a::SessionActiveApplicationError;

#[derive(Debug)]
struct NeverApplication;

impl crate::u3a::HostControlApplicationPort for NeverApplication {}
impl crate::u3a::SessionSnapshotApplicationPort for NeverApplication {}

macro_rules! never {
    () => {
        panic!("authenticated application was called by a transport-only witness")
    };
}

impl ApplicationPort for NeverApplication {
    fn global_form_get(
        &self,
        _principal: SealedPrincipal,
        _form_id: WireId<grapt_types::FormId>,
        _query: crate::u3a::ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::FormResponseDto, FormLookupApplicationError> {
        never!()
    }
    fn global_form_state(
        &self,
        _principal: SealedPrincipal,
        _form_id: WireId<grapt_types::FormId>,
        _query: crate::u3a::ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::FormStateResponseDto, FormLookupApplicationError> {
        never!()
    }
    fn global_form_reply(
        &self,
        _principal: SealedPrincipal,
        _form_id: WireId<grapt_types::FormId>,
        _query: crate::u3a::ProjectCurrentLocationQueryDto,
        _request: crate::u3a::FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, FormReplyApplicationError> {
        never!()
    }
    fn global_form_cancel(
        &self,
        _principal: SealedPrincipal,
        _form_id: WireId<grapt_types::FormId>,
        _query: crate::u3a::ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, FormCancelApplicationError> {
        never!()
    }
    fn session_rename(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionRenameRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }

    fn session_agent_select(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionAgentSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }

    fn session_model_select(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }

    fn session_preset_get(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<crate::u3a::SessionPresetResponseDto, SessionLookupApplicationError>
    {
        never!()
    }

    fn session_preset_select(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionPresetSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }
    fn session_subagent_model_get(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<crate::u3a::SessionSubagentModelResponseDto, SessionLookupApplicationError>
    {
        never!()
    }
    fn session_subagent_model_set(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }
    fn session_subagent_model_clear(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }
    fn session_capability_put(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionCapabilityPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }
    fn instruction_entry_list(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<crate::u3a::InstructionEntryListDto, SessionLookupApplicationError> {
        never!()
    }
    fn instruction_entry_put(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _key: crate::u3a::InstructionEntryKeyDto,
        _request: crate::u3a::InstructionEntryPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }
    fn instruction_inspect(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<crate::u3a::InstructionInspectionDto, SessionLookupApplicationError>
    {
        never!()
    }
    fn instruction_entry_remove(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _key: crate::u3a::InstructionEntryKeyDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }
    fn integration_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::IntegrationListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn plugin_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::PluginListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn workspace_diff(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::WorkspaceDiffResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn workspace_worktree(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
        _request: crate::u3a::WorkspaceWorktreeRequestDto,
    ) -> ApplicationFuture<crate::u3a::WorkspaceWorktreeResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn hook_import(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
        _request: crate::u3a::HookImportRequestDto,
    ) -> ApplicationFuture<crate::u3a::HookImportResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn mcp_inspect(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::McpInspectionResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn mcp_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::McpListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn mcp_configuration_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::McpConfigurationListResponseDto, InvalidRequestErrorDto>
    {
        never!()
    }

    fn mcp_add(
        &self,
        _principal: SealedPrincipal,
        _server: String,
        _query: ProjectCurrentLocationQueryDto,
        _request: crate::u3a::McpAddRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        never!()
    }

    fn mcp_approval(
        &self,
        _principal: SealedPrincipal,
        _server: String,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::McpApprovalPreviewDto, InvalidRequestErrorDto> {
        never!()
    }

    fn mcp_configuration(
        &self,
        _principal: SealedPrincipal,
        _server: String,
        _query: ProjectCurrentLocationQueryDto,
        _request: crate::u3a::McpConfigurationRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        never!()
    }

    fn mcp_reconnect(
        &self,
        _principal: SealedPrincipal,
        _server: String,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        never!()
    }
    fn mcp_auth(
        &self,
        _principal: SealedPrincipal,
        _server: String,
        _query: ProjectCurrentLocationQueryDto,
        _request: crate::u3a::McpAuthRequestDto,
    ) -> ApplicationFuture<crate::u3a::McpAuthResponseDto, InvalidRequestErrorDto> {
        never!()
    }
    fn mcp_content(
        &self,
        _principal: SealedPrincipal,
        _server: String,
        _query: ProjectCurrentLocationQueryDto,
        _request: crate::u3a::McpContentRequestDto,
    ) -> ApplicationFuture<crate::u3a::McpContentResponseDto, InvalidRequestErrorDto> {
        never!()
    }
    fn mcp_resources(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::McpResourcesResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn session_form_list(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<crate::u3a::SessionFormListResponseDto, SessionLookupApplicationError>
    {
        never!()
    }

    fn session_form_create(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::FormCreateRequestDto,
    ) -> ApplicationFuture<crate::u3a::FormResponseDto, FormCreateApplicationError> {
        never!()
    }

    fn session_form_get(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _form_id: WireId<grapt_types::FormId>,
    ) -> ApplicationFuture<crate::u3a::FormResponseDto, FormLookupApplicationError> {
        never!()
    }

    fn session_form_state(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _form_id: WireId<grapt_types::FormId>,
    ) -> ApplicationFuture<crate::u3a::FormStateResponseDto, FormLookupApplicationError> {
        never!()
    }

    fn session_form_reply(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _form_id: WireId<grapt_types::FormId>,
        _request: crate::u3a::FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, FormReplyApplicationError> {
        never!()
    }

    fn session_form_cancel(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _form_id: WireId<grapt_types::FormId>,
    ) -> ApplicationFuture<NoContent, FormCancelApplicationError> {
        never!()
    }

    fn session_pending_list(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<crate::u3a::SessionPendingListResponseDto, SessionLookupApplicationError>
    {
        never!()
    }

    fn sandbox_grant_list(
        &self,
        _principal: SealedPrincipal,
        _session: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<crate::u3a::SandboxGrantListResponseDto, PermissionApplicationError>
    {
        never!()
    }

    fn sandbox_grant_revoke(
        &self,
        _principal: SealedPrincipal,
        _session: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SandboxGrantDto,
    ) -> ApplicationFuture<crate::u3a::SandboxGrantRevokeResponseDto, PermissionApplicationError>
    {
        never!()
    }

    fn permission_saved_list(
        &self,
        _principal: SealedPrincipal,
        _project: Option<WireId<grapt_types::ProjectId>>,
    ) -> ApplicationFuture<crate::u3a::PermissionSavedListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn shell_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::ShellListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn form_request_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::FormListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn question_request_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::QuestionListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn reference_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::ReferenceListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn command_inspect(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::CommandInspectionResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn command_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::CommandListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn skill_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::SkillListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn location_get(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<crate::u3a::LocationInfoDto, InvalidRequestErrorDto> {
        never!()
    }

    fn project_list(
        &self,
        _principal: SealedPrincipal,
    ) -> ApplicationFuture<ProjectListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn project_current(
        &self,
        _principal: SealedPrincipal,
        _query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<ProjectCurrentDto, InvalidRequestErrorDto> {
        never!()
    }

    fn provider_list(
        &self,
        _principal: SealedPrincipal,
        _query: ProviderLocationQueryDto,
    ) -> ApplicationFuture<ProviderListResponseDto, CatalogApplicationError> {
        never!()
    }

    fn model_list(
        &self,
        _principal: SealedPrincipal,
        _query: ModelLocationQueryDto,
    ) -> ApplicationFuture<ModelListResponseDto, CatalogApplicationError> {
        never!()
    }

    fn model_default(
        &self,
        _principal: SealedPrincipal,
        _query: ModelDefaultLocationQueryDto,
    ) -> ApplicationFuture<ModelDefaultResponseDto, CatalogApplicationError> {
        never!()
    }

    fn agent_list(
        &self,
        _principal: SealedPrincipal,
        _query: AgentLocationQueryDto,
    ) -> ApplicationFuture<AgentListResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn session_list(
        &self,
        _principal: SealedPrincipal,
        _query: SessionListQueryDto,
    ) -> ApplicationFuture<SessionsResponseDto, SessionListApplicationError> {
        never!()
    }

    fn session_active(
        &self,
        _principal: SealedPrincipal,
    ) -> ApplicationFuture<SessionActiveResponseDto, SessionActiveApplicationError> {
        never!()
    }

    fn session_create(
        &self,
        _principal: SealedPrincipal,
        _request: SessionCreateRequestDto,
        _launch: Option<grapt_types::SessionLaunch>,
    ) -> ApplicationFuture<SessionResponseDto, InvalidRequestErrorDto> {
        never!()
    }

    fn session_get(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
        never!()
    }

    fn session_delete(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }

    fn side_question(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SideQuestionRequestDto,
    ) -> ApplicationFuture<crate::u3a::SideQuestionResponseDto, SessionLookupApplicationError> {
        never!()
    }

    fn session_fork(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionForkRequestDto,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
        never!()
    }

    fn session_messages(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _query: MessageListQueryDto,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
        never!()
    }

    fn session_prompt(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: PromptRequestDto,
    ) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError> {
        never!()
    }

    fn session_message(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _message_id: WireId<grapt_types::MessageId>,
    ) -> ApplicationFuture<
        crate::u3a::SessionMessageResponseDto,
        crate::u3a::SessionMessageApplicationError,
    > {
        never!()
    }

    fn session_interrupt(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: InterruptRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        never!()
    }

    fn session_compact(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::CompactRequestDto,
    ) -> ApplicationFuture<crate::u3a::CompactResponseDto, crate::u3a::SessionCompactApplicationError>
    {
        never!()
    }

    fn permission_list(
        &self,
        _principal: SealedPrincipal,
        _query: PermissionLocationQueryDto,
    ) -> ApplicationFuture<PermissionGlobalListResponseDto, PermissionApplicationError> {
        never!()
    }

    fn session_permission_create(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: PermissionCreateRequestDto,
    ) -> ApplicationFuture<PermissionCreateResponseDto, PermissionApplicationError> {
        never!()
    }

    fn session_permission_list(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<PermissionListResponseDto, PermissionApplicationError> {
        never!()
    }

    fn session_permission_get(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request_id: WireId<grapt_types::PermissionId>,
    ) -> ApplicationFuture<PermissionResponseDto, PermissionApplicationError> {
        never!()
    }

    fn session_permission_reply(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request_id: WireId<grapt_types::PermissionId>,
        _request: PermissionReplyRequestDto,
    ) -> ApplicationFuture<NoContent, PermissionApplicationError> {
        never!()
    }
}

pub(super) fn never_application() -> Arc<dyn ApplicationPort> {
    Arc::new(NeverApplication)
}

impl crate::u3a::SessionEditApplicationPort for NeverApplication {
    fn session_rewind_preview(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionRewindPreviewRequestDto,
    ) -> ApplicationFuture<
        crate::u3a::SessionRewindPreviewResponseDto,
        crate::u3a::SessionRewindApplicationError,
    > {
        never!()
    }

    fn session_rewind(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionRewindRequestDto,
    ) -> ApplicationFuture<
        crate::u3a::SessionRewindResponseDto,
        crate::u3a::SessionRewindApplicationError,
    > {
        never!()
    }

    fn session_undo_preview(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionUndoPreviewRequestDto,
    ) -> ApplicationFuture<
        crate::u3a::SessionUndoPreviewResponseDto,
        crate::u3a::SessionUndoApplicationError,
    > {
        never!()
    }

    fn session_undo(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionUndoRequestDto,
    ) -> ApplicationFuture<
        crate::u3a::SessionUndoResponseDto,
        crate::u3a::SessionUndoApplicationError,
    > {
        never!()
    }

    fn session_reconcile(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: crate::u3a::SessionReconcileRequestDto,
    ) -> ApplicationFuture<
        crate::u3a::SessionReconcileResponseDto,
        crate::u3a::SessionReconcileApplicationError,
    > {
        never!()
    }
}

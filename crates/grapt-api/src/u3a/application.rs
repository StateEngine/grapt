//! Typed seam between authenticated transport and externally owned product law.

use std::future::Future;
use std::pin::Pin;

use crate::SealedPrincipal;

mod outcome;
use outcome::sandbox_worktree_config_unsupported;
pub use outcome::*;

use super::{
    AgentListResponseDto, CompactRequestDto, CompactResponseDto, InterruptRequestDto,
    InvalidRequestErrorDto, LocationInfoDto, MessageListQueryDto, ModelDefaultLocationQueryDto,
    ModelDefaultResponseDto, ModelListResponseDto, ModelLocationQueryDto, NoContent,
    PermissionCreateRequestDto, PermissionCreateResponseDto, PermissionGlobalListResponseDto,
    PermissionListResponseDto, PermissionLocationQueryDto, PermissionReplyRequestDto,
    PermissionResponseDto, PluginListResponseDto, ProjectCurrentDto,
    ProjectCurrentLocationQueryDto, ProjectListResponseDto, PromptRequestDto, PromptResponseDto,
    ProviderListResponseDto, ProviderLocationQueryDto, SessionActiveResponseDto,
    SessionCreateRequestDto, SessionListQueryDto, SessionMessagesResponseDto, SessionResponseDto,
    SessionsResponseDto, WireId,
};

pub type ApplicationFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'static>>;

/// Durable history is stable across restart; live cache evidence is opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageListView {
    Durable,
    LiveCache,
}

#[derive(Debug)]
pub enum ApplicationRequest {
    HostControl(super::HostControlRequest),
    LocationGet(ProjectCurrentLocationQueryDto),
    ProjectList,
    ProjectCurrent(ProjectCurrentLocationQueryDto),
    ProviderList(ProviderLocationQueryDto),
    ModelList(ModelLocationQueryDto),
    ModelDefault(ModelDefaultLocationQueryDto),
    AgentList(super::AgentLocationQueryDto),
    CommandList(ProjectCurrentLocationQueryDto),
    CommandInspect(ProjectCurrentLocationQueryDto),
    SkillList(ProjectCurrentLocationQueryDto),
    ReferenceList(ProjectCurrentLocationQueryDto),
    IntegrationList(ProjectCurrentLocationQueryDto),
    PluginList(ProjectCurrentLocationQueryDto),
    WorkspaceDiff(ProjectCurrentLocationQueryDto),
    WorkspaceWorktree {
        query: ProjectCurrentLocationQueryDto,
        request: super::WorkspaceWorktreeRequestDto,
    },

    HookImport {
        query: ProjectCurrentLocationQueryDto,
        request: super::HookImportRequestDto,
    },
    McpInspect(ProjectCurrentLocationQueryDto),
    McpList(ProjectCurrentLocationQueryDto),
    McpConfigurationList(ProjectCurrentLocationQueryDto),
    McpAdd {
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: super::McpAddRequestDto,
    },
    McpConfiguration {
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: super::McpConfigurationRequestDto,
    },
    McpApproval {
        server: String,
        query: ProjectCurrentLocationQueryDto,
    },
    McpReconnect {
        server: String,
        query: ProjectCurrentLocationQueryDto,
    },
    McpAuth {
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: super::McpAuthRequestDto,
    },
    McpContent {
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: super::McpContentRequestDto,
    },
    McpResources(ProjectCurrentLocationQueryDto),
    FormRequestList(ProjectCurrentLocationQueryDto),
    QuestionRequestList(ProjectCurrentLocationQueryDto),
    ShellList(ProjectCurrentLocationQueryDto),
    PermissionSavedList(Option<WireId<grapt_types::ProjectId>>),
    SandboxGrantList(WireId<grapt_types::SessionId>),
    SandboxGrantRevoke {
        session: WireId<grapt_types::SessionId>,
        request: super::SandboxGrantDto,
    },
    SessionFormList(WireId<grapt_types::SessionId>),
    GlobalFormGet {
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    },
    GlobalFormState {
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    },
    GlobalFormReply {
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
        request: super::FormReplyRequestDto,
    },
    GlobalFormCancel {
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    },
    SessionFormCreate {
        session_id: WireId<grapt_types::SessionId>,
        request: super::FormCreateRequestDto,
    },
    SessionFormGet {
        session_id: WireId<grapt_types::SessionId>,
        form_id: WireId<grapt_types::FormId>,
    },
    SessionFormState {
        session_id: WireId<grapt_types::SessionId>,
        form_id: WireId<grapt_types::FormId>,
    },
    SessionFormReply {
        session_id: WireId<grapt_types::SessionId>,
        form_id: WireId<grapt_types::FormId>,
        request: super::FormReplyRequestDto,
    },
    SessionFormCancel {
        session_id: WireId<grapt_types::SessionId>,
        form_id: WireId<grapt_types::FormId>,
    },
    SessionPendingList(WireId<grapt_types::SessionId>),
    SessionList(SessionListQueryDto),
    SessionActive,
    SessionCreate {
        request: SessionCreateRequestDto,
        /// The launch mode the creating client stated, if it stated one.
        launch: Option<grapt_types::SessionLaunch>,
    },
    SessionGet(WireId<grapt_types::SessionId>),
    SessionDelete(WireId<grapt_types::SessionId>),
    SessionFork {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionForkRequestDto,
    },
    SideQuestion {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SideQuestionRequestDto,
    },
    SessionRename {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionRenameRequestDto,
    },
    SessionAgentSelect {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionAgentSelectDto,
    },
    SessionModelSelect {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionModelSelectDto,
    },
    SessionPresetSelect {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionPresetSelectDto,
    },
    SessionPresetGet(WireId<grapt_types::SessionId>),
    SessionSandboxWorktreeConfigGet(WireId<grapt_types::SessionId>),
    SessionSandboxWorktreeConfigCreate {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SandboxWorktreeConfigRequestDto,
    },
    SessionSubagentModelGet(WireId<grapt_types::SessionId>),
    SessionSubagentModelClear(WireId<grapt_types::SessionId>),
    SessionSubagentModelSet {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionModelSelectDto,
    },
    SessionCapabilityPut {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionCapabilityPutDto,
    },
    InstructionEntryList(WireId<grapt_types::SessionId>),
    InstructionInspect(WireId<grapt_types::SessionId>),
    InstructionEntryPut {
        session_id: WireId<grapt_types::SessionId>,
        key: super::InstructionEntryKeyDto,
        request: super::InstructionEntryPutDto,
    },
    InstructionEntryRemove {
        session_id: WireId<grapt_types::SessionId>,
        key: super::InstructionEntryKeyDto,
    },
    SessionMessages {
        session_id: WireId<grapt_types::SessionId>,
        query: MessageListQueryDto,
        view: MessageListView,
    },
    SessionMessage {
        session_id: WireId<grapt_types::SessionId>,
        message_id: WireId<grapt_types::MessageId>,
    },
    SessionSnapshot {
        session_id: WireId<grapt_types::SessionId>,
    },
    SessionPrompt {
        session_id: WireId<grapt_types::SessionId>,
        request: PromptRequestDto,
    },
    SessionInterrupt {
        session_id: WireId<grapt_types::SessionId>,
        request: InterruptRequestDto,
    },
    SessionRewindPreview {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionRewindPreviewRequestDto,
    },
    SessionRewind {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionRewindRequestDto,
    },
    SessionUndoPreview {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionUndoPreviewRequestDto,
    },
    SessionUndo {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionUndoRequestDto,
    },
    SessionReconcile {
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionReconcileRequestDto,
    },
    SessionCompact {
        session_id: WireId<grapt_types::SessionId>,
        request: CompactRequestDto,
    },
    PermissionList(PermissionLocationQueryDto),
    SessionPermissionCreate {
        session_id: WireId<grapt_types::SessionId>,
        request: PermissionCreateRequestDto,
    },
    SessionPermissionList(WireId<grapt_types::SessionId>),
    SessionPermissionGet {
        session_id: WireId<grapt_types::SessionId>,
        request_id: WireId<grapt_types::PermissionId>,
    },
    SessionPermissionReply {
        session_id: WireId<grapt_types::SessionId>,
        request_id: WireId<grapt_types::PermissionId>,
        request: PermissionReplyRequestDto,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationUnavailable {
    service: &'static str,
}

impl ApplicationUnavailable {
    pub fn try_new(service: &'static str) -> Result<Self, ApplicationUnavailableError> {
        if service.is_empty() {
            return Err(ApplicationUnavailableError::Empty);
        }
        if service.len() > 128 {
            return Err(ApplicationUnavailableError::TooLong);
        }
        Ok(Self { service })
    }

    pub fn service(self) -> &'static str {
        self.service
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationUnavailableError {
    Empty,
    TooLong,
}

/// Explicit native conversation and file recovery commands.
pub trait SessionEditApplicationPort {
    fn session_rewind_preview(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionRewindPreviewRequestDto,
    ) -> ApplicationFuture<super::SessionRewindPreviewResponseDto, SessionRewindApplicationError>;

    fn session_rewind(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionRewindRequestDto,
    ) -> ApplicationFuture<super::SessionRewindResponseDto, SessionRewindApplicationError>;

    fn session_undo_preview(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionUndoPreviewRequestDto,
    ) -> ApplicationFuture<super::SessionUndoPreviewResponseDto, SessionUndoApplicationError>;

    fn session_undo(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionUndoRequestDto,
    ) -> ApplicationFuture<super::SessionUndoResponseDto, SessionUndoApplicationError>;

    fn session_reconcile(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionReconcileRequestDto,
    ) -> ApplicationFuture<super::SessionReconcileResponseDto, SessionReconcileApplicationError>;
}

pub trait ApplicationPort:
    SessionEditApplicationPort
    + super::HostControlApplicationPort
    + super::SessionSnapshotApplicationPort
    + Send
    + Sync
    + 'static
{
    fn session_capability_put(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionCapabilityPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;
    fn instruction_entry_list(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<super::InstructionEntryListDto, SessionLookupApplicationError>;
    fn instruction_inspect(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<super::InstructionInspectionDto, SessionLookupApplicationError>;
    fn instruction_entry_put(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        key: super::InstructionEntryKeyDto,
        request: super::InstructionEntryPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;
    fn instruction_entry_remove(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        key: super::InstructionEntryKeyDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;
    fn integration_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::IntegrationListResponseDto, InvalidRequestErrorDto>;

    fn plugin_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<PluginListResponseDto, InvalidRequestErrorDto>;

    fn workspace_diff(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::WorkspaceDiffResponseDto, InvalidRequestErrorDto>;

    fn workspace_worktree(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: super::WorkspaceWorktreeRequestDto,
    ) -> ApplicationFuture<super::WorkspaceWorktreeResponseDto, InvalidRequestErrorDto>;

    fn hook_import(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: super::HookImportRequestDto,
    ) -> ApplicationFuture<super::HookImportResponseDto, InvalidRequestErrorDto>;

    fn mcp_inspect(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::McpInspectionResponseDto, InvalidRequestErrorDto>;

    fn mcp_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::McpListResponseDto, InvalidRequestErrorDto>;

    /// Configuration-only projection of the MCP catalog: declared names,
    /// their document scope and disabled state. Never touches the live
    /// registry, so the route starts no peer.
    fn mcp_configuration_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::McpConfigurationListResponseDto, InvalidRequestErrorDto>;

    fn mcp_add(
        &self,
        principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: super::McpAddRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;

    fn mcp_configuration(
        &self,
        principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: super::McpConfigurationRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;

    fn mcp_approval(
        &self,
        principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::McpApprovalPreviewDto, InvalidRequestErrorDto>;

    fn mcp_reconnect(
        &self,
        principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;

    fn mcp_auth(
        &self,
        principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: super::McpAuthRequestDto,
    ) -> ApplicationFuture<super::McpAuthResponseDto, InvalidRequestErrorDto>;

    fn mcp_content(
        &self,
        principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: super::McpContentRequestDto,
    ) -> ApplicationFuture<super::McpContentResponseDto, InvalidRequestErrorDto>;

    fn mcp_resources(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::McpResourcesResponseDto, InvalidRequestErrorDto>;

    fn session_form_list(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<super::SessionFormListResponseDto, SessionLookupApplicationError>;

    fn global_form_get(
        &self,
        principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::FormResponseDto, FormLookupApplicationError>;
    fn global_form_state(
        &self,
        principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::FormStateResponseDto, FormLookupApplicationError>;
    fn global_form_reply(
        &self,
        principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
        request: super::FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, FormReplyApplicationError>;
    fn global_form_cancel(
        &self,
        principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, FormCancelApplicationError>;

    fn session_form_create(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::FormCreateRequestDto,
    ) -> ApplicationFuture<super::FormResponseDto, FormCreateApplicationError>;

    fn session_form_get(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        form_id: WireId<grapt_types::FormId>,
    ) -> ApplicationFuture<super::FormResponseDto, FormLookupApplicationError>;

    fn session_form_state(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        form_id: WireId<grapt_types::FormId>,
    ) -> ApplicationFuture<super::FormStateResponseDto, FormLookupApplicationError>;

    fn session_form_reply(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        form_id: WireId<grapt_types::FormId>,
        request: super::FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, FormReplyApplicationError>;

    fn session_form_cancel(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        form_id: WireId<grapt_types::FormId>,
    ) -> ApplicationFuture<NoContent, FormCancelApplicationError>;

    fn session_rename(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionRenameRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;

    fn session_agent_select(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionAgentSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;

    fn session_model_select(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;

    /// Set the Session's permission preset (its trust level).
    fn session_preset_select(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionPresetSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;

    fn session_preset_get(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<super::SessionPresetResponseDto, SessionLookupApplicationError>;

    fn session_sandbox_worktree_config_get(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<super::SandboxWorktreeConfigResponseDto, SessionLookupApplicationError>
    {
        Box::pin(async { Err(sandbox_worktree_config_unsupported()) })
    }

    fn session_sandbox_worktree_config_create(
        &self,
        _principal: SealedPrincipal,
        _session_id: WireId<grapt_types::SessionId>,
        _request: super::SandboxWorktreeConfigRequestDto,
    ) -> ApplicationFuture<super::SandboxWorktreeConfigResponseDto, SessionLookupApplicationError>
    {
        Box::pin(async { Err(sandbox_worktree_config_unsupported()) })
    }

    fn session_subagent_model_get(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<super::SessionSubagentModelResponseDto, SessionLookupApplicationError>;

    fn session_subagent_model_set(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;

    fn session_subagent_model_clear(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;

    fn session_pending_list(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<super::SessionPendingListResponseDto, SessionLookupApplicationError>;

    fn sandbox_grant_list(
        &self,
        principal: SealedPrincipal,
        session: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<super::SandboxGrantListResponseDto, PermissionApplicationError>;

    fn sandbox_grant_revoke(
        &self,
        principal: SealedPrincipal,
        session: WireId<grapt_types::SessionId>,
        request: super::SandboxGrantDto,
    ) -> ApplicationFuture<super::SandboxGrantRevokeResponseDto, PermissionApplicationError>;

    fn permission_saved_list(
        &self,
        principal: SealedPrincipal,
        project: Option<WireId<grapt_types::ProjectId>>,
    ) -> ApplicationFuture<super::PermissionSavedListResponseDto, InvalidRequestErrorDto>;

    fn shell_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::ShellListResponseDto, InvalidRequestErrorDto>;

    fn form_request_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::FormListResponseDto, InvalidRequestErrorDto>;

    fn question_request_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::QuestionListResponseDto, InvalidRequestErrorDto>;

    fn reference_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::ReferenceListResponseDto, InvalidRequestErrorDto>;

    fn command_inspect(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::CommandInspectionResponseDto, InvalidRequestErrorDto>;

    fn command_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::CommandListResponseDto, InvalidRequestErrorDto>;

    fn skill_list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::SkillListResponseDto, InvalidRequestErrorDto>;

    fn location_get(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<LocationInfoDto, InvalidRequestErrorDto>;

    fn project_list(
        &self,
        principal: SealedPrincipal,
    ) -> ApplicationFuture<ProjectListResponseDto, InvalidRequestErrorDto>;

    fn project_current(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<ProjectCurrentDto, InvalidRequestErrorDto>;

    fn provider_list(
        &self,
        principal: SealedPrincipal,
        query: ProviderLocationQueryDto,
    ) -> ApplicationFuture<ProviderListResponseDto, CatalogApplicationError>;

    fn model_list(
        &self,
        principal: SealedPrincipal,
        query: ModelLocationQueryDto,
    ) -> ApplicationFuture<ModelListResponseDto, CatalogApplicationError>;

    fn model_default(
        &self,
        principal: SealedPrincipal,
        query: ModelDefaultLocationQueryDto,
    ) -> ApplicationFuture<ModelDefaultResponseDto, CatalogApplicationError>;

    fn agent_list(
        &self,
        principal: SealedPrincipal,
        query: super::AgentLocationQueryDto,
    ) -> ApplicationFuture<AgentListResponseDto, InvalidRequestErrorDto>;

    fn session_list(
        &self,
        principal: SealedPrincipal,
        query: SessionListQueryDto,
    ) -> ApplicationFuture<SessionsResponseDto, SessionListApplicationError>;

    fn session_active(
        &self,
        principal: SealedPrincipal,
    ) -> ApplicationFuture<SessionActiveResponseDto, SessionActiveApplicationError>;

    fn session_create(
        &self,
        principal: SealedPrincipal,
        request: SessionCreateRequestDto,
        launch: Option<grapt_types::SessionLaunch>,
    ) -> ApplicationFuture<SessionResponseDto, InvalidRequestErrorDto>;

    fn session_get(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError>;

    fn session_delete(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;

    fn session_fork(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SessionForkRequestDto,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError>;

    fn side_question(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: super::SideQuestionRequestDto,
    ) -> ApplicationFuture<super::SideQuestionResponseDto, SessionLookupApplicationError>;

    fn session_messages(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        query: MessageListQueryDto,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError>;

    fn session_messages_view(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        query: MessageListQueryDto,
        _view: MessageListView,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
        self.session_messages(principal, session_id, query)
    }

    fn session_message(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        message_id: WireId<grapt_types::MessageId>,
    ) -> ApplicationFuture<super::SessionMessageResponseDto, SessionMessageApplicationError>;

    fn session_prompt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: PromptRequestDto,
    ) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError>;

    fn session_interrupt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: InterruptRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError>;

    /// Queue a durable compaction request. It runs in the next input's
    /// first Step; a second request answers with the pending one.
    fn session_compact(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: CompactRequestDto,
    ) -> ApplicationFuture<CompactResponseDto, SessionCompactApplicationError>;

    fn permission_list(
        &self,
        principal: SealedPrincipal,
        query: PermissionLocationQueryDto,
    ) -> ApplicationFuture<PermissionGlobalListResponseDto, PermissionApplicationError>;

    fn session_permission_create(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request: PermissionCreateRequestDto,
    ) -> ApplicationFuture<PermissionCreateResponseDto, PermissionApplicationError>;

    fn session_permission_list(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
    ) -> ApplicationFuture<PermissionListResponseDto, PermissionApplicationError>;

    fn session_permission_get(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request_id: WireId<grapt_types::PermissionId>,
    ) -> ApplicationFuture<PermissionResponseDto, PermissionApplicationError>;

    fn session_permission_reply(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<grapt_types::SessionId>,
        request_id: WireId<grapt_types::PermissionId>,
        request: PermissionReplyRequestDto,
    ) -> ApplicationFuture<NoContent, PermissionApplicationError>;
}

pub trait CaptureApplicationPort: Send + Sync + 'static {
    fn capture_page(
        &self,
        principal: SealedPrincipal,
        interest: grapt_types::CaptureInterest,
        cursor: grapt_types::CaptureCursor,
    ) -> ApplicationFuture<super::capture::CapturePage, super::capture::CapturePageError>;

    /// The width probe: how many distinct durable aggregates the
    /// interest can see. It runs once, before the first page and before
    /// any sink exists, so an interest wider than the cursor's
    /// coordinate bound refuses up front with a named outcome instead
    /// of wedging mid-replay. The probe is not a page call: it consumes
    /// no rows and advances no cursor.
    fn count_capture_aggregates(
        &self,
        principal: SealedPrincipal,
        interest: grapt_types::CaptureInterest,
    ) -> ApplicationFuture<u64, super::capture::CapturePageError>;
}

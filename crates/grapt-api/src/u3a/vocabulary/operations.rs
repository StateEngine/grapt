use super::catalog::*;
use super::prelude::*;
use super::session::*;

closed_object! { pub struct CommandListResponseDto => "inline:command-list-response" {
    required data: BoundedVec<CommandInfoDto, MAX_CATALOG_ROWS> => "data",
    required location: LocationInfoDto => "location",
} }
closed_object! { pub struct SkillListResponseDto => "inline:skill-list-response" {
    required data: BoundedVec<SkillInfoDto, MAX_CATALOG_ROWS> => "data",
    optional warnings: BoundedVec<BoundedString<4096>, 512> => "warnings",
    required location: LocationInfoDto => "location",
} }
closed_object! { pub struct ReferenceListResponseDto => "inline:reference-list-response" {
    required data: BoundedVec<ReferenceInfoDto, MAX_CATALOG_ROWS> => "data",
    required location: LocationInfoDto => "location",
} }

string_enum! { pub enum PageOrderDto { Asc => "asc", Desc => "desc" } }
string_enum! { pub enum ParentFilterNullDto { Null => "null" } }
#[derive(Debug, Clone, PartialEq)]
pub enum ParentFilterDto {
    Session(WireId<grapt_types::SessionId>),
    Root(ParentFilterNullDto),
}

impl serde::Serialize for ParentFilterDto {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Session(session_id) => serde::Serialize::serialize(session_id, serializer),
            Self::Root(ParentFilterNullDto::Null) => serializer.serialize_str("null"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for ParentFilterDto {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <DtoString as serde::Deserialize>::deserialize(deserializer)?;
        if value.as_str() == "null" {
            return Ok(Self::Root(ParentFilterNullDto::Null));
        }
        WireId::parse(value.into_string())
            .map(Self::Session)
            .ok_or_else(|| <D::Error as serde::de::Error>::custom("invalid session identity"))
    }
}

closed_object! { pub struct ProjectCurrentLocationQueryValueDto => "operation:GET /api/project/current.query.location" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aProjectCurrentLocationDirectory) directory: DtoString => "directory",
    optional_nullable(grapt_schema::tri_state_ledger::U3aProjectCurrentLocationWorkspace) workspace: WireId<grapt_types::WorkspaceId> => "workspace",
} }
closed_object! { pub struct ProjectCurrentLocationQueryDto => "operation:GET /api/project/current.query" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aProjectCurrentLocation) location: ProjectCurrentLocationQueryValueDto => "location",
    optional session: WireId<grapt_types::SessionId> => "session",
} }
closed_object! { pub struct ProviderLocationQueryValueDto => "operation:GET /api/provider.query.location" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aProviderLocationDirectory) directory: DtoString => "directory",
    optional_nullable(grapt_schema::tri_state_ledger::U3aProviderLocationWorkspace) workspace: WireId<grapt_types::WorkspaceId> => "workspace",
} }
closed_object! { pub struct ProviderLocationQueryDto => "operation:GET /api/provider.query" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aProviderLocation) location: ProviderLocationQueryValueDto => "location",
    optional session: WireId<grapt_types::SessionId> => "session",
} }
closed_object! { pub struct ModelLocationQueryValueDto => "operation:GET /api/model.query.location" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aModelLocationDirectory) directory: DtoString => "directory",
    optional_nullable(grapt_schema::tri_state_ledger::U3aModelLocationWorkspace) workspace: WireId<grapt_types::WorkspaceId> => "workspace",
} }
closed_object! { pub struct ModelLocationQueryDto => "operation:GET /api/model.query" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aModelLocation) location: ModelLocationQueryValueDto => "location",
    optional session: WireId<grapt_types::SessionId> => "session",
} }
closed_object! { pub struct ModelDefaultLocationQueryValueDto => "operation:GET /api/model/default.query.location" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aModelDefaultLocationDirectory) directory: DtoString => "directory",
    optional_nullable(grapt_schema::tri_state_ledger::U3aModelDefaultLocationWorkspace) workspace: WireId<grapt_types::WorkspaceId> => "workspace",
} }
closed_object! { pub struct ModelDefaultLocationQueryDto => "operation:GET /api/model/default.query" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aModelDefaultLocation) location: ModelDefaultLocationQueryValueDto => "location",
    optional session: WireId<grapt_types::SessionId> => "session",
} }
closed_object! { pub struct AgentLocationQueryValueDto => "operation:GET /api/agent.query.location" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aAgentLocationDirectory) directory: DtoString => "directory",
    optional_nullable(grapt_schema::tri_state_ledger::U3aAgentLocationWorkspace) workspace: WireId<grapt_types::WorkspaceId> => "workspace",
} }
closed_object! { pub struct AgentLocationQueryDto => "operation:GET /api/agent.query" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aAgentLocation) location: AgentLocationQueryValueDto => "location",
    optional session: WireId<grapt_types::SessionId> => "session",
} }
closed_object! { pub struct SessionListQueryDto => "inline:session-list-query" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListWorkspace) workspace: WorkspaceIdDto => "workspace",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListLimit) limit: DtoString => "limit",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListOrder) order: PageOrderDto => "order",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListSearch) search: BoundedString<1024> => "search",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListParentId) parent_id: ParentFilterDto => "parentID",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListDirectory) directory: DtoString => "directory",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListProject) project: WireId<grapt_types::ProjectId> => "project",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListSubpath) subpath: DtoString => "subpath",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionListCursor) cursor: DtoString => "cursor",
} }
closed_object! { pub struct MessageListQueryDto => "inline:message-list-query" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aMessageListLimit) limit: DtoString => "limit",
    optional_nullable(grapt_schema::tri_state_ledger::U3aMessageListOrder) order: PageOrderDto => "order",
    optional_nullable(grapt_schema::tri_state_ledger::U3aMessageListCursor) cursor: DtoString => "cursor",
} }
closed_object! { pub struct SessionRenameRequestDto => "inline:session-rename" {
    required title: DtoString => "title",
}}

closed_object! { pub struct SessionAgentSelectDto => "inline:session-agent-select" {
    required agent: WireId<grapt_types::AgentId> => "agent",
}}

string_enum! { pub enum PermissionPresetDto {
    Inspect => "inspect", Author => "author", Maintainer => "maintainer", Bypass => "bypass"
} }
closed_object! { pub struct SessionPresetDataDto => "inline:session-preset-data" {
    nullable selected: PermissionPresetDto => "selected",
    required effective: PermissionPresetDto => "effective",
}}
closed_object! { pub struct SessionPresetResponseDto => "inline:session-preset-response" {
    required data: SessionPresetDataDto => "data",
}}

closed_object! { pub struct SessionPresetSelectDto => "inline:session-preset-select" {
    required preset: DtoString => "preset",
}}

closed_object! { pub struct SandboxWorktreeConfigRequestDto => "inline:sandbox-worktree-config-request" {
    required consent: bool => "consent",
    required path: DtoString => "path",
} }
closed_object! { pub struct SandboxWorktreeConfigDataDto => "inline:sandbox-worktree-config-data" {
    required missing: bool => "missing",
    optional path: DtoString => "path",
    required text: DtoString => "text",
} }
closed_object! { pub struct SandboxWorktreeConfigResponseDto => "inline:sandbox-worktree-config-response" {
    required data: SandboxWorktreeConfigDataDto => "data",
} }

closed_object! { pub struct InputUserMessageDto => "inline:input-user-message" {
    required kind: SessionPendingUserDtoType => "type",
    required data: SessionPendingUserDataDto => "data",
    required delivery: SessionPendingUserDtoDelivery => "delivery",
}}

closed_object! { pub struct InputSyntheticMessageDto => "inline:input-synthetic-message" {
    required kind: SessionPendingSyntheticDtoType => "type",
    required data: SessionPendingSyntheticDataDto => "data",
    required delivery: SessionPendingUserDtoDelivery => "delivery",
}}

tagged_union! {
    pub enum InputAdmittedMessageDto => "inline:input-admitted-message", tag "type" {
        User(InputUserMessageDto) => "user",
        Synthetic(InputSyntheticMessageDto) => "synthetic",
    }
}

closed_object! { pub struct InputAdmittedData => "inline:input-admitted-data" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required input_id: WireId<grapt_types::MessageId> => "inputID",
    required input: InputAdmittedMessageDto => "input",
}}

closed_object! { pub struct InputPromotedData => "inline:input-promoted-data" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required input_id: WireId<grapt_types::MessageId> => "inputID",
}}

closed_object! { pub struct SessionModelSelectDto => "inline:session-model-select" {
    required model: ModelRefDto => "model",
}}

closed_object! { pub struct SessionSubagentModelResponseDto => "inline:session-subagent-model-response" {
    nullable data: ModelRefDto => "data",
}}

closed_object! { pub struct SessionCreateRequestDto => "inline:session-create-request" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionCreateId) id: WireId<grapt_types::SessionId> => "id",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionCreateAgent) agent: WireId<grapt_types::AgentId> => "agent",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionCreateModel) model: ModelRefDto => "model",
    optional_nullable(grapt_schema::tri_state_ledger::U3aSessionCreateLocation) location: LocationRefDto => "location",
} }
closed_object! { pub struct PromptRequestDto => "inline:prompt-request" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aPromptId) id: WireId<grapt_types::MessageId> => "id",
    required text: PromptText => "text",
    optional files: BoundedVec<PromptInputFileAttachmentDto, MAX_PROMPT_FILES> => "files",
    optional agents: BoundedVec<PromptAgentAttachmentDto, MAX_PROMPT_AGENTS> => "agents",
    optional metadata: OpenObject => "metadata",
    optional_nullable(grapt_schema::tri_state_ledger::U3aPromptDelivery) delivery: SessionPendingUserDtoDelivery => "delivery",
    optional_nullable(grapt_schema::tri_state_ledger::U3aPromptResume) resume: bool => "resume",
} }
closed_object! { pub struct InterruptRequestDto => "inline:interrupt-request" {
    optional_nullable(grapt_schema::tri_state_ledger::U3aInterruptExecutionId) execution_id: WireId<grapt_types::ExecutionId> => "executionID",
    optional resume_pending: bool => "resumePending",
    optional stop_processes: bool => "stopProcesses",
} }

closed_object! { pub struct ProviderListResponseDto => "inline:provider-list-response" {
    required location: LocationInfoDto => "location",
    required data: BoundedVec<ProviderInfoDto, MAX_CATALOG_ROWS> => "data",
} }
closed_object! { pub struct ModelListResponseDto => "inline:model-list-response" {
    required location: LocationInfoDto => "location",
    required data: BoundedVec<ModelInfoDto, MAX_MODEL_ROWS> => "data",
} }
closed_object! { pub struct ModelDefaultResponseDto => "inline:model-default-response" {
    required location: LocationInfoDto => "location",
    nullable data: ModelInfoDto => "data",
} }
closed_object! { pub struct AgentListResponseDto => "inline:agent-list-response" {
    required location: LocationInfoDto => "location",
    required data: BoundedVec<AgentInfoDto, MAX_CATALOG_ROWS> => "data",
} }
closed_object! { pub struct SessionActiveResponseDto => "inline:session-active-response" {
    required data: BoundedMap<WireId<grapt_types::SessionId>, SessionActiveDto, MAX_SESSION_ROWS> => "data",
} }
closed_object! { pub struct SessionResponseDto => "inline:session-response" {
    required data: SessionInfoDto => "data",
} }
closed_object! { pub struct PromptResponseDto => "inline:prompt-response" {
    required data: SessionPendingUserDto => "data",
    optional queued: PromptQueuedDto => "queued",
} }

// Present when the prompt was admitted but its turn was blocked before it
// began: the prompt stays queued for the Session's next turn, and this says
// why, even if the durable not-run-yet notice could not be written.
closed_object! { pub struct PromptQueuedDto => "inline:prompt-queued" {
    required reason: DtoString => "reason",
} }

named_newtype! { pub struct ProjectListResponseDto(BoundedVec<ProjectDto, MAX_CATALOG_ROWS>) => "inline:project-list-response" }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoContent;

impl NoContent {
    pub const fn encoded(self) -> &'static [u8] {
        &[]
    }
}

closed_object! { pub struct SessionSubagentModelSelectedData => "inline:session-subagent-model-selected" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required model: ModelRefDto => "model",
}}
closed_object! { pub struct SessionModelSelectedData => "inline:session-model-selected" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required model: ModelRefDto => "model",
}}
closed_object! { pub struct SessionAgentSelectedData => "inline:session-agent-selected" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required agent: WireId<grapt_types::AgentId> => "agent",
}}
closed_object! { pub struct SessionSubagentModelClearedData => "inline:session-subagent-model-cleared" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
}}

// Native conversation editing; these routes do not restore filesystem effects.
closed_object! { pub struct SessionRewindPreviewRequestDto => "inline:session-rewind-preview-request" {
    required prompts: PositiveU64 => "prompts",
} }
closed_object! { pub struct SessionRewindRequestDto => "inline:session-rewind-request" {
    required message_id: WireId<grapt_types::MessageId> => "messageID",
    required tip: NonNegativeU64 => "tip",
} }
closed_object! { pub struct SessionRewindSelectionDto => "inline:session-rewind-selection" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required message_id: WireId<grapt_types::MessageId> => "messageID",
    required tip: NonNegativeU64 => "tip",
    required prompt: PromptText => "prompt",
} }
closed_object! { pub struct SessionRewindPreviewResponseDto => "inline:session-rewind-preview-response" {
    required data: SessionRewindSelectionDto => "data",
} }
closed_object! { pub struct SessionRewindCommittedDto => "inline:session-rewind-committed" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required message_id: WireId<grapt_types::MessageId> => "messageID",
    required committed_seq: NonNegativeU64 => "committedSeq",
    required prompt: PromptText => "prompt",
} }
closed_object! { pub struct SessionRewindResponseDto => "inline:session-rewind-response" {
    required data: SessionRewindCommittedDto => "data",
} }

string_enum! { pub enum SessionUndoStatusDto { Applied => "applied", Refused => "refused", Quarantined => "quarantined" } }

// File undo is an explicit native command, separate from conversation rewind.
closed_object! { pub struct SessionUndoPreviewRequestDto => "inline:session-undo-preview-request" { } }
closed_object! { pub struct SessionUndoRequestDto => "inline:session-undo-request" {
    required input_id: WireId<grapt_types::MessageId> => "inputID",
    required target: DtoString => "target",
} }
closed_object! { pub struct SessionUndoSelectionDto => "inline:session-undo-selection" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required input_id: WireId<grapt_types::MessageId> => "inputID",
    required target: DtoString => "target",
    required paths: BoundedVec<DtoString, 32> => "paths",
    required incomplete_patches: NonNegativeU64 => "incompletePatches",
    required writes: NonNegativeU64 => "writes",
    required deletes: NonNegativeU64 => "deletes",
    required bytes: NonNegativeU64 => "bytes",
} }
closed_object! { pub struct SessionUndoPreviewResponseDto => "inline:session-undo-preview-response" {
    required data: SessionUndoSelectionDto => "data",
} }
closed_object! { pub struct SessionUndoCommittedDto => "inline:session-undo-committed" {
    required status: SessionUndoStatusDto => "status",
    required message: DtoString => "message",
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required input_id: WireId<grapt_types::MessageId> => "inputID",
    required target: DtoString => "target",
} }
closed_object! { pub struct SessionUndoResponseDto => "inline:session-undo-response" {
    required data: SessionUndoCommittedDto => "data",
} }

// Settles nested tool starts that process loss left without a result: each is
// recorded as an unknown effect so the Session can continue, warned.
closed_object! { pub struct SessionReconcileRequestDto => "inline:session-reconcile-request" { } }
closed_object! { pub struct SessionReconciledDto => "inline:session-reconciled" {
    required session_id: WireId<grapt_types::SessionId> => "sessionID",
    required reconciled: NonNegativeU64 => "reconciled",
} }
closed_object! { pub struct SessionReconcileResponseDto => "inline:session-reconcile-response" {
    required data: SessionReconciledDto => "data",
} }

pub const MAX_WORKSPACE_DIFF_BYTES: usize = 128 * 1024;

closed_object! { pub struct WorkspaceDiffDataDto => "inline:workspace-diff-data" {
    required is_git: bool => "isGit",
    required text: BoundedString<MAX_WORKSPACE_DIFF_BYTES> => "text",
} }
closed_object! { pub struct WorkspaceDiffResponseDto => "inline:workspace-diff-response" {
    required location: LocationInfoDto => "location",
    required data: WorkspaceDiffDataDto => "data",
} }

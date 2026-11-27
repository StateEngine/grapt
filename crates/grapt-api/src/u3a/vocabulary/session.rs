use super::catalog::*;
use super::prelude::*;
use super::session_state::{SessionStateDto, SessionStateErrorDto};
use super::utf16_text::Utf16TextPublic;

bounded_id! { pub struct PartIdDto }
bounded_id! { pub struct ShellIdDto => "sh_" }

// Empty or absent body preserves latest-point fork compatibility.
closed_object! { pub struct SessionForkRequestDto => "inline:session-fork-request" {
    optional through_seq: NonNegativeU64 => "throughSeq",
    optional before_message_id: WireId<grapt_types::MessageId> => "beforeMessageID",
} }

closed_object! { pub struct SideQuestionRequestDto => "inline:side-question-request" {
    required prompt: DtoString => "prompt",
} }
closed_object! { pub struct SideQuestionResponseDto => "inline:side-question-response" {
    required text: DtoString => "text",
} }

closed_object! {
    pub struct SessionInfoDtoFork => "inline:SessionInfoDtoFork" {
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        optional message_id: WireId<grapt_types::MessageId> => "messageID",
    }
}

closed_object! {
    pub struct SessionInfoDtoTime => "inline:SessionInfoDtoTime" {
        // Unreadable lifecycle metadata leaves the creation time unknown.
        optional created: FiniteF64 => "created",
        required updated: FiniteF64 => "updated",
        optional archived: FiniteF64 => "archived",
    }
}

string_enum! {
    pub enum SessionInfoDtoMetricsState {
        ExceedsBound => "exceeds-bound",
    }
}

// Accepted deviation (compat/openapi/DEVIATIONS.md, SC-HIST-01): present only
// when the Session's lifetime metrics read refuses at its bound, in which case
// `tokens` is absent rather than a truncated total.
closed_object! {
    pub struct SessionInfoDtoMetrics => "inline:SessionInfoDtoMetrics" {
        required state: SessionInfoDtoMetricsState => "state",
        required rows: NonNegativeU64 => "rows",
        required bytes: NonNegativeU64 => "bytes",
    }
}

string_enum! {
    pub enum SessionInfoDtoCostStatus {
        CatalogApiEstimate => "catalog-api-estimate",
        CatalogApiSubtotal => "catalog-api-subtotal",
        Unavailable => "unavailable",
        Unreadable => "unreadable",
        KnownNonbillable => "known-nonbillable",
    }
}

string_enum! {
    pub enum SessionInfoDtoMetadataStatus {
        Unavailable => "unavailable",
        Quarantined => "quarantined",
    }
}

closed_object! {
    pub struct SessionInfoDto => "Session.Info" {
        required id: WireId<grapt_types::SessionId> => "id",
        optional parent_id: WireId<grapt_types::SessionId> => "parentID",
        optional fork: SessionInfoDtoFork => "fork",
        required project_id: WireId<grapt_types::ProjectId> => "projectID",
        optional agent: WireId<grapt_types::AgentId> => "agent",
        optional preset: DtoString => "preset",
        optional model: ModelRefDto => "model",
        // Absent is unknown. costStatus labels catalog estimates separately
        // from an independently known nonbillable zero.
        optional cost: MoneyUSDDto => "cost",
        optional cost_status: SessionInfoDtoCostStatus => "costStatus",
        optional tokens: TokenUsageInfoDto => "tokens",
        required time: SessionInfoDtoTime => "time",
        required title: DtoString => "title",
        required location: LocationRefDto => "location",
        optional subpath: DtoString => "subpath",
        optional revert: SessionRevertDto => "revert",
        optional metrics: SessionInfoDtoMetrics => "metrics",
        optional state: SessionStateDto => "state",
        optional state_error: SessionStateErrorDto => "stateError",
        optional metadata_status: SessionInfoDtoMetadataStatus => "metadataStatus",
    }
}

closed_object! {
    pub struct SessionMessageAgentSelectedDtoTime => "inline:SessionMessageAgentSelectedDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageAgentSelectedDtoType {
        AgentSwitched => "agent-switched",
    }
}

closed_object! {
    pub struct SessionMessageAgentSelectedDto => "Session.Message.AgentSelected" {
        required kind: SessionMessageAgentSelectedDtoType => "type",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageAgentSelectedDtoTime => "time",
        required agent: WireId<grapt_types::AgentId> => "agent",
    }
}

closed_object! {
    pub struct SessionMessageAssistantDtoTime => "inline:SessionMessageAssistantDtoTime" {
        required created: FiniteF64 => "created",
        optional completed: FiniteF64 => "completed",
    }
}

string_enum! {
    pub enum SessionMessageAssistantDtoType {
        Assistant => "assistant",
    }
}

tagged_union! {
    pub enum SessionMessageAssistantDtoContentItem, tag "type" {
        Text(SessionMessageAssistantTextDto) => "text",
        Reasoning(SessionMessageAssistantReasoningDto) => "reasoning",
        Tool(SessionMessageAssistantToolDto) => "tool",
    }
}

closed_object! {
    pub struct SessionMessageAssistantDtoSnapshot => "inline:SessionMessageAssistantDtoSnapshot" {
        optional start: DtoString => "start",
        optional end: DtoString => "end",
        optional files: BoundedVec<DtoString, MAX_PROMPT_FILES> => "files",
    }
}

string_enum! {
    pub enum SessionMessageAssistantDtoFinish {
        Stop => "stop",
        Length => "length",
        ToolCalls => "tool-calls",
        ContentFilter => "content-filter",
        Error => "error",
        Unknown => "unknown",
    }
}

closed_object! {
    pub struct SessionMessageAssistantDto => "Session.Message.Assistant" {
        required kind: SessionMessageAssistantDtoType => "type",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageAssistantDtoTime => "time",
        required agent: WireId<grapt_types::AgentId> => "agent",
        required model: ModelRefDto => "model",
        required content: BoundedVec<SessionMessageAssistantDtoContentItem, MAX_NESTED_ITEMS> => "content",
        optional snapshot: SessionMessageAssistantDtoSnapshot => "snapshot",
        optional finish: SessionMessageAssistantDtoFinish => "finish",
        optional cost: MoneyUSDDto => "cost",
        optional tokens: TokenUsageInfoDto => "tokens",
        optional observed_usage: TokenUsageObservationDto => "observedUsage",
        optional error: SessionStructuredErrorDto => "error",
        optional retry: SessionMessageAssistantRetryDto => "retry",
    }
}

string_enum! {
    pub enum SessionMessageAssistantReasoningDtoType {
        Reasoning => "reasoning",
    }
}

closed_object! {
    pub struct SessionMessageAssistantReasoningDtoTime => "inline:SessionMessageAssistantReasoningDtoTime" {
        required created: FiniteF64 => "created",
        optional completed: FiniteF64 => "completed",
    }
}

closed_object! {
    pub struct SessionMessageAssistantReasoningDto => "Session.Message.Assistant.Reasoning" {
        required kind: SessionMessageAssistantReasoningDtoType => "type",
        required text: DtoString => "text",
        optional state: SessionMessageProviderStateDto => "state",
        optional time: SessionMessageAssistantReasoningDtoTime => "time",
    }
}

closed_object! {
    pub struct SessionMessageAssistantRetryDto => "Session.Message.Assistant.Retry" {
        required attempt: PositiveU64 => "attempt",
        required at: FiniteF64 => "at",
        required error: SessionStructuredErrorDto => "error",
    }
}

string_enum! {
    pub enum SessionMessageAssistantTextDtoType {
        Text => "text",
    }
}

closed_object! {
    pub struct SessionMessageAssistantTextDto => "Session.Message.Assistant.Text" {
        required kind: SessionMessageAssistantTextDtoType => "type",
        required text: DtoString => "text",
        optional state: SessionMessageProviderStateDto => "state",
    }
}

string_enum! {
    pub enum SessionMessageAssistantToolDtoType {
        Tool => "tool",
    }
}

tagged_union! {
    pub enum SessionMessageAssistantToolDtoState, tag "status" {
        Streaming(SessionMessageToolStateStreamingDto) => "streaming",
        Running(SessionMessageToolStateRunningDto) => "running",
        Completed(SessionMessageToolStateCompletedDto) => "completed",
        Error(SessionMessageToolStateErrorDto) => "error",
    }
}

closed_object! {
    pub struct SessionMessageAssistantToolDtoTime => "inline:SessionMessageAssistantToolDtoTime" {
        required created: FiniteF64 => "created",
        optional ran: FiniteF64 => "ran",
        optional completed: FiniteF64 => "completed",
    }
}

closed_object! {
    pub struct SessionMessageAssistantToolDto => "Session.Message.Assistant.Tool" {
        required kind: SessionMessageAssistantToolDtoType => "type",
        required id: WireId<grapt_types::ToolCallId> => "id",
        required name: DtoString => "name",
        optional executed: bool => "executed",
        optional provider_state: SessionMessageProviderStateDto => "providerState",
        optional provider_result_state: SessionMessageProviderStateDto => "providerResultState",
        required state: SessionMessageAssistantToolDtoState => "state",
        required time: SessionMessageAssistantToolDtoTime => "time",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionMessageCompactionDto {
    Running(SessionMessageCompactionRunningDto),
    Completed(SessionMessageCompactionCompletedDto),
    Failed(SessionMessageCompactionFailedDto),
}

impl serde::Serialize for SessionMessageCompactionDto {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Running(value) => serde::Serialize::serialize(value, serializer),
            Self::Completed(value) => serde::Serialize::serialize(value, serializer),
            Self::Failed(value) => serde::Serialize::serialize(value, serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for SessionMessageCompactionDto {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CompactionVisitor;

        impl<'de> serde::de::Visitor<'de> for CompactionVisitor {
            type Value = SessionMessageCompactionDto;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("tag-first compaction union")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let Some(first) = serde::de::MapAccess::next_key::<BoundedString<256>>(&mut map)?
                else {
                    return Err(<A::Error as serde::de::Error>::missing_field("type"));
                };
                if first.as_str() != "type" {
                    return Err(<A::Error as serde::de::Error>::unknown_field(
                        first.as_str(),
                        &["type"],
                    ));
                }
                let kind: DtoString = serde::de::MapAccess::next_value(&mut map)?;
                if kind.as_str() != "compaction" {
                    return Err(<A::Error as serde::de::Error>::unknown_variant(
                        kind.as_str(),
                        &["compaction"],
                    ));
                }

                let Some(second) = serde::de::MapAccess::next_key::<BoundedString<256>>(&mut map)?
                else {
                    return Err(<A::Error as serde::de::Error>::missing_field("status"));
                };
                if second.as_str() != "status" {
                    return Err(<A::Error as serde::de::Error>::unknown_field(
                        second.as_str(),
                        &["status"],
                    ));
                }
                let status: DtoString = serde::de::MapAccess::next_value(&mut map)?;
                let replay = crate::u3a::dto::ReplayMap::new(
                    ["type", "status"],
                    [kind.into_string(), status.as_str().to_owned()],
                    map,
                );
                let input = serde::de::value::MapAccessDeserializer::new(replay);
                match status.as_str() {
                    "running" => serde::Deserialize::deserialize(input)
                        .map(SessionMessageCompactionDto::Running),
                    "completed" => serde::Deserialize::deserialize(input)
                        .map(SessionMessageCompactionDto::Completed),
                    "failed" => serde::Deserialize::deserialize(input)
                        .map(SessionMessageCompactionDto::Failed),
                    unknown => Err(<A::Error as serde::de::Error>::unknown_variant(
                        unknown,
                        &["running", "completed", "failed"],
                    )),
                }
            }
        }

        deserializer.deserialize_map(CompactionVisitor)
    }
}

impl NamedSchema for SessionMessageCompactionDto {
    const OPENAPI_NAME: &'static str = "Session.Message.Compaction";
}

string_enum! {
    pub enum SessionMessageCompactionCompletedDtoType {
        Compaction => "compaction",
    }
}

closed_object! {
    pub struct SessionMessageCompactionCompletedDtoTime => "inline:SessionMessageCompactionCompletedDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageCompactionCompletedDtoStatus {
        Completed => "completed",
    }
}

string_enum! {
    pub enum SessionMessageCompactionCompletedDtoReason {
        Auto => "auto",
        Manual => "manual",
    }
}

closed_object! {
    pub struct SessionMessageCompactionCompletedDto => "Session.Message.Compaction.Completed" {
        required kind: SessionMessageCompactionCompletedDtoType => "type",
        required status: SessionMessageCompactionCompletedDtoStatus => "status",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageCompactionCompletedDtoTime => "time",
        required reason: SessionMessageCompactionCompletedDtoReason => "reason",
        required summary: DtoString => "summary",
        required recent: Utf16TextPublic => "recent",
    }
}

string_enum! {
    pub enum SessionMessageCompactionFailedDtoType {
        Compaction => "compaction",
    }
}

closed_object! {
    pub struct SessionMessageCompactionFailedDtoTime => "inline:SessionMessageCompactionFailedDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageCompactionFailedDtoStatus {
        Failed => "failed",
    }
}

string_enum! {
    pub enum SessionMessageCompactionFailedDtoReason {
        Auto => "auto",
        Manual => "manual",
    }
}

closed_object! {
    pub struct SessionMessageCompactionFailedDto => "Session.Message.Compaction.Failed" {
        required kind: SessionMessageCompactionFailedDtoType => "type",
        required status: SessionMessageCompactionFailedDtoStatus => "status",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageCompactionFailedDtoTime => "time",
        required reason: SessionMessageCompactionFailedDtoReason => "reason",
        required error: SessionStructuredErrorDto => "error",
    }
}

string_enum! {
    pub enum SessionMessageCompactionRunningDtoType {
        Compaction => "compaction",
    }
}

closed_object! {
    pub struct SessionMessageCompactionRunningDtoTime => "inline:SessionMessageCompactionRunningDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageCompactionRunningDtoStatus {
        Running => "running",
    }
}

string_enum! {
    pub enum SessionMessageCompactionRunningDtoReason {
        Auto => "auto",
        Manual => "manual",
    }
}

closed_object! {
    pub struct SessionMessageCompactionRunningDto => "Session.Message.Compaction.Running" {
        required kind: SessionMessageCompactionRunningDtoType => "type",
        required status: SessionMessageCompactionRunningDtoStatus => "status",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageCompactionRunningDtoTime => "time",
        required reason: SessionMessageCompactionRunningDtoReason => "reason",
        required summary: DtoString => "summary",
        required recent: Utf16TextPublic => "recent",
    }
}

tagged_union! {
    pub enum SessionMessageInfoDto => "Session.Message.Info", tag "type" {
        AgentSelected(SessionMessageAgentSelectedDto) => "agent-switched",
        ModelSelected(SessionMessageModelSelectedDto) => "model-switched",
        User(SessionMessageUserDto) => "user",
        Synthetic(SessionMessageSyntheticDto) => "synthetic",
        System(SessionMessageSystemDto) => "system",
        Skill(SessionMessageSkillDto) => "skill",
        Shell(SessionMessageShellDto) => "shell",
        Assistant(SessionMessageAssistantDto) => "assistant",
        Compaction(SessionMessageCompactionDto) => "compaction",
    }
}

closed_object! {
    pub struct SessionMessageModelSelectedDtoTime => "inline:SessionMessageModelSelectedDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageModelSelectedDtoType {
        ModelSwitched => "model-switched",
    }
}

closed_object! {
    pub struct SessionMessageModelSelectedDto => "Session.Message.ModelSelected" {
        required kind: SessionMessageModelSelectedDtoType => "type",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageModelSelectedDtoTime => "time",
        required model: ModelRefDto => "model",
        optional previous: ModelRefDto => "previous",
    }
}

named_newtype! { pub struct SessionMessageProviderStateDto(OpenObject) => "Session.Message.ProviderState" }

closed_object! {
    pub struct SessionMessageShellDtoTime => "inline:SessionMessageShellDtoTime" {
        required created: FiniteF64 => "created",
        optional completed: FiniteF64 => "completed",
    }
}

string_enum! {
    pub enum SessionMessageShellDtoType {
        Shell => "shell",
    }
}

string_enum! {
    pub enum SessionMessageShellDtoStatus {
        Running => "running",
        Exited => "exited",
        Timeout => "timeout",
        Killed => "killed",
    }
}

pub type SessionMessageShellDtoExitValue1 = FiniteOrSentinelF64;
pub type SessionMessageShellDtoExit = FiniteOrSentinelF64;

closed_object! {
    pub struct SessionMessageShellDtoOutput => "inline:SessionMessageShellDtoOutput" {
        required output: DtoString => "output",
        required cursor: NonNegativeU64 => "cursor",
        required size: NonNegativeU64 => "size",
        required truncated: bool => "truncated",
    }
}

closed_object! {
    pub struct SessionMessageShellDto => "Session.Message.Shell" {
        required kind: SessionMessageShellDtoType => "type",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageShellDtoTime => "time",
        required shell_id: ShellIdDto => "shellID",
        required command: DtoString => "command",
        required status: SessionMessageShellDtoStatus => "status",
        optional exit: SessionMessageShellDtoExit => "exit",
        optional output: SessionMessageShellDtoOutput => "output",
    }
}

closed_object! {
    pub struct SessionMessageSkillDtoTime => "inline:SessionMessageSkillDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageSkillDtoType {
        Skill => "skill",
    }
}

closed_object! {
    pub struct SessionMessageSkillDto => "Session.Message.Skill" {
        required kind: SessionMessageSkillDtoType => "type",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageSkillDtoTime => "time",
        required skill: DtoString => "skill",
        required name: DtoString => "name",
        required text: DtoString => "text",
    }
}

closed_object! {
    pub struct SessionMessageSyntheticDtoTime => "inline:SessionMessageSyntheticDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageSyntheticDtoType {
        Synthetic => "synthetic",
    }
}

closed_object! {
    pub struct SessionMessageSyntheticDto => "Session.Message.Synthetic" {
        required kind: SessionMessageSyntheticDtoType => "type",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageSyntheticDtoTime => "time",
        required text: DtoString => "text",
        optional description: DtoString => "description",
    }
}

closed_object! {
    pub struct SessionMessageSystemDtoTime => "inline:SessionMessageSystemDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageSystemDtoType {
        System => "system",
    }
}

closed_object! {
    pub struct SessionMessageSystemDto => "Session.Message.System" {
        required kind: SessionMessageSystemDtoType => "type",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageSystemDtoTime => "time",
        required text: DtoString => "text",
    }
}

string_enum! {
    pub enum SessionMessageToolStateCompletedDtoStatus {
        Completed => "completed",
    }
}

closed_object! {
    pub struct SessionMessageToolStateCompletedDto => "Session.Message.ToolState.Completed" {
        required status: SessionMessageToolStateCompletedDtoStatus => "status",
        required input: super::ToolInputObject => "input",
        required content: BoundedVec<ToolContentDto, MAX_NESTED_ITEMS> => "content",
        optional metadata: OpenObject => "metadata",
    }
}

string_enum! {
    pub enum SessionMessageToolStateErrorDtoStatus {
        Error => "error",
    }
}

closed_object! {
    pub struct SessionMessageToolStateErrorDto => "Session.Message.ToolState.Error" {
        required status: SessionMessageToolStateErrorDtoStatus => "status",
        required input: super::ToolInputObject => "input",
        required error: SessionStructuredErrorDto => "error",
        optional content: BoundedVec<ToolContentDto, MAX_NESTED_ITEMS> => "content",
        optional metadata: OpenObject => "metadata",
    }
}

string_enum! {
    pub enum SessionMessageToolStateRunningDtoStatus {
        Running => "running",
    }
}

closed_object! {
    pub struct SessionMessageToolStateRunningDto => "Session.Message.ToolState.Running" {
        required status: SessionMessageToolStateRunningDtoStatus => "status",
        required input: super::ToolInputObject => "input",
        required metadata: OpenObject => "metadata",
    }
}

string_enum! {
    pub enum SessionMessageToolStateStreamingDtoStatus {
        Streaming => "streaming",
    }
}

closed_object! {
    pub struct SessionMessageToolStateStreamingDto => "Session.Message.ToolState.Streaming" {
        required status: SessionMessageToolStateStreamingDtoStatus => "status",
        required input: DtoString => "input",
    }
}

closed_object! {
    pub struct SessionMessageUserDtoTime => "inline:SessionMessageUserDtoTime" {
        required created: FiniteF64 => "created",
    }
}

string_enum! {
    pub enum SessionMessageUserDtoType {
        User => "user",
    }
}

closed_object! {
    pub struct SessionMessageUserDto => "Session.Message.User" {
        required kind: SessionMessageUserDtoType => "type",
        required id: WireId<grapt_types::MessageId> => "id",
        optional metadata: OpenObject => "metadata",
        required time: SessionMessageUserDtoTime => "time",
        required text: DtoString => "text",
        optional files: BoundedVec<PromptFileAttachmentDto, MAX_PROMPT_FILES> => "files",
        optional agents: BoundedVec<PromptAgentAttachmentDto, MAX_PROMPT_AGENTS> => "agents",
    }
}

closed_object! {
    pub struct SessionRevertDto => "Session.Revert" {
        required message_id: WireId<grapt_types::MessageId> => "messageID",
        optional part_id: PartIdDto => "partID",
        optional snapshot: DtoString => "snapshot",
        optional files: BoundedVec<FileDiffInfoDto, MAX_PROMPT_FILES> => "files",
    }
}

closed_object! {
    pub struct SessionStructuredErrorDto => "Session.StructuredError" {
        required kind: DtoString => "type",
        required message: DtoString => "message",
    }
}

string_enum! {
    pub enum SessionActiveDtoType {
        Running => "running",
    }
}

closed_object! {
    pub struct SessionActiveDto => "SessionActive" {
        required kind: SessionActiveDtoType => "type",
        optional_nullable(grapt_schema::tri_state_ledger::U3aSessionActiveExecutionId) execution_id: WireId<grapt_types::ExecutionId> => "executionID",
        optional state: SessionStateDto => "state",
        optional state_error: SessionStateErrorDto => "stateError",
    }
}

closed_object! {
    pub struct SessionMessagesResponseDtoCursor => "inline:SessionMessagesResponseDtoCursor" {
        optional_nullable(grapt_schema::tri_state_ledger::U3aSessionMessagesCursorPrevious) previous: DtoString => "previous",
        optional_nullable(grapt_schema::tri_state_ledger::U3aSessionMessagesCursorNext) next: DtoString => "next",
    }
}

closed_object! {
    pub struct SessionMessagesResponseDto => "SessionMessagesResponse" {
        required data: BoundedVec<SessionMessageInfoDto, MAX_MESSAGE_ROWS> => "data",
        required cursor: SessionMessagesResponseDtoCursor => "cursor",
    }
}

closed_object! {
    pub struct SessionMessageResponseDto => "SessionMessageResponse" {
        required data: SessionMessageInfoDto => "data",
    }
}

string_enum! {
    pub enum MessageNotFoundErrorDtoTag {
        MessageNotFoundError => "MessageNotFoundError",
    }
}

closed_object! {
    pub struct MessageNotFoundErrorDto => "MessageNotFoundError" {
        required tag: MessageNotFoundErrorDtoTag => "_tag",
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required message_id: WireId<grapt_types::MessageId> => "messageID",
        required message: DtoString => "message",
    }
}

string_enum! {
    pub enum SessionNotFoundErrorDtoTag {
        SessionNotFoundError => "SessionNotFoundError",
    }
}

closed_object! {
    pub struct SessionNotFoundErrorDto => "SessionNotFoundError" {
        required tag: SessionNotFoundErrorDtoTag => "_tag",
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required message: DtoString => "message",
    }
}

string_enum! {
    pub enum SessionPendingUserDtoType {
        User => "user",
    }
}

closed_object! { pub struct SessionPendingListResponseDto => "inline:session-pending-list-response" {
    required data: BoundedVec<SessionPendingInfoDto, 128> => "data",
} }

tagged_union! {
    pub enum SessionPendingInfoDto => "SessionPending.Info", tag "type" {
        User(SessionPendingUserDto) => "user",
        Synthetic(SessionPendingSyntheticDto) => "synthetic",
        Compaction(SessionPendingCompactionDto) => "compaction",
    }
}

string_enum! {
    pub enum SessionPendingUserDtoDelivery {
        Steer => "steer",
        Queue => "queue",
    }
}

// Pending arms emit the discriminator first for the bounded tagged-union decoder.
closed_object! {
    pub struct SessionPendingUserDto => "SessionPending.User" {
        required kind: SessionPendingUserDtoType => "type",
        required admitted_seq: NonNegativeU64 => "admittedSeq",
        required id: WireId<grapt_types::MessageId> => "id",
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required time_created: FiniteF64 => "timeCreated",
        required data: SessionPendingUserDataDto => "data",
        required delivery: SessionPendingUserDtoDelivery => "delivery",
    }
}

string_enum! {
    pub enum SessionPendingSyntheticDtoType {
        Synthetic => "synthetic",
    }
}

closed_object! {
    pub struct SessionPendingSyntheticDto => "SessionPending.Synthetic" {
        required kind: SessionPendingSyntheticDtoType => "type",
        required admitted_seq: NonNegativeU64 => "admittedSeq",
        required id: WireId<grapt_types::MessageId> => "id",
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required time_created: FiniteF64 => "timeCreated",
        required data: SessionPendingSyntheticDataDto => "data",
        required delivery: SessionPendingUserDtoDelivery => "delivery",
    }
}

string_enum! {
    pub enum SessionPendingCompactionDtoType {
        Compaction => "compaction",
    }
}

// A user-requested compaction barrier (`SessionPending.Compaction`). It runs
// in the next input's first Step and leaves the pending list when settled.
closed_object! {
    pub struct SessionPendingCompactionDto => "SessionPending.Compaction" {
        required kind: SessionPendingCompactionDtoType => "type",
        required admitted_seq: NonNegativeU64 => "admittedSeq",
        required id: WireId<grapt_types::MessageId> => "id",
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required time_created: FiniteF64 => "timeCreated",
    }
}

closed_object! { pub struct CompactRequestDto => "inline:compact-request" {
    optional id: WireId<grapt_types::MessageId> => "id",
    optional instructions: DtoString => "instructions",
} }

closed_object! { pub struct CompactResponseDto => "inline:session-compact-response" {
    required data: SessionPendingCompactionDto => "data",
} }

closed_object! {
    pub struct SessionPendingSyntheticDataDto => "SessionPending.SyntheticData" {
        required text: PromptText => "text",
        optional description: DtoString => "description",
        optional metadata: OpenObject => "metadata",
    }
}

closed_object! {
    pub struct SessionPendingUserDataDto => "SessionPending.UserData" {
        required text: PromptText => "text",
        optional files: BoundedVec<PromptFileAttachmentDto, MAX_PROMPT_FILES> => "files",
        optional agents: BoundedVec<PromptAgentAttachmentDto, MAX_PROMPT_AGENTS> => "agents",
        optional metadata: OpenObject => "metadata",
    }
}

closed_object! {
    pub struct SessionsResponseDtoCursor => "inline:SessionsResponseDtoCursor" {
        optional_nullable(grapt_schema::tri_state_ledger::U3aSessionsCursorPrevious) previous: DtoString => "previous",
        optional_nullable(grapt_schema::tri_state_ledger::U3aSessionsCursorNext) next: DtoString => "next",
    }
}

closed_object! {
    pub struct SessionsResponseDto => "SessionsResponse" {
        required data: BoundedVec<SessionInfoDto, MAX_SESSION_ROWS> => "data",
        required cursor: SessionsResponseDtoCursor => "cursor",
    }
}

closed_object! {
    pub struct TokenUsageInfoDtoCache => "inline:TokenUsageInfoDtoCache" {
        required read: FiniteF64 => "read",
        required write: FiniteF64 => "write",
    }
}

closed_object! {
    pub struct TokenUsageInfoDto => "TokenUsage.Info" {
        required input: FiniteF64 => "input",
        required output: FiniteF64 => "output",
        required reasoning: FiniteF64 => "reasoning",
        required cache: TokenUsageInfoDtoCache => "cache",
    }
}

closed_object! {
    pub struct TokenUsageObservationDto => "TokenUsage.Observation" {
        optional input_total: SafeTokenCount => "inputTotal",
        optional input_fresh: SafeTokenCount => "inputFresh",
        optional output: SafeTokenCount => "output",
        optional reasoning: SafeTokenCount => "reasoning",
        optional cache_read: SafeTokenCount => "cacheRead",
        optional cache_write: SafeTokenCount => "cacheWrite",
        optional total: SafeTokenCount => "total",
    }
}

tagged_union! {
    pub enum ToolContentDto => "Tool.Content", tag "type" {
        TextContent(ToolTextContentDto) => "text",
        FileContent(ToolFileContentDto) => "file",
    }
}

string_enum! {
    pub enum ToolFileContentDtoType {
        File => "file",
    }
}

closed_object! {
    pub struct ToolFileContentDto => "Tool.FileContent" {
        required kind: ToolFileContentDtoType => "type",
        required uri: DtoString => "uri",
        required mime: DtoString => "mime",
        optional_nullable(grapt_schema::tri_state_ledger::U3aToolFileContentName) name: DtoString => "name",
    }
}

string_enum! {
    pub enum ToolTextContentDtoType {
        Text => "text",
    }
}

closed_object! {
    pub struct ToolTextContentDto => "Tool.TextContent" {
        required kind: ToolTextContentDtoType => "type",
        required text: DtoString => "text",
    }
}

string_enum! {
    pub enum UnauthorizedErrorDtoTag {
        UnauthorizedError => "UnauthorizedError",
    }
}

closed_object! {
    pub struct UnauthorizedErrorDto => "UnauthorizedError" {
        required tag: UnauthorizedErrorDtoTag => "_tag",
        required message: DtoString => "message",
    }
}

string_enum! {
    pub enum UnknownErrorDtoTag {
        UnknownError => "UnknownError",
    }
}

closed_object! {
    pub struct UnknownErrorDto => "UnknownError" {
        required tag: UnknownErrorDtoTag => "_tag",
        required message: DtoString => "message",
        optional_nullable(grapt_schema::tri_state_ledger::U3aUnknownErrorRef) reference: DtoString => "ref",
    }
}

use super::catalog::*;
use super::operations::*;
use super::prelude::*;
use super::session::*;

pub const NAMED_SCHEMA_MANIFEST: &[(&str, &str)] = &[
    ("Agent.Color", stringify!(AgentColorDto)),
    ("Agent.Info", stringify!(AgentInfoDto)),
    ("ConflictError", stringify!(ConflictErrorDto)),
    ("FileDiff.Info", stringify!(FileDiffInfoDto)),
    ("InvalidCursorError", stringify!(InvalidCursorErrorDto)),
    ("InvalidRequestError", stringify!(InvalidRequestErrorDto)),
    ("InvalidRequestError1", stringify!(InvalidRequestError1Dto)),
    ("Location.Info", stringify!(LocationInfoDto)),
    ("Location.Ref", stringify!(LocationRefDto)),
    ("Model.Capabilities", stringify!(ModelCapabilitiesDto)),
    ("Model.Compatibility", stringify!(ModelCompatibilityDto)),
    ("Model.Cost", stringify!(ModelCostDto)),
    ("Model.Info", stringify!(ModelInfoDto)),
    ("Model.ReasoningField", stringify!(ModelReasoningFieldDto)),
    ("Model.Ref", stringify!(ModelRefDto)),
    ("Model.Variant", stringify!(ModelVariantDto)),
    ("Money.USD", stringify!(MoneyUSDDto)),
    (
        "Money.USDPerMillionTokens",
        stringify!(MoneyUSDPerMillionTokensDto),
    ),
    ("Permission.Effect", stringify!(PermissionEffectDto)),
    ("Permission.Rule", stringify!(PermissionRuleDto)),
    ("Permission.Ruleset", stringify!(PermissionRulesetDto)),
    ("Project", stringify!(ProjectDto)),
    ("Project.Commands", stringify!(ProjectCommandsDto)),
    ("Project.Current", stringify!(ProjectCurrentDto)),
    ("Project.Icon", stringify!(ProjectIconDto)),
    ("Project.Time", stringify!(ProjectTimeDto)),
    ("Project.Vcs", stringify!(ProjectVcsDto)),
    (
        "Prompt.AgentAttachment",
        stringify!(PromptAgentAttachmentDto),
    ),
    ("Prompt.Base64", stringify!(PromptBase64Dto)),
    ("Prompt.FileAttachment", stringify!(PromptFileAttachmentDto)),
    ("Prompt.FileSource", stringify!(PromptFileSourceDto)),
    ("Prompt.Mention", stringify!(PromptMentionDto)),
    (
        "PromptInput.FileAttachment",
        stringify!(PromptInputFileAttachmentDto),
    ),
    ("Provider.Info", stringify!(ProviderInfoDto)),
    ("Provider.Request", stringify!(ProviderRequestDto)),
    ("Provider.Settings", stringify!(ProviderSettingsDto)),
    ("ServiceHealth", stringify!(ServiceHealthDto)),
    (
        "ServiceUnavailableError",
        stringify!(ServiceUnavailableErrorDto),
    ),
    ("Session.Info", stringify!(SessionInfoDto)),
    (
        "Session.Message.AgentSelected",
        stringify!(SessionMessageAgentSelectedDto),
    ),
    (
        "Session.Message.Assistant",
        stringify!(SessionMessageAssistantDto),
    ),
    (
        "Session.Message.Assistant.Reasoning",
        stringify!(SessionMessageAssistantReasoningDto),
    ),
    (
        "Session.Message.Assistant.Retry",
        stringify!(SessionMessageAssistantRetryDto),
    ),
    (
        "Session.Message.Assistant.Text",
        stringify!(SessionMessageAssistantTextDto),
    ),
    (
        "Session.Message.Assistant.Tool",
        stringify!(SessionMessageAssistantToolDto),
    ),
    (
        "Session.Message.Compaction",
        stringify!(SessionMessageCompactionDto),
    ),
    (
        "Session.Message.Compaction.Completed",
        stringify!(SessionMessageCompactionCompletedDto),
    ),
    (
        "Session.Message.Compaction.Failed",
        stringify!(SessionMessageCompactionFailedDto),
    ),
    (
        "Session.Message.Compaction.Running",
        stringify!(SessionMessageCompactionRunningDto),
    ),
    ("Session.Message.Info", stringify!(SessionMessageInfoDto)),
    (
        "Session.Message.ModelSelected",
        stringify!(SessionMessageModelSelectedDto),
    ),
    (
        "Session.Message.ProviderState",
        stringify!(SessionMessageProviderStateDto),
    ),
    ("Session.Message.Shell", stringify!(SessionMessageShellDto)),
    ("Session.Message.Skill", stringify!(SessionMessageSkillDto)),
    (
        "Session.Message.Synthetic",
        stringify!(SessionMessageSyntheticDto),
    ),
    (
        "Session.Message.System",
        stringify!(SessionMessageSystemDto),
    ),
    (
        "Session.Message.ToolState.Completed",
        stringify!(SessionMessageToolStateCompletedDto),
    ),
    (
        "Session.Message.ToolState.Error",
        stringify!(SessionMessageToolStateErrorDto),
    ),
    (
        "Session.Message.ToolState.Running",
        stringify!(SessionMessageToolStateRunningDto),
    ),
    (
        "Session.Message.ToolState.Streaming",
        stringify!(SessionMessageToolStateStreamingDto),
    ),
    ("Session.Message.User", stringify!(SessionMessageUserDto)),
    ("Session.Revert", stringify!(SessionRevertDto)),
    (
        "Session.StructuredError",
        stringify!(SessionStructuredErrorDto),
    ),
    ("SessionActive", stringify!(SessionActiveDto)),
    (
        "SessionMessagesResponse",
        stringify!(SessionMessagesResponseDto),
    ),
    ("SessionNotFoundError", stringify!(SessionNotFoundErrorDto)),
    ("SessionPending.User", stringify!(SessionPendingUserDto)),
    (
        "SessionPending.UserData",
        stringify!(SessionPendingUserDataDto),
    ),
    ("SessionsResponse", stringify!(SessionsResponseDto)),
    ("TokenUsage.Info", stringify!(TokenUsageInfoDto)),
    ("Tool.Content", stringify!(ToolContentDto)),
    ("Tool.FileContent", stringify!(ToolFileContentDto)),
    ("Tool.TextContent", stringify!(ToolTextContentDto)),
    ("UnauthorizedError", stringify!(UnauthorizedErrorDto)),
    ("UnknownError", stringify!(UnknownErrorDto)),
];

fn named_schema_name<T: NamedSchema>() -> &'static str {
    T::OPENAPI_NAME
}

/// Compile-resolved codec census parallel to [`NAMED_SCHEMA_MANIFEST`].
///
/// Each function pointer monomorphizes a concrete Rust type and returns its
/// owned OpenAPI name. A string-only registry cannot satisfy this witness.
pub const NAMED_SCHEMA_CODECS: &[fn() -> &'static str] = &[
    named_schema_name::<AgentColorDto>,
    named_schema_name::<AgentInfoDto>,
    named_schema_name::<ConflictErrorDto>,
    named_schema_name::<FileDiffInfoDto>,
    named_schema_name::<InvalidCursorErrorDto>,
    named_schema_name::<InvalidRequestErrorDto>,
    named_schema_name::<InvalidRequestError1Dto>,
    named_schema_name::<LocationInfoDto>,
    named_schema_name::<LocationRefDto>,
    named_schema_name::<ModelCapabilitiesDto>,
    named_schema_name::<ModelCompatibilityDto>,
    named_schema_name::<ModelCostDto>,
    named_schema_name::<ModelInfoDto>,
    named_schema_name::<ModelReasoningFieldDto>,
    named_schema_name::<ModelRefDto>,
    named_schema_name::<ModelVariantDto>,
    named_schema_name::<MoneyUSDDto>,
    named_schema_name::<MoneyUSDPerMillionTokensDto>,
    named_schema_name::<PermissionEffectDto>,
    named_schema_name::<PermissionRuleDto>,
    named_schema_name::<PermissionRulesetDto>,
    named_schema_name::<ProjectDto>,
    named_schema_name::<ProjectCommandsDto>,
    named_schema_name::<ProjectCurrentDto>,
    named_schema_name::<ProjectIconDto>,
    named_schema_name::<ProjectTimeDto>,
    named_schema_name::<ProjectVcsDto>,
    named_schema_name::<PromptAgentAttachmentDto>,
    named_schema_name::<PromptBase64Dto>,
    named_schema_name::<PromptFileAttachmentDto>,
    named_schema_name::<PromptFileSourceDto>,
    named_schema_name::<PromptMentionDto>,
    named_schema_name::<PromptInputFileAttachmentDto>,
    named_schema_name::<ProviderInfoDto>,
    named_schema_name::<ProviderRequestDto>,
    named_schema_name::<ProviderSettingsDto>,
    named_schema_name::<ServiceHealthDto>,
    named_schema_name::<ServiceUnavailableErrorDto>,
    named_schema_name::<SessionInfoDto>,
    named_schema_name::<SessionMessageAgentSelectedDto>,
    named_schema_name::<SessionMessageAssistantDto>,
    named_schema_name::<SessionMessageAssistantReasoningDto>,
    named_schema_name::<SessionMessageAssistantRetryDto>,
    named_schema_name::<SessionMessageAssistantTextDto>,
    named_schema_name::<SessionMessageAssistantToolDto>,
    named_schema_name::<SessionMessageCompactionDto>,
    named_schema_name::<SessionMessageCompactionCompletedDto>,
    named_schema_name::<SessionMessageCompactionFailedDto>,
    named_schema_name::<SessionMessageCompactionRunningDto>,
    named_schema_name::<SessionMessageInfoDto>,
    named_schema_name::<SessionMessageModelSelectedDto>,
    named_schema_name::<SessionMessageProviderStateDto>,
    named_schema_name::<SessionMessageShellDto>,
    named_schema_name::<SessionMessageSkillDto>,
    named_schema_name::<SessionMessageSyntheticDto>,
    named_schema_name::<SessionMessageSystemDto>,
    named_schema_name::<SessionMessageToolStateCompletedDto>,
    named_schema_name::<SessionMessageToolStateErrorDto>,
    named_schema_name::<SessionMessageToolStateRunningDto>,
    named_schema_name::<SessionMessageToolStateStreamingDto>,
    named_schema_name::<SessionMessageUserDto>,
    named_schema_name::<SessionRevertDto>,
    named_schema_name::<SessionStructuredErrorDto>,
    named_schema_name::<SessionActiveDto>,
    named_schema_name::<SessionMessagesResponseDto>,
    named_schema_name::<SessionNotFoundErrorDto>,
    named_schema_name::<SessionPendingUserDto>,
    named_schema_name::<SessionPendingUserDataDto>,
    named_schema_name::<SessionsResponseDto>,
    named_schema_name::<TokenUsageInfoDto>,
    named_schema_name::<ToolContentDto>,
    named_schema_name::<ToolFileContentDto>,
    named_schema_name::<ToolTextContentDto>,
    named_schema_name::<UnauthorizedErrorDto>,
    named_schema_name::<UnknownErrorDto>,
];

fn presence_rows<T: NamedSchema>() -> &'static [&'static str] {
    T::OPTIONAL_NULLABLE_ROWS
}

fn encoding_receipts<T: NamedSchema>(
) -> Result<Vec<crate::u3a::dto::TriStateEncodingReceipt>, serde_json::Error> {
    T::optional_nullable_encoding_receipts()
}

/// Compile-resolved inventory of every struct carrying at least one frozen
/// optional-nullable field. Empty strings in a carrier's associated slice are
/// non-tri-state sibling fields and are ignored by the evidence census.
pub const OPTIONAL_NULLABLE_CARRIERS: &[fn() -> &'static [&'static str]] = &[
    presence_rows::<ConflictErrorDto>,
    presence_rows::<InvalidRequestErrorDto>,
    presence_rows::<InvalidRequestError1Dto>,
    presence_rows::<ServiceUnavailableErrorDto>,
    presence_rows::<SessionActiveDto>,
    presence_rows::<SessionMessagesResponseDtoCursor>,
    presence_rows::<SessionsResponseDtoCursor>,
    presence_rows::<ToolFileContentDto>,
    presence_rows::<UnknownErrorDto>,
    presence_rows::<ProjectCurrentLocationQueryValueDto>,
    presence_rows::<ProjectCurrentLocationQueryDto>,
    presence_rows::<ProviderLocationQueryValueDto>,
    presence_rows::<ProviderLocationQueryDto>,
    presence_rows::<ModelLocationQueryValueDto>,
    presence_rows::<ModelLocationQueryDto>,
    presence_rows::<ModelDefaultLocationQueryValueDto>,
    presence_rows::<ModelDefaultLocationQueryDto>,
    presence_rows::<AgentLocationQueryValueDto>,
    presence_rows::<AgentLocationQueryDto>,
    presence_rows::<SessionListQueryDto>,
    presence_rows::<MessageListQueryDto>,
    presence_rows::<SessionCreateRequestDto>,
    presence_rows::<PromptRequestDto>,
    presence_rows::<InterruptRequestDto>,
];

pub type EncodingCarrier =
    fn() -> Result<Vec<crate::u3a::dto::TriStateEncodingReceipt>, serde_json::Error>;

pub const OPTIONAL_NULLABLE_ENCODING_CARRIERS: &[EncodingCarrier] = &[
    encoding_receipts::<ConflictErrorDto>,
    encoding_receipts::<InvalidRequestErrorDto>,
    encoding_receipts::<InvalidRequestError1Dto>,
    encoding_receipts::<ServiceUnavailableErrorDto>,
    encoding_receipts::<SessionActiveDto>,
    encoding_receipts::<SessionMessagesResponseDtoCursor>,
    encoding_receipts::<SessionsResponseDtoCursor>,
    encoding_receipts::<ToolFileContentDto>,
    encoding_receipts::<UnknownErrorDto>,
    encoding_receipts::<ProjectCurrentLocationQueryValueDto>,
    encoding_receipts::<ProjectCurrentLocationQueryDto>,
    encoding_receipts::<ProviderLocationQueryValueDto>,
    encoding_receipts::<ProviderLocationQueryDto>,
    encoding_receipts::<ModelLocationQueryValueDto>,
    encoding_receipts::<ModelLocationQueryDto>,
    encoding_receipts::<ModelDefaultLocationQueryValueDto>,
    encoding_receipts::<ModelDefaultLocationQueryDto>,
    encoding_receipts::<AgentLocationQueryValueDto>,
    encoding_receipts::<AgentLocationQueryDto>,
    encoding_receipts::<SessionListQueryDto>,
    encoding_receipts::<MessageListQueryDto>,
    encoding_receipts::<SessionCreateRequestDto>,
    encoding_receipts::<PromptRequestDto>,
    encoding_receipts::<InterruptRequestDto>,
];

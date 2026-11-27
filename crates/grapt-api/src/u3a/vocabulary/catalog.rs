use super::prelude::*;
use grapt_schema::{Optional, Required};

bounded_id! { pub struct IntegrationIdDto }
bounded_id! { pub struct WorkspaceIdDto => "wrk" }

closed_object! { pub struct CommandInfoDto => "Command.Info" {
    required name: DtoString => "name",
    required template: DtoString => "template",
    optional description: DtoString => "description",
    optional agent: WireId<grapt_types::AgentId> => "agent",
    optional model: ModelRefDto => "model",
    optional subtask: bool => "subtask",
} }

closed_object! { pub struct McpPromptArgumentDto => "inline:mcp-prompt-argument" {
    required name: BoundedString<128> => "name",
    required required: bool => "required",
    optional description: BoundedString<1024> => "description",
} }
closed_object! { pub struct McpPromptBindingDto => "inline:mcp-prompt-binding" {
    required server: BoundedString<256> => "server",
    required name: BoundedString<128> => "name",
    required arguments: BoundedVec<McpPromptArgumentDto, 32> => "arguments",
} }

closed_object! { pub struct ClaudePluginPathsDto => "inline:claude-plugin-paths" {
    required root: BoundedString<4096> => "root",
    required data: BoundedString<4096> => "data",
    required project: BoundedString<4096> => "project",
} }

// Native command inspection preserves provenance outside the retained closed DTO.
string_enum! { pub enum CommandOriginDto {
    Builtin => "builtin", User => "user", Project => "project", Mcp => "mcp"
} }
closed_object! { pub struct CommandInspectionDto => "inline:command-inspection" {
    required command: CommandInfoDto => "command",
    required origin: CommandOriginDto => "origin",
    required source: DtoString => "source",
    optional unavailable: DtoString => "unavailable",
    optional mcp: McpPromptBindingDto => "mcp",
    optional claude: ClaudePluginPathsDto => "claude",
} }
closed_object! { pub struct CommandInspectionResponseDto => "inline:command-inspection-response" {
    required data: BoundedVec<CommandInspectionDto, MAX_CATALOG_ROWS> => "data",
    optional warnings: BoundedVec<BoundedString<4096>, 1024> => "warnings",
    optional mcp_scope: BoundedString<64> => "mcp_scope",
    required location: LocationInfoDto => "location",
} }

closed_object! { pub struct SkillInfoDto => "Skill.Info" {
    required id: DtoString => "id",
    required name: DtoString => "name",
    optional description: DtoString => "description",
    optional slash: bool => "slash",
    optional autoinvoke: bool => "autoinvoke",
    required location: DtoString => "location",
    required content: DtoString => "content",
} }

string_enum! { pub enum ReferenceLocalType { Local => "local" } }
string_enum! { pub enum ReferenceGitType { Git => "git" } }
closed_object! { pub struct ReferenceLocalSourceDto => "Reference.LocalSource" {
    required kind: ReferenceLocalType => "type",
    required path: DtoString => "path",
    optional description: DtoString => "description",
    optional hidden: bool => "hidden",
} }
closed_object! { pub struct ReferenceGitSourceDto => "Reference.GitSource" {
    required kind: ReferenceGitType => "type",
    required repository: DtoString => "repository",
    optional branch: DtoString => "branch",
    optional description: DtoString => "description",
    optional hidden: bool => "hidden",
} }
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum ReferenceSourceDto {
    Local(Box<ReferenceLocalSourceDto>),
    Git(Box<ReferenceGitSourceDto>),
}

impl NamedSchema for ReferenceSourceDto {
    const OPENAPI_NAME: &'static str = "Reference.Source";
}

#[cfg(test)]
mod reference_source_tests {
    use super::ReferenceSourceDto;

    #[test]
    fn tag_order_does_not_open_the_reference_variant_or_erase_presence(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (input, expected) in [
            (
                r#"{"path":"guide.md","hidden":false,"type":"local"}"#,
                serde_json::json!({"type":"local","path":"guide.md","hidden":false}),
            ),
            (
                r#"{"branch":"dev","repository":"https://example.invalid/repo","type":"git"}"#,
                serde_json::json!({"type":"git","repository":"https://example.invalid/repo","branch":"dev"}),
            ),
        ] {
            let source: ReferenceSourceDto = serde_json::from_str(input)?;
            assert_eq!(serde_json::to_value(source)?, expected);
        }
        for invalid in [
            r#"{"type":"local"}"#,
            r#"{"type":"git"}"#,
            r#"{"path":"a","repository":"b","type":"local"}"#,
            r#"{"path":"a","branch":"dev","type":"local"}"#,
            r#"{"repository":"b","path":"a","type":"git"}"#,
            r#"{"path":"a","type":"local","surprise":true}"#,
            r#"{"path":"a","path":"b","type":"local"}"#,
            r#"{"path":"a","type":"local","type":"git"}"#,
            r#"{"path":"a","hidden":null,"type":"local"}"#,
            r#"{"path":"a","hidden":"false","type":"local"}"#,
            r#"{"path":"a","type":"unknown"}"#,
        ] {
            assert!(
                serde_json::from_str::<ReferenceSourceDto>(invalid).is_err(),
                "reference variant accepted an invalid field or presence: {invalid}"
            );
        }
        Ok(())
    }
}

impl<'de> serde::Deserialize<'de> for ReferenceSourceDto {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SourceVisitor;
        impl<'de> serde::de::Visitor<'de> for SourceVisitor {
            type Value = ReferenceSourceDto;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded local or Git reference source")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                const FIELDS: &[&str] = &[
                    "type",
                    "path",
                    "repository",
                    "branch",
                    "description",
                    "hidden",
                ];
                fn field<'de, A, T>(
                    map: &mut A,
                    slot: &mut Option<T>,
                    name: &'static str,
                ) -> Result<(), A::Error>
                where
                    A: serde::de::MapAccess<'de>,
                    T: serde::Deserialize<'de>,
                {
                    if slot.is_some() {
                        return Err(<A::Error as serde::de::Error>::duplicate_field(name));
                    }
                    *slot = Some(map.next_value()?);
                    Ok(())
                }
                let mut tag: Option<DtoString> = None;
                let mut path: Option<DtoString> = None;
                let mut repository: Option<DtoString> = None;
                let mut branch: Option<DtoString> = None;
                let mut description: Option<DtoString> = None;
                let mut hidden: Option<bool> = None;
                while let Some(key) = map.next_key::<BoundedString<256>>()? {
                    // Capture only the six declared scalar fields, within
                    // their type bounds, before dispatching on a tag that may
                    // occur anywhere in the JSON object.
                    match key.as_str() {
                        "type" => field(&mut map, &mut tag, "type")?,
                        "path" => field(&mut map, &mut path, "path")?,
                        "repository" => field(&mut map, &mut repository, "repository")?,
                        "branch" => field(&mut map, &mut branch, "branch")?,
                        "description" => field(&mut map, &mut description, "description")?,
                        "hidden" => field(&mut map, &mut hidden, "hidden")?,
                        other => {
                            return Err(<A::Error as serde::de::Error>::unknown_field(
                                other, FIELDS,
                            ))
                        }
                    }
                }
                let tag =
                    tag.ok_or_else(|| <A::Error as serde::de::Error>::missing_field("type"))?;
                let description = description.map_or_else(Optional::absent, Optional::present);
                let hidden = hidden.map_or_else(Optional::absent, Optional::present);
                match tag.as_str() {
                    "local" => {
                        for (present, field) in [
                            (repository.is_some(), "repository"),
                            (branch.is_some(), "branch"),
                        ] {
                            if present {
                                return Err(<A::Error as serde::de::Error>::unknown_field(
                                    field,
                                    &["type", "path", "description", "hidden"],
                                ));
                            }
                        }
                        let path = path
                            .ok_or_else(|| <A::Error as serde::de::Error>::missing_field("path"))?;
                        Ok(ReferenceSourceDto::Local(Box::new(
                            ReferenceLocalSourceDto::new(
                                Required::new(ReferenceLocalType::Local),
                                Required::new(path),
                                description,
                                hidden,
                            ),
                        )))
                    }
                    "git" => {
                        if path.is_some() {
                            return Err(<A::Error as serde::de::Error>::unknown_field(
                                "path",
                                &["type", "repository", "branch", "description", "hidden"],
                            ));
                        }
                        let repository = repository.ok_or_else(|| {
                            <A::Error as serde::de::Error>::missing_field("repository")
                        })?;
                        Ok(ReferenceSourceDto::Git(Box::new(
                            ReferenceGitSourceDto::new(
                                Required::new(ReferenceGitType::Git),
                                Required::new(repository),
                                branch.map_or_else(Optional::absent, Optional::present),
                                description,
                                hidden,
                            ),
                        )))
                    }
                    other => Err(<A::Error as serde::de::Error>::unknown_variant(
                        other,
                        &["local", "git"],
                    )),
                }
            }
        }
        deserializer.deserialize_map(SourceVisitor)
    }
}
closed_object! { pub struct ReferenceInfoDto => "Reference.Info" {
    required name: DtoString => "name",
    required path: DtoString => "path",
    optional description: DtoString => "description",
    optional hidden: bool => "hidden",
    required source: ReferenceSourceDto => "source",
} }

// Rust-owned IA0 non-SSE wire vocabulary.
//
// These declarations are reviewed source, not generated output. The frozen
// OpenAPI digest is evidence for the test-side census only; production does
// not read that artifact or dispatch through a runtime schema registry.

named_newtype! { pub struct AgentColorDto(DtoString) => "Agent.Color" }

string_enum! {
    pub enum AgentInfoDtoMode {
        Subagent => "subagent",
        Primary => "primary",
        All => "all",
    }
}

closed_object! {
    pub struct AgentInfoDto => "Agent.Info" {
        required id: WireId<grapt_types::AgentId> => "id",
        required name: DtoString => "name",
        optional model: ModelRefDto => "model",
        optional subagent_model: ModelRefDto => "subagentModel",
        required request: ProviderRequestDto => "request",
        optional system: DtoString => "system",
        optional description: DtoString => "description",
        optional source: DtoString => "source",
        required mode: AgentInfoDtoMode => "mode",
        required hidden: bool => "hidden",
        optional color: AgentColorDto => "color",
        optional steps: PositiveU64 => "steps",
        required permissions: PermissionRulesetDto => "permissions",
    }
}

string_enum! {
    pub enum ConflictErrorDtoTag {
        ConflictError => "ConflictError",
    }
}

closed_object! {
    pub struct ConflictErrorDto => "ConflictError" {
        required tag: ConflictErrorDtoTag => "_tag",
        required message: DtoString => "message",
        optional_nullable(grapt_schema::tri_state_ledger::U3aConflictErrorResource) resource: DtoString => "resource",
    }
}

string_enum! {
    pub enum FileDiffInfoDtoStatus {
        Added => "added",
        Deleted => "deleted",
        Modified => "modified",
    }
}

closed_object! {
    pub struct FileDiffInfoDto => "FileDiff.Info" {
        required file: DtoString => "file",
        required patch: DtoString => "patch",
        required additions: NonNegativeU64 => "additions",
        required deletions: NonNegativeU64 => "deletions",
        required status: FileDiffInfoDtoStatus => "status",
    }
}

string_enum! {
    pub enum InvalidCursorErrorDtoTag {
        InvalidCursorError => "InvalidCursorError",
    }
}

closed_object! {
    pub struct InvalidCursorErrorDto => "InvalidCursorError" {
        required tag: InvalidCursorErrorDtoTag => "_tag",
        required message: DtoString => "message",
    }
}

string_enum! {
    pub enum InvalidRequestErrorDtoTag {
        InvalidRequestError => "InvalidRequestError",
    }
}

closed_object! {
    pub struct InvalidRequestErrorDto => "InvalidRequestError" {
        required tag: InvalidRequestErrorDtoTag => "_tag",
        required message: DtoString => "message",
        optional_nullable(grapt_schema::tri_state_ledger::U3aInvalidRequestErrorKind) kind: DtoString => "kind",
        optional_nullable(grapt_schema::tri_state_ledger::U3aInvalidRequestErrorField) field: DtoString => "field",
    }
}

string_enum! {
    pub enum InvalidRequestError1DtoTag {
        InvalidRequestError => "InvalidRequestError",
    }
}

closed_object! {
    pub struct InvalidRequestError1Dto => "InvalidRequestError1" {
        required tag: InvalidRequestError1DtoTag => "_tag",
        required message: DtoString => "message",
        optional_nullable(grapt_schema::tri_state_ledger::U3aInvalidRequestError1Kind) kind: DtoString => "kind",
        optional_nullable(grapt_schema::tri_state_ledger::U3aInvalidRequestError1Field) field: DtoString => "field",
    }
}

closed_object! {
    pub struct LocationInfoDtoProject => "inline:LocationInfoDtoProject" {
        required id: WireId<grapt_types::ProjectId> => "id",
        required directory: DtoString => "directory",
    }
}

closed_object! {
    pub struct LocationInfoDto => "Location.Info" {
        required directory: DtoString => "directory",
        optional workspace_id: WorkspaceIdDto => "workspaceID",
        required project: LocationInfoDtoProject => "project",
    }
}

closed_object! {
    pub struct LocationRefDto => "Location.Ref" {
        required directory: DtoString => "directory",
        optional workspace_id: WorkspaceIdDto => "workspaceID",
    }
}

closed_object! {
    pub struct ModelCapabilitiesDto => "Model.Capabilities" {
        required tools: bool => "tools",
        required input: BoundedVec<DtoString, MAX_NESTED_ITEMS> => "input",
        required output: BoundedVec<DtoString, MAX_NESTED_ITEMS> => "output",
    }
}

closed_object! {
    pub struct ModelCompatibilityDto => "Model.Compatibility" {
        optional reasoning_field: ModelReasoningFieldDto => "reasoningField",
    }
}

string_enum! {
    pub enum ModelCostDtoTierType {
        Context => "context",
    }
}

closed_object! {
    pub struct ModelCostDtoTier => "inline:ModelCostDtoTier" {
        required kind: ModelCostDtoTierType => "type",
        required size: WireI64 => "size",
    }
}

closed_object! {
    pub struct ModelCostDtoCache => "inline:ModelCostDtoCache" {
        required read: MoneyUSDPerMillionTokensDto => "read",
        required write: MoneyUSDPerMillionTokensDto => "write",
    }
}

closed_object! {
    pub struct ModelCostDto => "Model.Cost" {
        optional tier: ModelCostDtoTier => "tier",
        required input: MoneyUSDPerMillionTokensDto => "input",
        required output: MoneyUSDPerMillionTokensDto => "output",
        required cache: ModelCostDtoCache => "cache",
    }
}

closed_object! {
    pub struct ModelInfoDtoTime => "inline:ModelInfoDtoTime" {
        required released: FiniteF64 => "released",
    }
}

string_enum! {
    pub enum ModelInfoDtoStatus {
        Alpha => "alpha",
        Beta => "beta",
        Deprecated => "deprecated",
        Active => "active",
    }
}

closed_object! {
    pub struct ModelInfoDtoLimit => "inline:ModelInfoDtoLimit" {
        required context: WireI64 => "context",
        optional input: WireI64 => "input",
        required output: WireI64 => "output",
    }
}

closed_object! {
    pub struct ModelInfoDto => "Model.Info" {
        required id: WireId<grapt_types::ModelId> => "id",
        required model_id: WireId<grapt_types::ModelId> => "modelID",
        required provider_id: WireId<grapt_types::ProviderId> => "providerID",
        optional family: DtoString => "family",
        required name: DtoString => "name",
        optional compatibility: ModelCompatibilityDto => "compatibility",
        optional package: DtoString => "package",
        optional settings: OpenObject => "settings",
        optional headers: BoundedMap<DtoString, DtoString, MAX_METADATA_KEYS> => "headers",
        optional body: OpenObject => "body",
        required capabilities: ModelCapabilitiesDto => "capabilities",
        required variants: BoundedVec<ModelVariantDto, MAX_NESTED_ITEMS> => "variants",
        required time: ModelInfoDtoTime => "time",
        required cost: BoundedVec<ModelCostDto, MAX_NESTED_ITEMS> => "cost",
        required status: ModelInfoDtoStatus => "status",
        required enabled: bool => "enabled",
        required limit: ModelInfoDtoLimit => "limit",
    }
}

named_newtype! { pub struct ModelReasoningFieldDto(DtoString) => "Model.ReasoningField" }

closed_object! {
    pub struct ModelRefDto => "Model.Ref" {
        required id: WireId<grapt_types::ModelId> => "id",
        required provider_id: WireId<grapt_types::ProviderId> => "providerID",
        optional variant: WireId<grapt_types::ModelVariantId> => "variant",
    }
}

closed_object! {
    pub struct ModelVariantDto => "Model.Variant" {
        required id: WireId<grapt_types::ModelVariantId> => "id",
        optional settings: OpenObject => "settings",
        optional headers: BoundedMap<DtoString, DtoString, MAX_METADATA_KEYS> => "headers",
        optional body: OpenObject => "body",
    }
}

named_newtype! { pub struct MoneyUSDDto(FiniteF64) => "Money.USD" }

named_newtype! { pub struct MoneyUSDPerMillionTokensDto(FiniteF64) => "Money.USDPerMillionTokens" }

string_enum! {
    pub enum PermissionEffectDto {
        Allow => "allow",
        Deny => "deny",
        Ask => "ask",
    }
}

impl NamedSchema for PermissionEffectDto {
    const OPENAPI_NAME: &'static str = "Permission.Effect";
}

closed_object! {
    pub struct PermissionRuleDto => "Permission.Rule" {
        required action: DtoString => "action",
        required resource: DtoString => "resource",
        required effect: PermissionEffectDto => "effect",
    }
}

named_newtype! { pub struct PermissionRulesetDto(BoundedVec<PermissionRuleDto, MAX_NESTED_ITEMS>) => "Permission.Ruleset" }

closed_object! {
    pub struct ProjectDto => "Project" {
        required id: WireId<grapt_types::ProjectId> => "id",
        required worktree: DtoString => "worktree",
        optional vcs: ProjectVcsDto => "vcs",
        optional name: DtoString => "name",
        optional icon: ProjectIconDto => "icon",
        optional commands: ProjectCommandsDto => "commands",
        required time: ProjectTimeDto => "time",
        required sandboxes: BoundedVec<DtoString, MAX_NESTED_ITEMS> => "sandboxes",
    }
}

closed_object! {
    pub struct ProjectCommandsDto => "Project.Commands" {
        optional start: DtoString => "start",
    }
}

closed_object! {
    pub struct ProjectCurrentDto => "Project.Current" {
        required id: WireId<grapt_types::ProjectId> => "id",
        required directory: DtoString => "directory",
    }
}

closed_object! {
    pub struct ProjectIconDto => "Project.Icon" {
        optional url: DtoString => "url",
        optional override_value: DtoString => "override",
        optional color: DtoString => "color",
    }
}

closed_object! {
    pub struct ProjectTimeDto => "Project.Time" {
        required created: NonNegativeU64 => "created",
        required updated: NonNegativeU64 => "updated",
        optional initialized: NonNegativeU64 => "initialized",
    }
}

string_enum! {
    pub enum ProjectVcsDto {
        Git => "git",
        Hg => "hg",
    }
}

impl NamedSchema for ProjectVcsDto {
    const OPENAPI_NAME: &'static str = "Project.Vcs";
}

closed_object! {
    pub struct PromptAgentAttachmentDto => "Prompt.AgentAttachment" {
        required name: DtoString => "name",
        optional mention: PromptMentionDto => "mention",
    }
}

named_newtype! { pub struct PromptBase64Dto(Base64Dto) => "Prompt.Base64" }

closed_object! {
    pub struct PromptFileAttachmentDto => "Prompt.FileAttachment" {
        required data: PromptBase64Dto => "data",
        required mime: DtoString => "mime",
        required source: PromptFileSourceDto => "source",
        optional name: DtoString => "name",
        optional description: DtoString => "description",
        optional mention: PromptMentionDto => "mention",
    }
}

string_enum! {
    pub enum PromptFileSourceDtoInlineType {
        Inline => "inline",
    }
}

closed_object! {
    pub struct PromptFileSourceDtoInline => "inline:PromptFileSourceDtoInline" {
        required kind: PromptFileSourceDtoInlineType => "type",
    }
}

string_enum! {
    pub enum PromptFileSourceDtoUriType {
        Uri => "uri",
    }
}

closed_object! {
    pub struct PromptFileSourceDtoUri => "inline:PromptFileSourceDtoUri" {
        required kind: PromptFileSourceDtoUriType => "type",
        required uri: DtoString => "uri",
    }
}

tagged_union! {
    pub enum PromptFileSourceDto => "Prompt.FileSource", tag "type" {
        Inline(PromptFileSourceDtoInline) => "inline",
        Uri(PromptFileSourceDtoUri) => "uri",
    }
}

closed_object! {
    pub struct PromptMentionDto => "Prompt.Mention" {
        required start: FiniteF64 => "start",
        required end: FiniteF64 => "end",
        required text: DtoString => "text",
    }
}

closed_object! {
    pub struct PromptInputFileAttachmentDto => "PromptInput.FileAttachment" {
        required uri: BoundedString<1048576> => "uri",
        optional name: DtoString => "name",
        optional description: DtoString => "description",
        optional mention: PromptMentionDto => "mention",
    }
}

closed_object! {
    pub struct ProviderInfoDto => "Provider.Info" {
        required id: WireId<grapt_types::ProviderId> => "id",
        optional integration_id: IntegrationIdDto => "integrationID",
        required name: DtoString => "name",
        optional disabled: bool => "disabled",
        required package: DtoString => "package",
        optional settings: OpenObject => "settings",
        optional headers: BoundedMap<DtoString, DtoString, MAX_METADATA_KEYS> => "headers",
        optional body: OpenObject => "body",
    }
}

closed_object! {
    pub struct ProviderRequestDto => "Provider.Request" {
        required settings: ProviderSettingsDto => "settings",
        required headers: BoundedMap<DtoString, DtoString, MAX_METADATA_KEYS> => "headers",
        required body: OpenObject => "body",
    }
}

named_newtype! { pub struct ProviderSettingsDto(OpenObject) => "Provider.Settings" }

closed_object! {
    pub struct ServiceHealthDto => "ServiceHealth" {
        required healthy: TrueValue => "healthy",
        required version: DtoString => "version",
        required pid: PositiveU64 => "pid",
    }
}

#[cfg(test)]
#[path = "catalog/renderer_health_tests.rs"]
mod renderer_health_tests;

string_enum! {
    pub enum ServiceUnavailableErrorDtoTag {
        ServiceUnavailableError => "ServiceUnavailableError",
    }
}

closed_object! {
    pub struct ServiceUnavailableErrorDto => "ServiceUnavailableError" {
        required tag: ServiceUnavailableErrorDtoTag => "_tag",
        required message: DtoString => "message",
        optional_nullable(grapt_schema::tri_state_ledger::U3aServiceUnavailableErrorService) service: DtoString => "service",
    }
}

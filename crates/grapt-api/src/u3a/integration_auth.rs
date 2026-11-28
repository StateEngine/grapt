//! Retained integration authentication wire contract shared by desktop and TUI.

use super::connections::connection_union;
use super::prelude::*;
use super::{IntegrationInfoDto, LocationInfoDto};

/// A provider refused the submitted key during its network verification.
pub const INTEGRATION_KEY_REJECTED_KIND: &str = "integration_key_rejected";
/// An endpoint connect refused for a reason the product states in its own
/// words, such as a server that cannot be reached or serves no usable model.
pub const INTEGRATION_ENDPOINT_KIND: &str = "integration_endpoint";
pub const INTEGRATION_AZURE_CREDENTIALS_KIND: &str = "azure_credentials_unavailable";
pub const INTEGRATION_AZURE_CREDENTIALS_MESSAGE: &str = "Azure CLI could not authorize this deployment; install Azure CLI 2.54 or newer, run az login as the service user, check the Azure resource endpoint, deployment and permissions, then retry Connect";
pub const INTEGRATION_GOOGLE_ADC_KIND: &str = "google_adc_verification_failed";
pub const INTEGRATION_GOOGLE_ADC_MESSAGE: &str = "Google application-default verification failed; external-account credential sources are unsupported. Run gcloud auth application-default login or configure a service-account JSON file with GOOGLE_APPLICATION_CREDENTIALS, check the Vertex project permissions and model location, then retry Connect";
pub const INTEGRATION_AWS_PROFILE_KIND: &str = "aws_profile_configuration";
pub const INTEGRATION_AWS_PROFILE_MESSAGE: &str = "AWS profile configuration could not be used; credential_process is unsupported. Use environment credentials, a shared static profile, SSO, web identity or an IAM role, then retry Connect";
pub const INTEGRATION_AWS_CREDENTIALS_KIND: &str = "aws_credentials_unavailable";
pub const INTEGRATION_AWS_CREDENTIALS_MESSAGE: &str = "AWS credentials could not authorize Bedrock; configure the selected AWS profile or default chain, check Bedrock permissions and the system clock, then retry Connect";

named_newtype! { pub struct IntegrationAttemptIdDto(DtoString) => "Integration.AttemptID" }
string_enum! { pub enum IntegrationWhenOperator { Eq => "eq", Neq => "neq" } }
string_enum! { pub enum IntegrationTextPromptType { Text => "text" } }
string_enum! { pub enum IntegrationSelectPromptType { Select => "select" } }
string_enum! { pub enum IntegrationOAuthMethodType { OAuth => "oauth" } }
string_enum! { pub enum IntegrationCommandMethodType { Command => "command" } }
string_enum! { pub enum IntegrationOAuthMode { Auto => "auto", Code => "code" } }
closed_object! { pub struct IntegrationWhenDto => "Integration.When" {
    required key: DtoString => "key",
    required op: IntegrationWhenOperator => "op",
    required value: DtoString => "value",
} }
closed_object! { pub struct IntegrationTextPromptDto => "Integration.TextPrompt" {
    required kind: IntegrationTextPromptType => "type",
    required key: DtoString => "key",
    required message: DtoString => "message",
    optional placeholder: DtoString => "placeholder",
    optional when: IntegrationWhenDto => "when",
} }
closed_object! { pub struct IntegrationSelectOptionDto => "inline:integration-select-option" {
    required label: DtoString => "label",
    required value: DtoString => "value",
    optional hint: DtoString => "hint",
} }
closed_object! { pub struct IntegrationSelectPromptDto => "Integration.SelectPrompt" {
    required kind: IntegrationSelectPromptType => "type",
    required key: DtoString => "key",
    required message: DtoString => "message",
    required options: BoundedVec<IntegrationSelectOptionDto, MAX_NESTED_ITEMS> => "options",
    optional when: IntegrationWhenDto => "when",
} }
connection_union!(IntegrationPromptDto, "type",
    Text(IntegrationTextPromptDto) => "text", Select(IntegrationSelectPromptDto) => "select",
);
closed_object! { pub struct IntegrationOAuthMethodDto => "Integration.OAuthMethod" {
    required id: DtoString => "id",
    required kind: IntegrationOAuthMethodType => "type",
    required label: DtoString => "label",
    optional prompts: BoundedVec<IntegrationPromptDto, MAX_NESTED_ITEMS> => "prompts",
} }
closed_object! { pub struct IntegrationCommandMethodDto => "Integration.CommandMethod" {
    required id: DtoString => "id",
    required kind: IntegrationCommandMethodType => "type",
    required label: DtoString => "label",
    required command: BoundedVec<DtoString, MAX_NESTED_ITEMS> => "command",
} }
closed_object! { pub struct IntegrationGetResponseDto => "inline:integration-get-response" {
    nullable data: IntegrationInfoDto => "data",
    required location: LocationInfoDto => "location",
} }
closed_object! { pub struct IntegrationAttemptTimeDto => "inline:integration-attempt-time" {
    required created: FiniteOrSentinelF64 => "created",
    required expires: FiniteOrSentinelF64 => "expires",
} }
closed_object! { pub struct IntegrationOAuthAttemptDto => "inline:integration-oauth-attempt" {
    required attempt_id: IntegrationAttemptIdDto => "attemptID",
    required url: DtoString => "url",
    required instructions: DtoString => "instructions",
    required mode: IntegrationOAuthMode => "mode",
    required time: IntegrationAttemptTimeDto => "time",
} }
closed_object! { pub struct IntegrationCommandAttemptDto => "Integration.CommandAttempt" {
    required attempt_id: IntegrationAttemptIdDto => "attemptID",
    required time: IntegrationAttemptTimeDto => "time",
} }
string_enum! { pub enum IntegrationPendingStatus { Pending => "pending" } }
string_enum! { pub enum IntegrationCompleteStatus { Complete => "complete" } }
string_enum! { pub enum IntegrationFailedStatus { Failed => "failed" } }
string_enum! { pub enum IntegrationExpiredStatus { Expired => "expired" } }
closed_object! { pub struct IntegrationPendingDto => "inline:integration-pending" {
    required status: IntegrationPendingStatus => "status",
    required time: IntegrationAttemptTimeDto => "time",
} }
closed_object! { pub struct IntegrationCommandPendingDto => "inline:integration-command-pending" {
    required status: IntegrationPendingStatus => "status",
    optional message: DtoString => "message",
    required time: IntegrationAttemptTimeDto => "time",
} }
closed_object! { pub struct IntegrationCompleteDto => "inline:integration-complete" {
    required status: IntegrationCompleteStatus => "status",
    required time: IntegrationAttemptTimeDto => "time",
} }
closed_object! { pub struct IntegrationFailedDto => "inline:integration-failed" {
    required status: IntegrationFailedStatus => "status",
    required message: DtoString => "message",
    required time: IntegrationAttemptTimeDto => "time",
} }
closed_object! { pub struct IntegrationExpiredDto => "inline:integration-expired" {
    required status: IntegrationExpiredStatus => "status",
    required time: IntegrationAttemptTimeDto => "time",
} }
connection_union!(IntegrationAttemptStatusDto, "status",
    Pending(IntegrationPendingDto) => "pending", Complete(IntegrationCompleteDto) => "complete",
    Failed(IntegrationFailedDto) => "failed", Expired(IntegrationExpiredDto) => "expired",
);
connection_union!(IntegrationCommandAttemptStatusDto, "status",
    Pending(IntegrationCommandPendingDto) => "pending", Complete(IntegrationCompleteDto) => "complete",
    Failed(IntegrationFailedDto) => "failed", Expired(IntegrationExpiredDto) => "expired",
);
closed_object! { pub struct IntegrationOAuthConnectResponseDto => "inline:integration-oauth-connect-response" {
    required data: IntegrationOAuthAttemptDto => "data",
    required location: LocationInfoDto => "location",
} }
closed_object! { pub struct IntegrationOAuthStatusResponseDto => "inline:integration-oauth-status-response" {
    required data: IntegrationAttemptStatusDto => "data",
    required location: LocationInfoDto => "location",
} }
closed_object! { pub struct IntegrationCommandConnectResponseDto => "inline:integration-command-connect-response" {
    required data: IntegrationCommandAttemptDto => "data",
    required location: LocationInfoDto => "location",
} }
closed_object! { pub struct IntegrationCommandStatusResponseDto => "inline:integration-command-status-response" {
    required data: IntegrationCommandAttemptStatusDto => "data",
    required location: LocationInfoDto => "location",
} }
closed_object! { pub struct CredentialUpdateRequestDto => "inline:credential-update-request" {
    required label: DtoString => "label",
} }

// Shared clients serialize these only as outbound requests. Debug output is
// redacted; typed connect handlers return NoContent and never return a request.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationKeyConnectRequestDto {
    pub key: DtoString,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub cloudflare: grapt_schema::Optional<IntegrationCloudflareGatewayConnectDto>,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub vertex: grapt_schema::Optional<VertexProviderRequestDto>,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub watsonx: grapt_schema::Optional<WatsonxProviderRequestDto>,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub label: grapt_schema::Optional<DtoString>,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub custom: grapt_schema::Optional<CustomProviderRequestDto>,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub bedrock: grapt_schema::Optional<BedrockProviderRequestDto>,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub databricks: grapt_schema::Optional<DatabricksProviderRequestDto>,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub azure: grapt_schema::Optional<AzureProviderRequestDto>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AzureProviderRequestDto {
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub auth: grapt_schema::Optional<AzureAuthRequestDto>,
    pub endpoint: DtoString,
    pub deployment: DtoString,
    pub model: DtoString,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AzureAuthRequestDto {
    ApiKey,
    AzureCli,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomProviderRequestDto {
    pub endpoint: DtoString,
    /// Absent: the endpoint's own model list names every model it serves.
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub model: grapt_schema::Optional<DtoString>,
    /// Absent: each model's window is what the endpoint reports, or unknown.
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub context_window: grapt_schema::Optional<u32>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabricksProviderRequestDto {
    pub workspace: DtoString,
    pub model: DtoString,
    pub context_window: u32,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BedrockAuthRequestDto {
    Bearer,
    AwsChain {
        #[serde(
            default = "grapt_schema::Optional::absent",
            skip_serializing_if = "grapt_schema::Optional::is_absent"
        )]
        profile: grapt_schema::Optional<DtoString>,
    },
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BedrockProviderRequestDto {
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub auth: grapt_schema::Optional<BedrockAuthRequestDto>,
    pub region: DtoString,
    pub model: DtoString,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub endpoint: grapt_schema::Optional<DtoString>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationCloudflareGatewayConnectDto {
    pub account: DtoString,
    pub gateway: DtoString,
    pub provider: DtoString,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VertexAuthRequestDto {
    ServiceAccount,
    ApplicationDefault,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VertexProviderRequestDto {
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub auth: grapt_schema::Optional<VertexAuthRequestDto>,
    pub project: DtoString,
    pub location: DtoString,
    pub model: DtoString,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatsonxProviderRequestDto {
    pub region: DtoString,
    pub project: DtoString,
    pub model: DtoString,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationOAuthConnectRequestDto {
    #[serde(rename = "methodID")]
    pub method_id: DtoString,
    pub inputs: BoundedMap<DtoString, DtoString, MAX_METADATA_KEYS>,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub label: grapt_schema::Optional<DtoString>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationOAuthCompleteRequestDto {
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub code: grapt_schema::Optional<DtoString>,
}
closed_object! { pub struct IntegrationCommandConnectRequestDto => "inline:integration-command-connect-request" {
    required method_id: DtoString => "methodID",
    optional label: DtoString => "label",
} }

macro_rules! redacted_debug {
    ($($name:ident),+ $(,)?) => {$(
        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), " { [redacted] }"))
            }
        }
    )+};
}
redacted_debug!(
    IntegrationKeyConnectRequestDto,
    IntegrationOAuthConnectRequestDto,
    IntegrationOAuthCompleteRequestDto
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_auth_shapes_support_prompts_attempts_and_redacted_requests(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let method = serde_json::json!({
            "type":"oauth", "id":"chatgpt", "label":"ChatGPT",
            "prompts":[{"type":"select", "key":"flow", "message":"Sign in",
                "options":[{"label":"Browser", "value":"browser"}],
                "when":{"key":"account", "op":"neq", "value":"managed"}}]
        });
        let decoded: super::super::IntegrationMethodDto = serde_json::from_value(method.clone())?;
        assert_eq!(serde_json::to_value(decoded)?, method);
        for status in ["pending", "complete", "expired", "failed"] {
            let mut value =
                serde_json::json!({"status":status,"time":{"created":1.0,"expires":2.0}});
            if status == "failed" {
                value["message"] = "Sign-in failed".into();
            }
            let decoded: IntegrationAttemptStatusDto = serde_json::from_value(value.clone())?;
            assert_eq!(serde_json::to_value(decoded)?, value);
        }
        assert!(serde_json::from_value::<IntegrationAttemptStatusDto>(
            serde_json::json!({"status":"failed","time":{"created":1.0,"expires":2.0}})
        )
        .is_err());
        let request: IntegrationKeyConnectRequestDto =
            serde_json::from_str(r#"{"key":"private-key","label":"Work"}"#)?;
        assert!(!format!("{request:?}").contains("private-key"));
        let unlabeled: IntegrationKeyConnectRequestDto =
            serde_json::from_str(r#"{"key":"outbound-key"}"#)?;
        assert_eq!(
            serde_json::to_value(unlabeled)?,
            serde_json::json!({"key":"outbound-key"})
        );
        assert!(serde_json::from_str::<IntegrationKeyConnectRequestDto>(
            r#"{"key":"one","label":null}"#
        )
        .is_err());
        assert!(serde_json::from_str::<IntegrationKeyConnectRequestDto>(
            r#"{"key":"one","key":"two"}"#
        )
        .is_err());
        Ok(())
    }
}

/// Native CLI extension: only the exported variable's name crosses this route.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct IntegrationEnvironmentConnectRequestDto {
    pub name: DtoString,
    #[serde(
        default = "grapt_schema::Optional::absent",
        skip_serializing_if = "grapt_schema::Optional::is_absent"
    )]
    pub label: grapt_schema::Optional<DtoString>,
}

/// Native CLI extension; desktop authentication DTOs retain their exact shape.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct IntegrationOAuthChallengeResponseDto {
    #[serde(rename = "userCode")]
    pub user_code: Option<DtoString>,
}

/// Native CLI metadata; tokens and account email are never part of this DTO.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationStatusResponseDto {
    pub connections: BoundedVec<AuthenticationConnectionStatusDto, 8>,
}
#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationConnectionStatusDto {
    pub id: DtoString,
    pub label: DtoString,
    pub provider: DtoString,
    pub backend: DtoString,
    pub last4: Option<DtoString>,
    pub status: DtoString,
}

/// Native connection selection uses the store's bounded local identity grammar.
pub fn connection_target(raw: &str) -> Result<DtoString, &'static str> {
    if raw.is_empty()
        || raw.len() > 64
        || raw.starts_with('.')
        || raw.contains("..")
        || !raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("invalid connection identity");
    }
    DtoString::try_new(raw.to_owned())
}

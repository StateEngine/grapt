use super::invalid;
use grapt_types::{
    AccountId, CredentialVersion, ModelId, ModelRouteId, ModelVariantId, PolicyVersion, ProviderId,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io};

const SCHEMA: &str = "secode.connections.v1";
pub const CONNECTION_CAP: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    GoogleGemini,
    CohereChat,
    VertexGemini,
    ChatgptCodex,
    OpenaiResponses,
    AnthropicMessages,
    VertexAnthropic,
    OpenaiCompatible,
    BedrockConverse,
    WatsonxChat,
}

impl Family {
    pub fn is_vertex(self) -> bool {
        matches!(self, Self::VertexGemini | Self::VertexAnthropic)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AuthMethod {
    AzureCli,
    OauthDevice,
    GoogleAdc,
    AwsChain,
    Key,
    Env { name: String },
    Custom,
}

/// File formats are explicit: a malformed vault never becomes a legacy key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CredentialFormat {
    AzureCli {
        binding: grapt_login::vault::DestinationBinding,
    },
    #[default]
    LegacyFile,
    GoogleAdc {
        binding: grapt_login::vault::DestinationBinding,
    },
    AwsChain {
        binding: grapt_login::vault::DestinationBinding,
    },
    Vault {
        binding: grapt_login::vault::DestinationBinding,
    },
    MessagesBearer {
        binding: grapt_login::vault::DestinationBinding,
        destination: grapt_login::vault::MessagesDestination,
    },
    CloudflareGateway {
        binding: grapt_login::vault::DestinationBinding,
        #[serde(with = "super::cloudflare")]
        scope: grapt_types::cloudflare::CloudflareGatewayScope,
    },
    /// OAuth state may reuse a bounded slot only behind its version fence.
    #[serde(rename = "vaulted-oauth")]
    VaultedOAuth {
        binding: grapt_login::vault::DestinationBinding,
    },
}
impl CredentialFormat {
    pub fn binding(&self) -> Option<&grapt_login::vault::DestinationBinding> {
        match self {
            Self::LegacyFile => None,
            Self::Vault { binding }
            | Self::GoogleAdc { binding }
            | Self::AwsChain { binding }
            | Self::AzureCli { binding }
            | Self::VaultedOAuth { binding }
            | Self::MessagesBearer { binding, .. }
            | Self::CloudflareGateway { binding, .. } => Some(binding),
        }
    }
    pub fn cloudflare(&self) -> Option<&grapt_types::cloudflare::CloudflareGatewayScope> {
        match self {
            Self::CloudflareGateway { scope, .. } => Some(scope),
            _ => None,
        }
    }
}

/// IDs retain their existing domain types. File names are local references;
/// neither this record nor its serialized form contains a credential payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: ProviderId,
    pub catalog_provider: ProviderId,
    pub family: Family,
    pub method: AuthMethod,
    pub label: String,
    pub endpoint: Option<String>,
    pub watsonx_project: Option<grapt_types::WatsonxProjectId>,
    pub account: AccountId,
    pub route: ModelRouteId,
    pub credential_version: CredentialVersion,
    pub policy_version: PolicyVersion,
    pub credential_file: String,
    pub credential_format: CredentialFormat,
    pub discovery_file: String,
    pub oauth_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultModel {
    pub provider: ProviderId,
    pub model: ModelId,
    pub variant: Option<ModelVariantId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connections {
    pub connections: Vec<Connection>,
    pub default_model: Option<DefaultModel>,
}

impl Connections {
    /// Encode a validated bounded generation for the shared sign-in journal.
    pub fn to_vec(&self) -> io::Result<Vec<u8>> {
        encode(self)
    }

    /// Decode a captured, bounded generation without reopening its source.
    pub fn from_slice(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > super::BYTE_CAP {
            return Err(invalid("connections document exceeds 64 KiB"));
        }
        decode(bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    connections: Vec<Record>,
    default_model: Option<Selection>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    provider_id: String,
    id: String,
    variant: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: String,
    catalog_provider: String,
    family: Family,
    method: AuthMethod,
    label: String,
    endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    watsonx_project: Option<String>,
    account: String,
    route: String,
    credential_version: String,
    policy_version: String,
    credential_file: String,
    #[serde(default)]
    credential_format: CredentialFormat,
    discovery_file: String,
    oauth_file: Option<String>,
}

pub(super) fn decode(bytes: &[u8]) -> io::Result<Connections> {
    let document: Document =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid connections document"))?;
    if document.schema != SCHEMA {
        return Err(invalid("unsupported connections schema"));
    }
    let connections = document
        .connections
        .into_iter()
        .map(|record| {
            Ok(Connection {
                id: ProviderId::new(record.id).ok_or_else(|| invalid("empty connection id"))?,
                catalog_provider: ProviderId::new(record.catalog_provider)
                    .ok_or_else(|| invalid("empty catalog provider"))?,
                family: record.family,
                method: record.method,
                label: record.label,
                endpoint: record.endpoint,
                watsonx_project: record
                    .watsonx_project
                    .map(|project| {
                        grapt_types::WatsonxProjectId::new(project)
                            .ok_or_else(|| invalid("invalid watsonx project"))
                    })
                    .transpose()?,
                account: AccountId::new(record.account)
                    .ok_or_else(|| invalid("empty account id"))?,
                route: ModelRouteId::new(record.route)
                    .ok_or_else(|| invalid("empty model route"))?,
                credential_version: CredentialVersion::new(record.credential_version)
                    .ok_or_else(|| invalid("empty credential version"))?,
                policy_version: PolicyVersion::new(record.policy_version)
                    .ok_or_else(|| invalid("empty policy version"))?,
                credential_file: record.credential_file,
                credential_format: record.credential_format,
                discovery_file: record.discovery_file,
                oauth_file: record.oauth_file,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let default_model = document
        .default_model
        .map(|selected| {
            Ok::<_, io::Error>(DefaultModel {
                provider: ProviderId::new(selected.provider_id)
                    .ok_or_else(|| invalid("empty default provider"))?,
                model: ModelId::new(selected.id).ok_or_else(|| invalid("empty default model"))?,
                variant: selected
                    .variant
                    .map(|variant| {
                        ModelVariantId::new(variant).ok_or_else(|| invalid("empty default variant"))
                    })
                    .transpose()?,
            })
        })
        .transpose()?;
    let result = Connections {
        connections,
        default_model,
    };
    validate(&result)?;
    Ok(result)
}

pub(super) fn encode(current: &Connections) -> io::Result<Vec<u8>> {
    validate(current)?;
    let document = Document {
        schema: SCHEMA.into(),
        connections: current
            .connections
            .iter()
            .map(|connection| Record {
                id: connection.id.as_str().into(),
                catalog_provider: connection.catalog_provider.as_str().into(),
                family: connection.family,
                method: connection.method.clone(),
                label: connection.label.clone(),
                endpoint: connection.endpoint.clone(),
                watsonx_project: connection
                    .watsonx_project
                    .as_ref()
                    .map(|project| project.as_str().to_owned()),
                account: connection.account.as_str().into(),
                route: connection.route.as_str().into(),
                credential_version: connection.credential_version.as_str().into(),
                policy_version: connection.policy_version.as_str().into(),
                credential_file: connection.credential_file.clone(),
                credential_format: connection.credential_format.clone(),
                discovery_file: connection.discovery_file.clone(),
                oauth_file: connection.oauth_file.clone(),
            })
            .collect(),
        default_model: current.default_model.as_ref().map(|selected| Selection {
            provider_id: selected.provider.as_str().into(),
            id: selected.model.as_str().into(),
            variant: selected
                .variant
                .as_ref()
                .map(|variant| variant.as_str().into()),
        }),
    };
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| invalid("connections cannot be encoded"))?;
    if bytes.len() > super::BYTE_CAP {
        return Err(invalid("connections document exceeds 64 KiB"));
    }
    Ok(bytes)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn validate(current: &Connections) -> io::Result<()> {
    if current.connections.len() > CONNECTION_CAP {
        return Err(invalid("too many connections"));
    }
    let mut ids = BTreeSet::new();
    let mut files = BTreeSet::new();
    for connection in &current.connections {
        if !plain(connection.id.as_str(), 64)
            || !plain(connection.catalog_provider.as_str(), 64)
            || !ids.insert(&connection.id)
        {
            return Err(invalid("invalid or duplicate connection id"));
        }
        for text in [
            &connection.label,
            connection.account.as_str(),
            connection.route.as_str(),
            connection.credential_version.as_str(),
            connection.policy_version.as_str(),
        ] {
            if !textual(text, 256) {
                return Err(invalid("invalid connection metadata"));
            }
        }
        if let Some(endpoint) = &connection.endpoint {
            // The provider adapter validates origins before credential use.
            // Never allow userinfo or control bytes into this non-secret file.
            if !textual(endpoint, 2048)
                || endpoint.contains('@')
                || endpoint.contains(['?', '#'])
                || !(endpoint.starts_with("https://") || endpoint.starts_with("http://"))
            {
                return Err(invalid("invalid connection endpoint"));
            }
        }
        for (name, suffix) in [
            (&connection.credential_file, ".credential"),
            (&connection.discovery_file, ".json"),
        ] {
            if !local_file(name, suffix) || !files.insert(name) {
                return Err(invalid("invalid or shared connection file"));
            }
        }
        if matches!(
            connection.credential_format,
            CredentialFormat::GoogleAdc { .. }
        ) != (connection.method == AuthMethod::GoogleAdc)
            || (connection.method == AuthMethod::GoogleAdc
                && (!connection.family.is_vertex()
                    || connection.endpoint.is_none()
                    || connection.oauth_file.is_some()))
        {
            return Err(invalid(
                "Google application-default credentials require a bound Vertex connection",
            ));
        }
        if matches!(
            connection.credential_format,
            CredentialFormat::AwsChain { .. }
        ) != (connection.method == AuthMethod::AwsChain)
            || (connection.method == AuthMethod::AwsChain
                && (connection.family != Family::BedrockConverse
                    || connection.endpoint.is_none()
                    || connection.oauth_file.is_some()))
        {
            return Err(invalid(
                "AWS chain credentials require a bound Bedrock connection",
            ));
        }
        if matches!(
            connection.credential_format,
            CredentialFormat::AzureCli { .. }
        ) != (connection.method == AuthMethod::AzureCli)
            || (connection.method == AuthMethod::AzureCli
                && (!matches!(
                    connection.catalog_provider.as_str(),
                    "azure" | "azure-cognitive-services"
                ) || connection.family != Family::OpenaiResponses
                    || connection.endpoint.is_none()
                    || connection.oauth_file.is_some()))
        {
            return Err(invalid(
                "Azure CLI credentials require a bound Azure connection",
            ));
        }
        if matches!(
            connection.discovery_file.as_str(),
            "connections.json" | "grapt.json" | "secode.json" // grapt-rename: keep
        ) || connection.discovery_file.ends_with(".oauth.json")
        {
            return Err(invalid("discovery file names reserved profile storage"));
        }
        if let Some(name) = &connection.oauth_file {
            if !local_file(name, ".oauth.json") || !files.insert(name) {
                return Err(invalid("invalid or shared OAuth file"));
            }
        }
        if matches!(connection.method, AuthMethod::Env { .. })
            && connection.credential_format != CredentialFormat::LegacyFile
        {
            return Err(invalid(
                "environment connections cannot name a vault payload",
            ));
        }
        if let CredentialFormat::MessagesBearer {
            binding,
            destination,
        } = &connection.credential_format
        {
            if connection.family != Family::AnthropicMessages
                || connection.method != AuthMethod::Key
                || connection.oauth_file.is_some()
                || connection.endpoint.as_deref() != Some(destination.as_str())
                || *binding
                    != grapt_login::vault::DestinationBinding::key(
                        grapt_login::vault::ApiDestination::parse(destination.as_str())
                            .map_err(|_| invalid("invalid Messages destination"))?,
                    )
            {
                return Err(invalid(
                    "bearer Messages credentials do not match the connection destination",
                ));
            }
        }
        if connection.catalog_provider.as_str() == "cloudflare-ai-gateway"
            && connection.credential_format.cloudflare().is_none()
        {
            return Err(invalid("Cloudflare requires typed gateway credentials"));
        }
        if let CredentialFormat::CloudflareGateway { binding, scope } =
            &connection.credential_format
        {
            let family = match scope.wire() {
                grapt_types::cloudflare::CloudflareWire::Responses => Family::OpenaiResponses,
                grapt_types::cloudflare::CloudflareWire::Messages => Family::AnthropicMessages,
                grapt_types::cloudflare::CloudflareWire::Chat => Family::OpenaiCompatible,
            };
            let endpoint = connection
                .endpoint
                .as_deref()
                .ok_or_else(|| invalid("Cloudflare endpoint is absent"))?;
            if connection.catalog_provider.as_str() != "cloudflare-ai-gateway"
                || connection.method != AuthMethod::Key
                || connection.family != family
                || connection.oauth_file.is_some()
                || grapt_login::key::cloudflare::check_base(scope, endpoint).is_err()
                || binding.check_api(endpoint).is_err()
                || *binding
                    != grapt_login::vault::DestinationBinding::key(
                        grapt_login::vault::ApiDestination::parse(endpoint)
                            .map_err(|_| invalid("invalid Cloudflare API binding"))?,
                    )
            {
                return Err(invalid(
                    "Cloudflare credential scope does not match its connection",
                ));
            }
        }
        if matches!(
            connection.credential_format,
            CredentialFormat::VaultedOAuth { .. }
        ) && connection.method != AuthMethod::OauthDevice
        {
            return Err(invalid("generation-fenced OAuth requires OAuth state"));
        }
        if (connection.family == Family::WatsonxChat) != connection.watsonx_project.is_some() {
            return Err(invalid("watsonx requires its own project coordinate"));
        }
        if matches!(
            connection.family,
            Family::VertexGemini | Family::VertexAnthropic | Family::WatsonxChat
        ) && connection.method != AuthMethod::GoogleAdc
            && (connection.method != AuthMethod::Key
                || !matches!(connection.credential_format, CredentialFormat::Vault { .. })
                || connection.endpoint.is_none())
        {
            return Err(invalid(
                "cloud token exchange requires an explicit destination-bound key vault",
            ));
        }
        match &connection.method {
            AuthMethod::OauthDevice
                if connection.family != Family::ChatgptCodex || connection.oauth_file.is_none() =>
            {
                return Err(invalid("OAuth requires ChatGPT state"))
            }
            AuthMethod::Env { name }
                if !environment_name(name) || connection.oauth_file.is_some() =>
            {
                return Err(invalid("invalid environment reference"))
            }
            AuthMethod::Key | AuthMethod::Custom if connection.oauth_file.is_some() => {
                return Err(invalid("key connection cannot carry OAuth state"))
            }
            _ => {}
        }
    }
    if let Some(selected) = &current.default_model {
        if !ids.contains(&selected.provider)
            || !textual(selected.model.as_str(), 256)
            || selected
                .variant
                .as_ref()
                .is_some_and(|variant| !textual(variant.as_str(), 256))
        {
            return Err(invalid(
                "default model names no connection or has an invalid id",
            ));
        }
    }
    Ok(())
}

fn textual(text: &str, cap: usize) -> bool {
    !text.is_empty() && text.len() <= cap && !text.chars().any(char::is_control)
}
fn plain(text: &str, cap: usize) -> bool {
    textual(text, cap)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !text.starts_with('.')
        && !text.contains("..")
}
fn local_file(text: &str, suffix: &str) -> bool {
    plain(text, 64) && text.ends_with(suffix)
}
pub(super) fn environment_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

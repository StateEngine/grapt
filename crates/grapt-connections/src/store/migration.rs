//! A one-document migration. Existing credential and OAuth files stay in place.
use super::{invalid, AuthMethod, Connection, Connections, DefaultModel, Family};
use grapt_config::{
    ConfigBounds, ConfigValue, MaxAncestorDepth, MaxDocumentBytes, MaxDocumentNodes, MaxDocuments,
    MaxValueDepth,
};
use grapt_types::{
    AccountId, CredentialVersion, ModelId, ModelRouteId, ModelVariantId, PolicyVersion, ProviderId,
};
use std::io;

pub(super) fn decode(bytes: &[u8]) -> io::Result<Connections> {
    let bounds = ConfigBounds::new(
        MaxDocumentBytes::try_new(super::BYTE_CAP)
            .map_err(|_| invalid("invalid configuration bound"))?,
        MaxAncestorDepth::try_new(1).map_err(|_| invalid("invalid configuration bound"))?,
        MaxDocuments::try_new(1).map_err(|_| invalid("invalid configuration bound"))?,
        MaxValueDepth::try_new(16).map_err(|_| invalid("invalid configuration bound"))?,
        MaxDocumentNodes::try_new(4096).map_err(|_| invalid("invalid configuration bound"))?,
    );
    let document = grapt_config::decode(bytes, bounds)
        .map_err(|_| invalid("legacy configuration is invalid JSONC"))?;
    let provider = document
        .value()
        .get("provider")
        .ok_or_else(|| invalid("legacy provider is absent"))?
        .value();
    let primary = connection(provider, true)?;
    let default_model = Some(DefaultModel {
        provider: primary.id.clone(),
        model: ModelId::new(required(provider, "model")?)
            .ok_or_else(|| invalid("legacy model is empty"))?,
        variant: optional(provider, "selected_variant")?
            .map(|variant| {
                ModelVariantId::new(variant).ok_or_else(|| invalid("legacy variant is empty"))
            })
            .transpose()?,
    });
    let mut connections = vec![primary];
    if let Some(accounts) = provider.get("accounts") {
        let ConfigValue::Array(accounts) = accounts.value() else {
            return Err(invalid("legacy accounts must be an array"));
        };
        if accounts.len() >= super::model::CONNECTION_CAP {
            return Err(invalid("too many legacy accounts"));
        }
        for account in accounts {
            connections.push(connection(account.value(), false)?);
        }
    }
    let result = Connections {
        connections,
        default_model,
    };
    super::model::validate(&result)?;
    Ok(result)
}

fn connection(provider: &ConfigValue, primary: bool) -> io::Result<Connection> {
    let family = match optional(provider, "protocol_family")? {
        None if primary => Family::ChatgptCodex,
        Some("chatgpt-codex") => Family::ChatgptCodex,
        Some("openai-responses") => Family::OpenaiResponses,
        Some("anthropic-messages") => Family::AnthropicMessages,
        Some("vertex-anthropic") => Family::VertexAnthropic,
        Some("openai-compatible") => Family::OpenaiCompatible,
        Some("google-gemini") => Family::GoogleGemini,
        Some("cohere-chat") => Family::CohereChat,
        Some("vertex-gemini") => Family::VertexGemini,
        _ => return Err(invalid("legacy protocol family is absent or unsupported")),
    };
    let id = required(provider, "id")?;
    let credential = optional(provider, "credential")?;
    let method = if let Some(name) = credential.and_then(|value| value.strip_prefix("env:")) {
        if !super::model::environment_name(name) {
            return Err(invalid("invalid legacy environment reference"));
        }
        AuthMethod::Env { name: name.into() }
    } else if primary
        && family == Family::ChatgptCodex
        && required(provider, "credential_version")?
            == grapt_login::chatgpt::SIGN_IN_CREDENTIAL_VERSION
    {
        AuthMethod::OauthDevice
    } else {
        AuthMethod::Key
    };
    // Primary file names are historical constants. Secondary records already
    // name their own files; do not rename them when preserving provider IDs.
    let credential_file = if primary {
        if credential.is_some() && !matches!(method, AuthMethod::Env { .. }) {
            return Err(invalid(
                "legacy primary credential is not an environment reference",
            ));
        }
        "provider.credential".to_owned()
    } else {
        required(provider, "credential")?.to_owned()
    };
    let oauth_file = if method == AuthMethod::OauthDevice {
        if !primary {
            return Err(invalid(
                "legacy secondary ChatGPT state is not named; sign in again",
            ));
        }
        Some("chatgpt.oauth.json".into())
    } else {
        None
    };
    Ok(Connection {
        id: ProviderId::new(id).ok_or_else(|| invalid("legacy provider id is empty"))?,
        catalog_provider: ProviderId::new(id)
            .ok_or_else(|| invalid("legacy catalog id is empty"))?,
        family,
        method,
        label: if family == Family::ChatgptCodex {
            "ChatGPT".into()
        } else {
            id.into()
        },
        endpoint: optional(provider, "endpoint")?.map(str::to_owned),
        watsonx_project: None,
        account: AccountId::new(required(provider, "account")?)
            .ok_or_else(|| invalid("legacy account is empty"))?,
        route: ModelRouteId::new(required(provider, "route")?)
            .ok_or_else(|| invalid("legacy route is empty"))?,
        credential_version: CredentialVersion::new(required(provider, "credential_version")?)
            .ok_or_else(|| invalid("legacy credential version is empty"))?,
        policy_version: PolicyVersion::new(required(provider, "policy_version")?)
            .ok_or_else(|| invalid("legacy policy version is empty"))?,
        credential_file,
        credential_format: super::CredentialFormat::LegacyFile,
        discovery_file: if primary {
            "account-models.json".into()
        } else {
            required(provider, "discovery")?.into()
        },
        oauth_file,
    })
}

fn optional<'a>(value: &'a ConfigValue, name: &str) -> io::Result<Option<&'a str>> {
    value
        .get(name)
        .map(|value| {
            value
                .value()
                .as_str()
                .filter(|text| !text.is_empty())
                .ok_or_else(|| invalid("legacy connection field must be a nonempty string"))
        })
        .transpose()
}
fn required<'a>(value: &'a ConfigValue, name: &str) -> io::Result<&'a str> {
    optional(value, name)?.ok_or_else(|| invalid("legacy connection field is absent"))
}

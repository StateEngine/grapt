//! Profile operators use one service-owned connection store and vault.
//!
//! CLI UX.
//! Copyright 2025 OpenAI. Modified by Grapt: shared API custody, bounded
//! input and status observation, environment references, JSON progress.
//! License and NOTICE.
use crate::command::login::{ConnectionAction, ConnectionCommand, KeyInput};
use crate::{ApiClient, CliLimits, ExitClass};
use grapt_api::u3a::*;
use grapt_connections::store::{Connection, Store};
use grapt_schema::Optional;
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use tokio::io::AsyncReadExt;

use super::remote;

type Failure = (ExitClass, String);
const DISCONNECTED: &str = "Disconnected locally. This does not revoke access at the provider.";

pub(super) async fn execute(command: ConnectionCommand, limits: CliLimits) -> u8 {
    let structured = command.json;
    match run(command, limits).await {
        Ok(true) => ExitClass::Success.code(),
        Ok(false) => 1,
        Err((class, reason)) => {
            let output = if structured {
                json!({"status":"failed","exit":class,"reason":reason}).to_string()
            } else {
                format!("grapt: {reason}")
            };
            if super::emit(output, !structured, limits).await.is_err() {
                return ExitClass::Output.code();
            }
            class.code()
        }
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn run(command: ConnectionCommand, limits: CliLimits) -> Result<bool, Failure> {
    if matches!(command.action, ConnectionAction::List) {
        let providers = grapt_connections::catalog::integrations().map_err(|_| protocol())?;
        let mut rows = vec![
            json!({"id":"chatgpt","name":"ChatGPT","supported":true,"methods":["oauth-browser","oauth-device"]}),
        ];
        for provider in providers {
            let mut methods = Vec::new();
            if let Some(key) = provider.key {
                methods.push("key");
                if matches!(provider.id, "azure" | "azure-cognitive-services") {
                    methods.push("azure-cli");
                }
                if matches!(provider.id, "google-vertex" | "google-vertex-anthropic") {
                    methods.push("google-adc");
                }
                if provider.id == "amazon-bedrock" {
                    methods.push("aws-chain");
                }
                if !key.environment_names().map_err(|_| protocol())?.is_empty() {
                    methods.push("env");
                }
            }
            rows.push(json!({"id":provider.id,"name":provider.name,
                "supported":provider.key.is_some(),"methods":methods}));
        }
        let text = rows
            .iter()
            .filter_map(|row| {
                row["id"].as_str().map(|id| {
                    format!(
                        "{id}{}",
                        if row["supported"] == true {
                            ""
                        } else {
                            " (not yet supported)"
                        }
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        event(command.json, json!({"providers":rows}), &text, limits).await?;
        return Ok(true);
    }
    let root = command
        .profile
        .clone()
        .or_else(|| crate::command::terminal::user_profile(|name| std::env::var_os(name)))
        .ok_or((
            ExitClass::Usage,
            "name the profile with --profile <profile>".into(),
        ))?;
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()
            .map_err(|_| (ExitClass::Usage, "current directory is unreadable".into()))?
            .join(root)
    };
    if let ConnectionAction::Login { provider, key, .. } = &command.action {
        if provider != "chatgpt" {
            let provider = grapt_connections::catalog::KeyProvider::from_id(provider)
                .map_err(|_| protocol())?
                .ok_or((
                    ExitClass::Usage,
                    "this provider has no implemented API key adapter".into(),
                ))?;
            if let KeyInput::Environment(name) = key {
                if !provider.allows_environment(name).map_err(|_| protocol())? {
                    return Err((
                        ExitClass::Usage,
                        "that variable is not a supported credential source for this provider"
                            .into(),
                    ));
                }
            }
        }
    }
    // Retain the pre-service legacy sign-in command. It has no service metadata
    // or database authority and must not be silently converted into a service.
    if let ConnectionAction::Login {
        provider,
        key: KeyInput::Terminal,
        no_browser: false,
        label: None,
        connection: None,
        setup: None,
        vertex: None,
        bedrock: None,
        azure: None,
        cloudflare: None,
    } = &command.action
    {
        if provider == "chatgpt"
            && !structured_service_profile(&root)?
            && !command.json
            && root.is_dir()
        {
            if super::login::chatgpt(Some(root)).await == ExitClass::Success.code() {
                return Ok(true);
            }
            return Err((
                ExitClass::Remote,
                "legacy ChatGPT sign-in did not finish".into(),
            ));
        }
    }
    let create = matches!(&command.action, ConnectionAction::Login { .. });
    let Some(client) = super::connection_client::open(&root, create, limits).await? else {
        if let ConnectionAction::Logout { provider } = &command.action {
            return Err(not_signed_in(provider, &[]));
        }
        event(
            command.json,
            json!({"status":"signed_out","connections":[]}),
            "No providers connected.",
            limits,
        )
        .await?;
        return Ok(false);
    };
    let result = operate(&client, &root, command, limits).await;
    let closed = super::connection_client::close(&client).await;
    let usable = result?;
    closed?;
    Ok(usable)
}

fn structured_service_profile(root: &Path) -> Result<bool, Failure> {
    match std::fs::symlink_metadata(root.join("service.conf")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err((
            ExitClass::Discovery,
            format!("service configuration is unreadable: {:?}", error.kind()),
        )),
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn operate(
    client: &ApiClient,
    root: &Path,
    command: ConnectionCommand,
    limits: CliLimits,
) -> Result<bool, Failure> {
    match command.action {
        ConnectionAction::List => Err(protocol()),
        ConnectionAction::Status => status(client, command.json, limits).await,
        ConnectionAction::Logout { provider } => {
            let records = Store::read_snapshot(root)
                .map_err(|error| {
                    (
                        ExitClass::Discovery,
                        format!("connections are unreadable: {:?}", error.kind()),
                    )
                })?
                .ok_or_else(|| not_signed_in(&provider, &[]))?;
            let Some(record) = records
                .connections
                .iter()
                .find(|record| record.id.as_str() == provider)
            else {
                return Err(not_signed_in(&provider, &records.connections));
            };
            let id = CredentialIdDto::try_new(format!("cred_{}", record.id.as_str()))
                .map_err(|_| protocol())?;
            client.credential_remove(&id).await.map_err(remote)?;
            event(
                command.json,
                json!({"status":"signed_out","provider":provider,"revoked":false}),
                DISCONNECTED,
                limits,
            )
            .await?;
            Ok(true)
        }
        ConnectionAction::Login {
            provider,
            key,
            no_browser,
            label,
            connection,
            setup,
            vertex,
            bedrock,
            azure,
            cloudflare,
        } => {
            if provider == "chatgpt" {
                return oauth(client, no_browser, label, connection, command.json, limits)
                    .await
                    .map(|()| true);
            }
            let id = IntegrationIdDto::try_new(provider.clone()).map_err(|_| protocol())?;
            let provider = grapt_connections::catalog::KeyProvider::from_id(&provider)
                .map_err(|_| protocol())?
                .ok_or((
                    ExitClass::Usage,
                    "this provider has no implemented API key adapter".into(),
                ))?;
            let label = match label {
                Some(label) => {
                    Optional::present(DtoString::try_new(label).map_err(|_| protocol())?)
                }
                None => Optional::absent(),
            };
            // An explicit key source wins. With none, the provider's own
            // standard variable is used when it is set, before any prompt,
            // but only by a service this command started: a service already
            // running did not inherit this shell's environment, so it asks.
            // An environment connect carries no endpoint or model setup, so
            // a login that supplies one enters its key instead.
            let supplied = setup.is_some()
                || vertex.is_some()
                || bedrock.is_some()
                || azure.is_some()
                || cloudflare.is_some();
            let key = match key {
                KeyInput::Terminal if client.owns_service() && !supplied => {
                    match ambient_variable(provider)? {
                        Some(name) => {
                            super::emit(
                            format!("Using {name} from the environment; pass --key-stdin to enter a different key."),
                            true,
                            limits,
                        )
                        .await
                        .map_err(|error| (ExitClass::Output, error))?;
                            KeyInput::Environment(name.to_owned())
                        }
                        None => KeyInput::Terminal,
                    }
                }
                other => other,
            };
            let azure_cli = key == KeyInput::AzureCli;
            let aws_profile = match &key {
                KeyInput::AwsChain { profile } => Some(profile.clone()),
                _ => None,
            };
            match key {
                KeyInput::Environment(name) => {
                    if !provider.allows_environment(&name).map_err(|_| protocol())? {
                        return Err((
                            ExitClass::Usage,
                            "that variable is not a supported credential source for this provider"
                                .into(),
                        ));
                    }
                    client
                        .integration_environment(
                            &id,
                            connection.as_deref(),
                            &IntegrationEnvironmentConnectRequestDto {
                                name: DtoString::try_new(name).map_err(|_| protocol())?,
                                label,
                            },
                        )
                        .await
                        .map_err(remote)?;
                }
                input => {
                    let permits_empty = provider.permits_empty_local_key()
                        && setup
                            .as_ref()
                            .is_some_and(|setup| setup.base_url.starts_with("http://"));
                    let key = match input {
                        KeyInput::AwsChain { .. } | KeyInput::AzureCli | KeyInput::GoogleAdc => {
                            String::new()
                        }
                        KeyInput::Stdin => stdin_key(permits_empty).await?,
                        // A local server on this machine usually takes no key;
                        // --key-stdin still supplies one for a server that does.
                        KeyInput::Terminal
                            if permits_empty
                                && grapt_connections::catalog::endpoint_provider(provider.id())
                                    .is_some_and(|row| row.local.is_some()) =>
                        {
                            String::new()
                        }
                        KeyInput::Terminal => crate::client::terminal::read_api_key(permits_empty)
                            .map_err(|reason| (ExitClass::Usage, reason))?,
                        KeyInput::Environment(_) => return Err(protocol()),
                    };
                    client
                        .integration_key(
                            &id,
                            connection.as_deref(),
                            &IntegrationKeyConnectRequestDto {
                                vertex: match vertex {
                                    Some(native) => Optional::present(VertexProviderRequestDto {
                                        auth: Optional::present(
                                            VertexAuthRequestDto::ApplicationDefault,
                                        ),
                                        project: DtoString::try_new(native.project)
                                            .map_err(|_| protocol())?,
                                        location: DtoString::try_new(native.location)
                                            .map_err(|_| protocol())?,
                                        model: DtoString::try_new(native.model)
                                            .map_err(|_| protocol())?,
                                    }),
                                    None => Optional::absent(),
                                },
                                watsonx: grapt_schema::Optional::absent(),
                                key: DtoString::try_new(key).map_err(|_| {
                                    (ExitClass::Usage, "API key input is invalid".into())
                                })?,
                                label,
                                custom: match setup {
                                    Some(setup) => Optional::present(CustomProviderRequestDto {
                                        endpoint: DtoString::try_new(setup.base_url)
                                            .map_err(|_| protocol())?,
                                        model: setup
                                            .model
                                            .map(DtoString::try_new)
                                            .transpose()
                                            .map_err(|_| protocol())?
                                            .map_or_else(Optional::absent, Optional::present),
                                        context_window: setup
                                            .context_window
                                            .map_or_else(Optional::absent, Optional::present),
                                    }),
                                    None => Optional::absent(),
                                },
                                bedrock: match bedrock {
                                    Some(native) => Optional::present(BedrockProviderRequestDto {
                                        auth: match aws_profile {
                                            Some(profile) => {
                                                Optional::present(BedrockAuthRequestDto::AwsChain {
                                                    profile: profile
                                                        .map(DtoString::try_new)
                                                        .transpose()
                                                        .map_err(|_| protocol())?
                                                        .map_or_else(
                                                            Optional::absent,
                                                            Optional::present,
                                                        ),
                                                })
                                            }
                                            None => Optional::absent(),
                                        },
                                        region: DtoString::try_new(native.region)
                                            .map_err(|_| protocol())?,
                                        model: DtoString::try_new(native.model)
                                            .map_err(|_| protocol())?,
                                        endpoint: native
                                            .endpoint
                                            .map(DtoString::try_new)
                                            .transpose()
                                            .map_err(|_| protocol())?
                                            .map_or_else(Optional::absent, Optional::present),
                                    }),
                                    None => Optional::absent(),
                                },
                                databricks: Optional::absent(),
                                azure: match azure {
                                    Some(native) => Optional::present(AzureProviderRequestDto {
                                        auth: Optional::present(if azure_cli {
                                            AzureAuthRequestDto::AzureCli
                                        } else {
                                            AzureAuthRequestDto::ApiKey
                                        }),
                                        endpoint: DtoString::try_new(native.endpoint)
                                            .map_err(|_| protocol())?,
                                        deployment: DtoString::try_new(native.deployment)
                                            .map_err(|_| protocol())?,
                                        model: DtoString::try_new(native.model)
                                            .map_err(|_| protocol())?,
                                    }),
                                    None => Optional::absent(),
                                },
                                cloudflare: match cloudflare {
                                    Some(scope) => {
                                        Optional::present(IntegrationCloudflareGatewayConnectDto {
                                            account: DtoString::try_new(scope.account().as_str())
                                                .map_err(|_| protocol())?,
                                            gateway: DtoString::try_new(scope.gateway().as_str())
                                                .map_err(|_| protocol())?,
                                            provider: DtoString::try_new(scope.provider().as_str())
                                                .map_err(|_| protocol())?,
                                        })
                                    }
                                    None => Optional::absent(),
                                },
                            },
                        )
                        .await
                        .map_err(remote)?;
                }
            }
            event(
                command.json,
                json!({"status":"signed_in","provider":provider.id()}),
                "Connected.",
                limits,
            )
            .await?;
            Ok(true)
        }
    }
}

async fn stdin_key(permits_empty: bool) -> Result<String, Failure> {
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take(515)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| (ExitClass::Usage, "API key stdin is unreadable".into()))?;
    if bytes.len() > 514 {
        return Err((
            ExitClass::Usage,
            "API key stdin exceeds its byte bound".into(),
        ));
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    if (!permits_empty && bytes.is_empty())
        || bytes.len() > 512
        || bytes.iter().any(|byte| byte.is_ascii_control())
    {
        return Err((
            ExitClass::Usage,
            "API key stdin must contain one nonempty line of at most 512 bytes".into(),
        ));
    }
    String::from_utf8(bytes).map_err(|_| (ExitClass::Usage, "API key stdin must be UTF-8".into()))
}

async fn status(client: &ApiClient, structured: bool, limits: CliLimits) -> Result<bool, Failure> {
    let response = serde_json::to_value(client.authentication_status().await.map_err(remote)?)
        .map_err(|_| protocol())?;
    let rows = response["connections"].as_array().ok_or_else(protocol)?;
    let usable = rows
        .iter()
        .any(|row| matches!(row["status"].as_str(), Some("ok" | "renewal-due")));
    let text = if rows.is_empty() {
        "No providers connected.".into()
    } else {
        rows.iter()
            .map(|row| {
                format!(
                    "{}: {} [{}]{}",
                    row["id"].as_str().unwrap_or_default(),
                    row["status"].as_str().unwrap_or_default(),
                    row["backend"].as_str().unwrap_or_default(),
                    row["last4"]
                        .as_str()
                        .map_or_else(String::new, |last4| format!(" …{last4}"))
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    event(
        structured,
        json!({"status":if usable {"signed_in"} else {"signed_out"},"connections":rows}),
        &text,
        limits,
    )
    .await?;
    Ok(usable)
}

async fn event(
    structured: bool,
    value: Value,
    text: &str,
    limits: CliLimits,
) -> Result<(), Failure> {
    super::emit(
        if structured {
            value.to_string()
        } else {
            text.to_owned()
        },
        false,
        limits,
    )
    .await
    .map_err(|reason| (ExitClass::Output, reason))
}
fn not_signed_in(provider: &str, connections: &[Connection]) -> Failure {
    let signed_in = if connections.is_empty() {
        "none".to_owned()
    } else {
        connections
            .iter()
            .map(|connection| connection.id.as_str().escape_debug().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    (
        ExitClass::NotFound,
        format!(
            "connection '{}' is not signed in. Signed-in connections: {signed_in}. Use grapt auth status to inspect connection IDs, or grapt login to sign in.",
            provider.escape_debug()
        ),
    )
}
/// The first of a provider's standard key variables that is set and non-empty.
fn ambient_variable(
    provider: grapt_connections::catalog::KeyProvider,
) -> Result<Option<&'static str>, Failure> {
    Ok(provider
        .environment_names()
        .map_err(|_| protocol())?
        .into_iter()
        .find(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty())))
}

/// The login commands that would connect a key already in this environment,
/// for a refusal that has no connection to use yet.
pub(super) fn ambient_logins() -> Vec<String> {
    let Ok(integrations) = grapt_connections::catalog::integrations() else {
        return Vec::new();
    };
    integrations
        .iter()
        .filter_map(|integration| {
            let name = ambient_variable(integration.key?).ok()??;
            Some(format!("grapt login {} (uses {name})", integration.id))
        })
        .take(3)
        .collect()
}

fn protocol() -> Failure {
    (
        ExitClass::Protocol,
        "connection service returned an incompatible response".into(),
    )
}

async fn oauth(
    client: &ApiClient,
    no_browser: bool,
    label: Option<String>,
    connection: Option<String>,
    structured: bool,
    limits: CliLimits,
) -> Result<(), Failure> {
    let id = IntegrationIdDto::try_new("openai".to_owned()).map_err(|_| protocol())?;
    let method = if no_browser {
        "chatgpt-device"
    } else {
        "chatgpt-browser"
    };
    let mut value = json!({"methodID":method,"inputs":{}});
    if let Some(label) = label {
        value["label"] = label.into();
    }
    let body: IntegrationOAuthConnectRequestDto =
        serde_json::from_value(value).map_err(|_| protocol())?;
    // Constructors install the handler synchronously. Retain any interrupt
    // received while starting the attempt, before its id is available to cancel.
    let mut cancelled = super::signals::TerminationSignal::register().map_err(|_| {
        (
            ExitClass::Protocol,
            "sign-in cancellation could not be registered".into(),
        )
    })?;
    let response = client
        .integration_oauth(&id, connection.as_deref(), &body)
        .await
        .map_err(remote)?;
    let attempt = response.data().get().attempt_id().get().clone();
    let result = tokio::select! {
        biased;
        _ = cancelled.recv() => Err((ExitClass::Cancelled, "sign-in cancelled; check grapt auth status for its current state".into())),
        result = observe_oauth(client, &id, &attempt, response, no_browser, structured, limits) => result,
    };
    if result.is_err() {
        let _cancel = client.integration_oauth_cancel(&id, &attempt).await;
    }
    result
}

async fn observe_oauth(
    client: &ApiClient,
    provider: &IntegrationIdDto,
    attempt: &IntegrationAttemptIdDto,
    response: IntegrationOAuthConnectResponseDto,
    no_browser: bool,
    structured: bool,
    limits: CliLimits,
) -> Result<(), Failure> {
    let response = serde_json::to_value(response).map_err(|_| protocol())?;
    let data = &response["data"];
    let url = data["url"].as_str().ok_or_else(protocol)?;
    login_url(url)?;
    let user_code = client
        .integration_oauth_challenge(provider, attempt)
        .await
        .map_err(remote)?
        .user_code;
    event(structured, json!({"status":"pending","attemptID":attempt,"url":url,"userCode":user_code,"expiresAt":data["time"]["expires"]}),
        &format!("Open {url}\n{}\nWaiting for sign-in (Ctrl-C cancels)...", data["instructions"].as_str().ok_or_else(protocol)?), limits).await?;
    if !no_browser {
        super::mcp::browser(&grapt_types::Secret::new(url.to_owned())).await;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
    loop {
        let status =
            tokio::time::timeout_at(deadline, client.integration_oauth_status(provider, attempt))
                .await
                .map_err(|_| (ExitClass::Remote, "sign-in observation expired".into()))?
                .map_err(remote)?;
        let value = serde_json::to_value(status).map_err(|_| protocol())?;
        match value["data"]["status"].as_str() {
            Some("complete") => {
                return event(
                    structured,
                    json!({"status":"signed_in","provider":"chatgpt","attemptID":attempt}),
                    "Signed in to ChatGPT.",
                    limits,
                )
                .await
            }
            Some("failed" | "expired") => {
                return Err((
                    ExitClass::Remote,
                    value["data"]["message"]
                        .as_str()
                        .unwrap_or("sign-in expired; try again")
                        .into(),
                ))
            }
            Some("pending") => (),
            _ => return Err(protocol()),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn login_url(url: &str) -> Result<(), Failure> {
    if super::mcp::authorization_url(url).is_ok() {
        return Ok(());
    }
    #[cfg(feature = "loopback-testkit")]
    if url.len() <= 32768 && !url.chars().any(char::is_control) {
        if let Ok(parsed) = reqwest::Url::parse(url) {
            if parsed.scheme() == "http"
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.fragment().is_none()
                && parsed.host_str().is_some_and(|host| {
                    host.parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
                })
            {
                return Ok(());
            }
        }
    }
    Err((ExitClass::Protocol, "sign-in URL is invalid".into()))
}

//! Explicit profile connection operators. Secret values are never argv options.
use super::{CliCommand, CommandError};
use std::path::PathBuf;

const USAGE: &str = "usage: grapt login [provider] [--profile <profile>] [--key-stdin | --api-key-env <name> | --azure-cli | --google-adc | --aws | --aws-profile <name>] [--region <region>] [--deployment <name>] [--google-project <id>] [--google-location <location>] [--id <connection>] [--label <label>] [--base-url <url>] [--model <id> [--context-window <tokens>]] [--cf-account <account>] [--cf-gateway <gateway>] [--cf-provider <provider>] [--no-browser] [--json]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    Terminal,
    AzureCli,
    Stdin,
    Environment(String),
    GoogleAdc,
    AwsChain { profile: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointCoordinates {
    pub base_url: String,
    /// Absent: every model the endpoint lists.
    pub model: Option<String>,
    /// Present only with a model: the user's own window for it.
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureCoordinates {
    pub endpoint: String,
    pub deployment: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexCoordinates {
    pub project: String,
    pub location: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedrockCoordinates {
    pub region: String,
    pub model: String,
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionAction {
    List,
    Login {
        provider: String,
        key: KeyInput,
        no_browser: bool,
        label: Option<String>,
        connection: Option<String>,
        setup: Option<Box<EndpointCoordinates>>,
        vertex: Option<Box<VertexCoordinates>>,
        bedrock: Option<Box<BedrockCoordinates>>,
        azure: Option<Box<AzureCoordinates>>,
        cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
    },
    Logout {
        provider: String,
    },
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionCommand {
    pub profile: Option<PathBuf>,
    pub json: bool,
    pub action: ConnectionAction,
}

pub(super) fn parse(arguments: &[String]) -> Result<CliCommand, CommandError> {
    operator("login", arguments)
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn operator(verb: &str, arguments: &[String]) -> Result<CliCommand, CommandError> {
    if arguments.len() > 20 {
        return Err(CommandError::Login(
            "too many connection arguments (maximum 20)",
        ));
    }
    let listing = verb == "login"
        && arguments
            .first()
            .is_none_or(|value| value.starts_with("--"));
    let (provider, rest) = if listing {
        (None, arguments)
    } else if verb == "auth" {
        if arguments.first().map(String::as_str) != Some("status") {
            return Err(CommandError::Login(
                "usage: grapt auth status [--profile <profile>] [--json]",
            ));
        }
        (None, &arguments[1..])
    } else {
        let provider = arguments.first().ok_or(CommandError::Login(USAGE))?;
        if grapt_api::u3a::connection_target(provider).is_err() {
            return Err(CommandError::Login("provider identity is invalid"));
        }
        (Some(provider.clone()), &arguments[1..])
    };
    let mut profile = None;
    let mut json = false;
    let mut no_browser = false;
    let mut key = KeyInput::Terminal;
    let mut label = None;
    let mut connection = None;
    let mut base_url = None;
    let mut model = None;
    let mut context_window = None;
    let mut google_project = None;
    let mut google_location = None;
    let mut region = None;
    let mut deployment = None;
    let mut cf_account = None;
    let mut cf_gateway = None;
    let mut cf_provider = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut rest = rest.iter();
    while let Some(option) = rest.next() {
        let canonical = match option.as_str() {
            "--jsonl" => "--json",
            value => value,
        };
        if !seen.insert(canonical) {
            return Err(CommandError::Login("duplicate connection option"));
        }
        match canonical {
            "--json" => json = true,
            "--no-browser" if verb == "login" => no_browser = true,
            "--azure-cli" if verb == "login" => {
                if key != KeyInput::Terminal {
                    return Err(CommandError::Login("choose one credential input"));
                }
                key = KeyInput::AzureCli;
            }
            "--google-adc" if verb == "login" => {
                if key != KeyInput::Terminal {
                    return Err(CommandError::Login("choose one credential input"));
                }
                key = KeyInput::GoogleAdc;
            }
            "--aws" if verb == "login" => {
                if key != KeyInput::Terminal {
                    return Err(CommandError::Login("choose one credential input"));
                }
                key = KeyInput::AwsChain { profile: None };
            }
            "--key-stdin" if verb == "login" => {
                if key != KeyInput::Terminal {
                    return Err(CommandError::Login("choose one API key input"));
                }
                key = KeyInput::Stdin;
            }
            "--profile" | "--label" | "--api-key-env" | "--id" | "--base-url" | "--model"
            | "--context-window" | "--deployment" | "--google-project" | "--google-location"
            | "--region" | "--aws-profile" | "--cf-account" | "--cf-gateway" | "--cf-provider" => {
                let value = rest
                    .next()
                    .ok_or(CommandError::Login("connection option needs a value"))?;
                let maximum = if matches!(canonical, "--profile" | "--base-url") {
                    4096
                } else {
                    256
                };
                if value.trim().is_empty()
                    || value.len() > maximum
                    || value.chars().any(char::is_control)
                {
                    return Err(CommandError::Login(
                        "connection option is empty, oversized or contains control characters",
                    ));
                }
                match canonical {
                    "--google-project" if verb == "login" => google_project = Some(value.clone()),
                    "--google-location" if verb == "login" => google_location = Some(value.clone()),
                    "--region" if verb == "login" => region = Some(value.clone()),
                    "--deployment" if verb == "login" => deployment = Some(value.clone()),
                    "--aws-profile" if verb == "login" => {
                        if key != KeyInput::Terminal {
                            return Err(CommandError::Login("choose one credential input"));
                        }
                        key = KeyInput::AwsChain {
                            profile: Some(value.clone()),
                        };
                    }
                    "--cf-account" if verb == "login" => cf_account = Some(value.clone()),
                    "--cf-gateway" if verb == "login" => cf_gateway = Some(value.clone()),
                    "--cf-provider" if verb == "login" => cf_provider = Some(value.clone()),
                    "--profile" => profile = Some(PathBuf::from(value)),
                    "--label" if verb == "login" => label = Some(value.clone()),
                    "--base-url" if verb == "login" => base_url = Some(value.trim().to_owned()),
                    "--model" if verb == "login" => model = Some(value.trim().to_owned()),
                    "--context-window" if verb == "login" => {
                        context_window = Some(
                            value
                                .parse::<u32>()
                                .ok()
                                .filter(|value| (512..=16_777_216).contains(value))
                                .ok_or(CommandError::Login(
                                    "context window must be 512 to 16777216 tokens",
                                ))?,
                        );
                    }
                    "--id" if verb == "login" => {
                        if grapt_api::u3a::connection_target(value).is_err() {
                            return Err(CommandError::Login("connection identity is invalid"));
                        }
                        connection = Some(value.clone());
                    }
                    "--api-key-env" if verb == "login" => {
                        if key != KeyInput::Terminal {
                            return Err(CommandError::Login("choose one API key input"));
                        }
                        if !environment_name(value) {
                            return Err(CommandError::Login(
                                "environment variable name is invalid",
                            ));
                        }
                        key = KeyInput::Environment(value.clone());
                    }
                    _ => return Err(CommandError::Login("this option requires login")),
                }
            }
            other => return Err(CommandError::UnknownOption(other.into())),
        }
    }
    let vertex = if key == KeyInput::GoogleAdc {
        if !matches!(
            provider.as_deref(),
            Some("google-vertex" | "google-vertex-anthropic")
        ) {
            return Err(CommandError::Login(
                "--google-adc requires login google-vertex or google-vertex-anthropic",
            ));
        }
        if base_url.is_some() || context_window.is_some() {
            return Err(CommandError::Login(
                "Vertex uses its project and location endpoint",
            ));
        }
        Some(Box::new(VertexCoordinates {
            project: google_project.take().unwrap_or_default(),
            location: google_location.take().unwrap_or_default(),
            model: model
                .take()
                .ok_or(CommandError::Login("Google ADC requires --model"))?,
        }))
    } else {
        if google_project.is_some() || google_location.is_some() {
            return Err(CommandError::Login(
                "Google coordinates require --google-adc",
            ));
        }
        None
    };
    let bedrock = if provider.as_deref() == Some("amazon-bedrock") && verb == "login" {
        if context_window.is_some() || matches!(key, KeyInput::Environment(_)) {
            return Err(CommandError::Login(
                "Bedrock accepts a bearer token or --aws/--aws-profile with --region and --model",
            ));
        }
        Some(Box::new(BedrockCoordinates {
            region: region
                .take()
                .ok_or(CommandError::Login("Bedrock requires --region"))?,
            model: model
                .take()
                .ok_or(CommandError::Login("Bedrock requires --model"))?,
            endpoint: base_url.take(),
        }))
    } else {
        if region.is_some() || matches!(key, KeyInput::AwsChain { .. }) {
            return Err(CommandError::Login(
                "AWS credential selection and --region require login amazon-bedrock",
            ));
        }
        None
    };
    let azure = if matches!(
        provider.as_deref(),
        Some("azure" | "azure-cognitive-services")
    ) && verb == "login"
    {
        if context_window.is_some() || matches!(key, KeyInput::Environment(_)) {
            return Err(CommandError::Login(
                "Azure requires an endpoint, deployment and catalog model",
            ));
        }
        Some(Box::new(AzureCoordinates {
            endpoint: base_url
                .take()
                .ok_or(CommandError::Login("Azure requires --base-url"))?,
            deployment: deployment
                .take()
                .ok_or(CommandError::Login("Azure requires --deployment"))?,
            model: model
                .take()
                .ok_or(CommandError::Login("Azure requires --model"))?,
        }))
    } else {
        if deployment.is_some() || key == KeyInput::AzureCli {
            return Err(CommandError::Login(
                "--azure-cli and --deployment require login azure or azure-cognitive-services",
            ));
        }
        None
    };
    let endpoint = provider.as_deref().filter(|id| {
        *id == "custom" || grapt_connections::catalog::endpoint_provider(id).is_some()
    });
    let setup = match endpoint {
        Some(id) if verb == "login" => {
            if context_window.is_some() && model.is_none() {
                return Err(CommandError::Login("--context-window requires --model"));
            }
            if matches!(key, KeyInput::Environment(_)) {
                return Err(CommandError::Login(
                    "endpoint setup requires an entered key or --key-stdin",
                ));
            }
            let base_url = base_url
                .or_else(|| {
                    grapt_connections::catalog::endpoint_provider(id)
                        .and_then(|row| row.local)
                        .map(|local| local.base.to_owned())
                })
                .ok_or(CommandError::Login("enter --base-url for this provider"))?;
            Some(Box::new(EndpointCoordinates {
                base_url,
                model,
                context_window,
            }))
        }
        _ if base_url.is_some() || model.is_some() || context_window.is_some() => {
            return Err(CommandError::Login(
                "this provider does not accept endpoint setup",
            ))
        }
        _ => None,
    };
    let cloudflare = if provider.as_deref() == Some("cloudflare-ai-gateway") && verb == "login" {
        if matches!(key, KeyInput::Environment(_)) {
            return Err(CommandError::Login(
                "Cloudflare requires token input with account, gateway and provider coordinates",
            ));
        }
        let invalid_scope = || {
            CommandError::Login(
                "Cloudflare requires valid --cf-account, --cf-gateway and --cf-provider",
            )
        };
        Some(grapt_types::cloudflare::CloudflareGatewayScope::new(
            cf_account
                .ok_or_else(invalid_scope)?
                .try_into()
                .map_err(|_| invalid_scope())?,
            cf_gateway
                .ok_or_else(invalid_scope)?
                .try_into()
                .map_err(|_| invalid_scope())?,
            cf_provider
                .ok_or_else(invalid_scope)?
                .try_into()
                .map_err(|_| invalid_scope())?,
        ))
    } else {
        if cf_account.is_some() || cf_gateway.is_some() || cf_provider.is_some() {
            return Err(CommandError::Login(
                "Cloudflare coordinates require login cloudflare-ai-gateway",
            ));
        }
        None
    };
    let action = match provider {
        None if listing => {
            if no_browser || key != KeyInput::Terminal || label.is_some() || connection.is_some() {
                return Err(CommandError::Login(
                    "provider listing accepts only --profile and --json",
                ));
            }
            ConnectionAction::List
        }
        None => ConnectionAction::Status,
        Some(provider) if verb == "logout" => ConnectionAction::Logout { provider },
        Some(provider) => {
            if provider == "chatgpt" && key != KeyInput::Terminal {
                return Err(CommandError::Login(
                    "ChatGPT uses OAuth; use openai for an API key",
                ));
            }
            if no_browser && provider != "chatgpt" {
                return Err(CommandError::Login(
                    "--no-browser applies to ChatGPT sign-in",
                ));
            }
            if json && provider != "chatgpt" && key == KeyInput::Terminal {
                return Err(CommandError::Login(
                    "JSON login requires --key-stdin or --api-key-env",
                ));
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
            }
        }
    };
    Ok(CliCommand::Connections(ConnectionCommand {
        profile,
        json,
        action,
    }))
}

fn environment_name(name: &str) -> bool {
    name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn words(input: &[&str]) -> Vec<String> {
        input.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn connection_operators_refuse_secret_argv_and_conflicting_input_modes() {
        for args in [
            vec!["chatgpt", "--profile"],
            vec!["chatgpt", "--key-stdin"],
            vec!["anthropic", "--key", "secret"],
            vec!["openai", "--json"],
            vec!["openai", "--key-stdin", "--api-key-env", "OPENAI_API_KEY"],
            vec!["openai", "--api-key-env", "../secret"],
            vec!["openai", "--api-key-env", "KEY=VALUE"],
            vec!["openai", "--profile", "a", "--profile", "b"],
            vec!["openai", "--id", "../foreign"],
            vec!["openai", "--id", ".hidden"],
            vec!["openai", "--json", "--jsonl"],
        ] {
            assert!(parse(&words(&args)).is_err(), "{args:?}");
        }
        let parsed = parse(&words(&[
            "anthropic",
            "--api-key-env",
            "ANTHROPIC_API_KEY",
            "--json",
            "--profile",
            "/p",
        ]));
        assert_eq!(
            parsed.ok(),
            Some(CliCommand::Connections(ConnectionCommand {
                profile: Some(PathBuf::from("/p")),
                json: true,
                action: ConnectionAction::Login {
                    provider: "anthropic".into(),
                    key: KeyInput::Environment("ANTHROPIC_API_KEY".into()),
                    no_browser: false,
                    label: None,
                    connection: None,
                    setup: None,
                    vertex: None,
                    bedrock: None,
                    azure: None,
                    cloudflare: None,
                },
            }))
        );
        assert!(matches!(
            parse(&[]),
            Ok(CliCommand::Connections(ConnectionCommand {
                action: ConnectionAction::List,
                ..
            }))
        ));
        assert!(operator("logout", &words(&["openai", "--key-stdin"])).is_err());
        assert!(operator("auth", &words(&["status", "--json"])).is_ok());
        assert!(parse(&words(&["chatgpt", "--no-browser", "--json"])).is_ok());
    }

    #[test]
    fn a_local_preset_needs_no_coordinates_and_a_window_needs_its_model() {
        let setup = |args: &[&str]| match parse(&words(args)) {
            Ok(CliCommand::Connections(ConnectionCommand {
                action: ConnectionAction::Login { setup, .. },
                ..
            })) => setup.map(|setup| *setup),
            _ => None,
        };
        assert_eq!(
            setup(&["ollama"]),
            Some(EndpointCoordinates {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: None,
                context_window: None,
            })
        );
        assert_eq!(
            setup(&[
                "vllm",
                "--base-url",
                "http://127.0.0.1:9000/v1",
                "--model",
                "m"
            ]),
            Some(EndpointCoordinates {
                base_url: "http://127.0.0.1:9000/v1".into(),
                model: Some("m".into()),
                context_window: None,
            })
        );
        assert!(parse(&words(&["ollama", "--context-window", "65536"])).is_err());
        assert!(
            parse(&words(&["custom"])).is_err(),
            "custom has no usual address"
        );
        assert!(parse(&words(&["openai", "--model", "m"])).is_err());
    }
}

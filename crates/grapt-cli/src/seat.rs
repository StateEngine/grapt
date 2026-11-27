//! Explicit seat provisioning through the existing authenticated product APIs.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use grapt_api::u3a::{InstructionEntryPutDto, SessionCapabilityPutDto};
use grapt_api::SessionCreateRequestDto;
use grapt_types::{AgentId, ModelId, ProviderId, Secret, SessionId};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use crate::{ApiClient, CliLimits, CommandError, CommandFailure, ConnectionChoice};

const MAX_CAPABILITY: usize = 65_536;
const MAX_ENTRY: usize = 16_384;
const INPUT_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionCommand {
    service_root: PathBuf,
    session: SessionId,
    agent: AgentId,
    provider: ProviderId,
    model: ModelId,
    location: String,
    identity: String,
    packet: String,
    config_path: Option<String>,
}

impl ProvisionCommand {
    pub(crate) fn parse(arguments: &[String]) -> Result<Self, CommandError> {
        if arguments.first().map(String::as_str) != Some("provision") || arguments.len() != 15 {
            return Err(CommandError::UnknownOption("session provision requires --service, --session, --agent, --model, --location, --identity-entry and --packet-entry".into()));
        }
        let mut values = BTreeMap::new();
        for pair in arguments[1..].chunks_exact(2) {
            let key = pair[0].as_str();
            let name = match key {
                "--service" => "--service",
                "--session" => "--session",
                "--agent" => "--agent",
                "--model" => "--model",
                "--location" => "--location",
                "--identity-entry" => "--identity-entry",
                "--packet-entry" => "--packet-entry",
                _ => return Err(CommandError::UnknownOption(key.to_owned())),
            };
            let value = pair[1].as_str();
            let maximum = if matches!(key, "--identity-entry" | "--packet-entry") {
                MAX_ENTRY
            } else {
                8192
            };
            if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
                return Err(CommandError::UnknownOption(format!(
                    "{name} must be a bounded nonempty value without control characters"
                )));
            }
            if values.insert(name, value).is_some() {
                return Err(CommandError::Duplicate(name));
            }
        }
        let get = |key: &'static str| {
            values
                .get(key)
                .copied()
                .ok_or(CommandError::MissingValue(key))
        };
        let (provider, model) = get("--model")?
            .split_once('/')
            .ok_or_else(|| CommandError::UnknownOption("--model requires provider/model".into()))?;
        let command = Self {
            service_root: PathBuf::from(get("--service")?),
            session: get("--session")?
                .parse()
                .map_err(|_| CommandError::InvalidSession)?,
            agent: AgentId::new(get("--agent")?)
                .ok_or_else(|| CommandError::UnknownOption("invalid --agent".into()))?,
            provider: ProviderId::new(provider)
                .ok_or_else(|| CommandError::UnknownOption("invalid provider".into()))?,
            model: ModelId::new(model)
                .ok_or_else(|| CommandError::UnknownOption("invalid model".into()))?,
            location: get("--location")?.to_owned(),
            identity: get("--identity-entry")?.to_owned(),
            packet: get("--packet-entry")?.to_owned(),
            config_path: None,
        };
        command.entries().map_err(|_| {
            CommandError::UnknownOption(
                "identity and packet entries must be valid launch JSON objects".into(),
            )
        })?;
        Ok(command)
    }

    fn entries(&self) -> Result<(InstructionEntryPutDto, InstructionEntryPutDto), ()> {
        // Decode with the instruction DTO before inspection: parsing through
        // Value first would erase duplicate fields and defeat its closed bounds.
        let identity_entry: InstructionEntryPutDto =
            serde_json::from_str(&format!("{{\"value\":{}}}", self.identity)).map_err(|_| ())?;
        let packet_entry: InstructionEntryPutDto =
            serde_json::from_str(&format!("{{\"value\":{}}}", self.packet)).map_err(|_| ())?;
        let identity = identity_entry.value().as_value().ok_or(())?.value();
        let packet = packet_entry.value().as_value().ok_or(())?.value();
        if identity.get("scope").and_then(Value::as_str) != Some("project")
            || !["project_id", "agent_id"].iter().all(|key| {
                identity
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty() && value.len() <= 256)
            })
            || identity
                .get("mail_transport_protocol")
                .and_then(Value::as_str)
                != Some("remote-seat-mail-transport/v2")
            || packet.get("state").and_then(Value::as_str) != Some("written")
            || !packet.is_object()
        {
            return Err(());
        }
        Ok((identity_entry, packet_entry))
    }

    fn create(&self) -> Result<SessionCreateRequestDto, String> {
        serde_json::from_value(
            json!({"id":self.session.as_str(),"agent":self.agent.as_str(),
            "model":{"providerID":self.provider.as_str(),"id":self.model.as_str()},
            "location":{"directory":self.location}}),
        )
        .map_err(|_| "seat selection is invalid".into())
    }

    /// Never retries a write or deletes a partly provisioned Session. A new
    /// launch uses a new issuer-minted identity after a partial failure.
    pub async fn provision(
        &self,
        client: &ApiClient,
        capability: Secret<String>,
    ) -> Result<(), CommandFailure> {
        if capability.expose().is_empty()
            || capability.expose().len() > MAX_CAPABILITY
            || capability.expose().chars().any(char::is_control)
        {
            return Err("capability must be one bounded nonempty value".into());
        }
        let (identity, packet) = self.entries().map_err(|_| "launch entries are invalid")?;
        let mut binding = json!({"capability":capability.expose()});
        if let Some(path) = &self.config_path {
            binding["configPath"] = json!(path);
        }
        let capability: SessionCapabilityPutDto =
            serde_json::from_value(binding).map_err(|_| "capability is invalid")?;
        let created = client
            .create_seat(&self.create()?)
            .await
            .map_err(|error| CommandFailure::from(error).context("session create failed"))?;
        if created.data().get().id().get().as_inner() != &self.session {
            return Err("created session differs from the requested launch identity".into());
        }
        client
            .put_seat_entry(&self.session, "stateengine-packet", &packet)
            .await
            .map_err(|error| CommandFailure::from(error).context("packet entry failed"))?;
        client
            .put_seat_entry(&self.session, "stateengine-identity", &identity)
            .await
            .map_err(|error| CommandFailure::from(error).context("identity entry failed"))?;
        // A hostile peer may reflect the submitted secret in its error body.
        client
            .put_seat_capability(&self.session, &capability)
            .await
            .map_err(|error| {
                CommandFailure::new(
                    error.exit_class(),
                    "capability bind failed; repair the service binding before relaunching"
                        .to_owned(),
                )
            })?;
        Ok(())
    }
}

pub(crate) async fn execute(mut command: ProvisionCommand) -> u8 {
    let result = async {
        command.config_path = config_selector()?;
        let client = crate::connect(ConnectionChoice::Discover { service_root: command.service_root.clone() }, CliLimits::production())
            .await.map_err(|(class, message)| CommandFailure::new(class, format!("service {} is unavailable: {message}; start the configured service before launching this seat", command.service_root.display())))?;
        command.provision(&client, read_capability(tokio::io::stdin()).await?).await
    }.await;
    match result {
        Ok(()) => {
            println!("provisioned {}", command.session);
            0
        }
        Err(error) => {
            eprintln!("grapt: seat {}: {error}", command.session);
            error.exit_class().code()
        }
    }
}

fn config_selector() -> Result<Option<String>, String> {
    selected_path(
        std::env::var_os("GRAPT_CONFIG")
            .or_else(|| std::env::var_os(crate::branding_compat::PRE_RENAME_CONFIG_ENV)),
        std::env::var_os(crate::branding_compat::LEGACY_CONFIG_ENV),
    )
}

fn selected_path(
    modern: Option<std::ffi::OsString>,
    legacy: Option<std::ffi::OsString>,
) -> Result<Option<String>, String> {
    let selected = modern.or(legacy);
    selected
        .map(|raw| {
            let value = raw
                .into_string()
                .map_err(|_| "Session config selector is not Unicode")?;
            if value.is_empty()
                || value.len() > 4096
                || value.chars().any(char::is_control)
                || ![grapt_types::Platform::Posix, grapt_types::Platform::Windows]
                    .into_iter()
                    .any(|platform| grapt_types::LocationRef::on(platform, &value, None).is_ok())
            {
                return Err("Session config selector must be a bounded absolute path".to_owned());
            }
            Ok(value)
        })
        .transpose()
}

async fn read_capability(
    input: impl tokio::io::AsyncRead + Unpin,
) -> Result<Secret<String>, String> {
    let mut raw = Vec::new();
    // One overflow byte beyond the longest valid line (capability + CRLF)
    // prevents a valid prefix from concealing trailing input at the bound.
    tokio::time::timeout(
        INPUT_WAIT,
        input
            .take((MAX_CAPABILITY + 3) as u64)
            .read_to_end(&mut raw),
    )
    .await
    .map_err(|_| "capability stdin timed out")?
    .map_err(|_| "capability stdin is unreadable")?;
    stdin_capability(raw)
}

fn stdin_capability(raw: Vec<u8>) -> Result<Secret<String>, String> {
    let raw = String::from_utf8(raw).map_err(|_| "capability stdin must be UTF-8")?;
    let value = raw.strip_suffix('\n').unwrap_or(&raw);
    let value = value.strip_suffix('\r').unwrap_or(value);
    if value.is_empty() || value.len() > MAX_CAPABILITY || value.chars().any(char::is_control) {
        return Err("capability stdin must contain one bounded capability".to_owned());
    }
    Ok(Secret::new(value.to_owned()))
}

#[cfg(test)]
mod tests;

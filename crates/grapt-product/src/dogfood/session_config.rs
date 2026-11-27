//! Session-owned MCP configuration. No capability or process environment is persisted.
//! Refresh snapshots.
// Copyright 2025 OpenAI. Modified by Grapt; see licenses/openai-codex/.
use super::catalog_config::{self, CatalogDocument, CatalogEnvironment, CatalogSources};
use grapt_instruction::InstructionService;
use grapt_session::config_binding::{self, ConfigBinding, Selection};
use grapt_types::SessionId;
use serde_json::Value;
use std::{path::Path, sync::Mutex};

pub(super) mod catalog;
#[cfg(test)]
mod tests;

pub(super) fn identity(
    inbox: &grapt_inbox::Inbox,
    session: &SessionId,
) -> Result<
    (
        grapt_types::remote_mail::SourceProjectId,
        grapt_types::remote_mail::SourcePrincipalId,
    ),
    String,
> {
    let entry = InstructionService::entries(inbox.store(), session)
        .map_err(|_| "Session identity is unreadable")?
        .into_iter()
        .find(|entry| entry.key.as_str() == "stateengine-identity")
        .ok_or("Session config requires a StateEngine identity")?;
    let value: Value =
        serde_json::from_str(&entry.value).map_err(|_| "Session identity is corrupt")?;
    if value["scope"] != "project" {
        return Err("Session config requires project scope".into());
    }
    let field = |key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|raw| !raw.is_empty() && raw.len() <= 256 && !raw.chars().any(char::is_control))
            .map(str::to_owned)
            .ok_or_else(|| "Session config identity is invalid".to_owned())
    };
    Ok((
        grapt_types::remote_mail::SourceProjectId::try_new(field("project_id")?)
            .map_err(|_| "Session project is invalid")?,
        grapt_types::remote_mail::SourcePrincipalId::try_new(field("agent_id")?)
            .map_err(|_| "Session agent is invalid")?,
    ))
}

fn current(inbox: &grapt_inbox::Inbox, session: &SessionId) -> Result<Option<Selection>, String> {
    let binding = config_binding::read(inbox.store().connection(), session)?;
    if let Some(selection) = &binding {
        let binding = &selection.binding;
        if identity(inbox, session)? != (binding.project.clone(), binding.agent.clone()) {
            return Err(
                "Session config binding differs from its current identity; provision again".into(),
            );
        }
    }
    Ok(binding)
}

pub(super) fn revision(
    inbox: &grapt_inbox::Inbox,
    session: &SessionId,
) -> Result<Option<u64>, String> {
    current(inbox, session).map(|selection| selection.map(|selection| selection.sequence))
}

/// Compare and publish under the same short lock used by a rebind commit.
pub(super) fn publish_current(
    inbox: &Mutex<grapt_inbox::Inbox>,
    session: &SessionId,
    expected: Option<u64>,
    publish: impl FnOnce(),
) -> Result<(), String> {
    let held = inbox
        .lock()
        .map_err(|_| "Session config custody unavailable")?;
    if revision(&held, session)? != expected {
        return Err("Session config changed during publication; retry the turn".into());
    }
    publish();
    Ok(())
}

/// The actual opened bytes, not a path check followed by a second catalog read.
pub(super) fn document(
    binding: &ConfigBinding,
    session: &SessionId,
    directory: &Path,
    service: &Path,
) -> Result<CatalogDocument, String> {
    let path = Path::new(&binding.path);
    if !path.is_absolute()
        || binding.path.len() > 4096
        || binding.path.chars().any(char::is_control)
    {
        return Err("Session config path must be a bounded absolute path".into());
    }
    let origin = grapt_plugin::PluginOrigin::LaunchExplicit;
    let check_origin = || {
        let origin = super::catalog_origin::source_origin(path, origin, directory, service)?;
        if !catalog_config::user_owned(origin) {
            return Err(
                "Session config must be outside repositories and .grapt ancestry".to_owned(),
            );
        }
        let canonical =
            std::fs::canonicalize(path).map_err(|_| "Session config cannot be resolved")?;
        if canonical
            .ancestors()
            .take(catalog_config::MAX_ANCESTORS + 1)
            .count()
            > catalog_config::MAX_ANCESTORS
        {
            return Err("Session config origin exceeds its ancestor bound".to_owned());
        }
        Ok(canonical)
    };
    let before = check_origin()?;
    let bytes =
        grapt_login::profile_security::read_owner_only_file(path, catalog_config::DOCUMENT_BYTES)
            .map_err(|_| "Session config is unreadable or lacks owner-only file protection")?;
    if check_origin()? != before {
        return Err("Session config changed origin while reading".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if std::fs::metadata(path)
            .map_err(|_| "Session config metadata is unreadable")?
            .uid()
            != std::fs::metadata(service)
                .map_err(|_| "Service owner is unreadable")?
                .uid()
        {
            return Err("Session config is not owned by the service user".into());
        }
    }
    let value = grapt_config::decode(&bytes, catalog_config::bounds()?)
        .map_err(|_| "Session config JSON is invalid")?
        .into_value();
    let json = super::catalogs::config_json(&value)?;
    let environment = &json["mcp"]["stateengine"]["environment"];
    if environment["STATEENGINE_PROJECT_ID"].as_str() != Some(binding.project.as_str())
        || environment["STATEENGINE_AGENT_ID"].as_str() != Some(binding.agent.as_str())
        || environment["SECODE_SESSION_ID"].as_str() != Some(session.as_str())
    {
        return Err(
            "Session config MCP environment differs from its project, agent or Session".into(),
        );
    }
    Ok(CatalogDocument {
        directory: path
            .parent()
            .ok_or("Session config has no parent")?
            .to_path_buf(),
        source: binding.path.clone(),
        origin,
        value,
    })
}

pub(super) fn sources(
    inbox: &Mutex<grapt_inbox::Inbox>,
    session: &SessionId,
    service: &Path,
    directory: &Path,
) -> Result<CatalogSources, String> {
    let binding = current(
        &*inbox
            .lock()
            .map_err(|_| "Session config custody unavailable")?,
        session,
    )?;
    let mut environment = CatalogEnvironment::read(service)?;
    let Some(binding) = binding else {
        return CatalogSources::read(directory, environment);
    };
    environment.clear_explicit();
    let mut sources = CatalogSources::read(directory, environment)?;
    let document = document(&binding.binding, session, directory, service)?;
    if current(
        &*inbox
            .lock()
            .map_err(|_| "Session config custody unavailable")?,
        session,
    )?
    .as_ref()
        != Some(&binding)
    {
        return Err("Session config was rebound during publication".into());
    }
    sources
        .document_paths
        .push(Path::new(&binding.binding.path).to_path_buf());
    sources.documents.push(document);
    Ok(sources)
}

pub(super) fn validate_selected(
    inbox: &Mutex<grapt_inbox::Inbox>,
    session: &SessionId,
    service: &Path,
    directory: &Path,
) -> Result<(), String> {
    let selected = current(
        &*inbox
            .lock()
            .map_err(|_| "Session config custody unavailable")?,
        session,
    )?;
    if let Some(selected) = selected {
        document(&selected.binding, session, directory, service)?;
    }
    Ok(())
}

pub(super) fn registry_key(
    inbox: &Mutex<grapt_inbox::Inbox>,
    session: &SessionId,
    directory: &str,
) -> Result<String, String> {
    Ok(key_for_revision(
        session,
        directory,
        revision(
            &*inbox
                .lock()
                .map_err(|_| "Session config custody unavailable")?,
            session,
        )?,
    ))
}

pub(super) fn key_for_revision(
    session: &SessionId,
    directory: &str,
    revision: Option<u64>,
) -> String {
    let directory = grapt_types::path::comparison_key(directory);
    match revision {
        Some(sequence) => format!("session:{session}:{sequence}:{directory}"),
        None => directory,
    }
}

/// Refuse a previously planned route before it receives replacement custody.
pub(super) fn validate_registry_key(
    inbox: &grapt_inbox::Inbox,
    session: &SessionId,
    key: &str,
) -> Result<(), String> {
    let Some(sequence) = revision(inbox, session)? else {
        return Ok(());
    };
    let record = grapt_session::SessionStore::new(inbox.store().connection())
        .record(session)
        .map_err(|_| "Session placement is unreadable")?;
    if key != key_for_revision(session, record.placement.directory(), Some(sequence)) {
        return Err("Session config was rebound; retry with its current MCP catalog".into());
    }
    Ok(())
}

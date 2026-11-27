//! Exact project-server approvals are read only from user-owned documents.
use super::{config_json, CatalogSources};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
#[serde(untagged)]
enum Approval {
    Grant(Grant),
    Revoked(Revocation),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revocation {
    revoked: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    fingerprint: String,
    enabled: bool,
}

pub(in crate::dogfood) struct ProjectServer {
    pub fingerprint: String,
    pub source: String,
    pub summary: String,
    pub approved: bool,
}

pub(super) fn preview(
    location: &str,
    source: &str,
    name: &str,
    fields: &Map<String, Value>,
    global: super::McpTimeouts,
) -> Result<ProjectServer, String> {
    let budgets = (
        global.startup.as_millis(),
        global.catalog.as_millis(),
        global.execution.as_millis(),
    );
    let encoded = serde_json::to_vec(&(location, source, name, fields, budgets))
        .map_err(|_| "MCP project declaration cannot be fingerprinted")?;
    let mut summary = match fields.get("type").and_then(Value::as_str) {
        Some("local") => serde_json::to_string(fields.get("command").unwrap_or(&Value::Null))
            .map_err(|_| "MCP command preview cannot be encoded")?,
        Some("remote") => fields
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("Remote server URL is missing")
            .escape_debug()
            .to_string(),
        _ => "Server type is missing; inspect its configuration before approval".into(),
    };
    if let Some(cwd) = fields.get("cwd") {
        summary.push_str(&format!("\nWorking directory: {cwd}"));
    }
    if let Some(environment) = fields.get("environment").and_then(Value::as_object) {
        let names: Vec<&str> = environment.keys().map(String::as_str).collect();
        let names = serde_json::to_string(&names)
            .map_err(|_| "MCP environment names cannot be previewed")?;
        summary.push_str(&format!("\nEnvironment keys (values hidden): {names}"));
    }
    if summary.len() > 8192 {
        return Err("MCP declaration exceeds its approval preview bound".into());
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, &encoded);
    let fingerprint = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(ProjectServer {
        fingerprint,
        source: source.into(),
        summary,
        approved: false,
    })
}

pub(super) struct Approvals(std::collections::BTreeMap<String, Approval>);

impl Approvals {
    pub(super) fn read(sources: &CatalogSources, location: &str) -> Result<Self, String> {
        let mut selected = std::collections::BTreeMap::new();
        let mut decisions = std::collections::BTreeMap::new();
        for document in &sources.documents {
            if !super::super::catalog_config::user_owned(document.origin) {
                continue;
            }
            let Some(approvals) = document.value.get("mcpProjectApprovals") else {
                continue;
            };
            let values =
                config_json(approvals.value()).map_err(|reason| document.refusal(&reason))?;
            let projects = values
                .as_object()
                .ok_or_else(|| document.refusal("MCP project approvals must be an object"))?;
            if projects.len() > 256 {
                return Err(document.refusal("MCP project approvals exceed 256 projects"));
            }
            let Some(project) = projects.get(location) else {
                continue;
            };
            let servers = project
                .as_object()
                .ok_or_else(|| document.refusal("MCP project approval must be an object"))?;
            if servers.len() > 256 {
                return Err(document.refusal("MCP project approvals exceed 256 servers"));
            }
            for (name, value) in servers {
                let approval: Approval = serde_json::from_value(value.clone()).map_err(|_| {
                    document.refusal("MCP project approval requires a grant or explicit revocation")
                })?;
                match &approval {
                    Approval::Grant(grant)
                        if grant.fingerprint.len() == 64
                            && grant
                                .fingerprint
                                .bytes()
                                .all(|byte| byte.is_ascii_hexdigit()) => {}
                    Approval::Revoked(value) if value.revoked => {}
                    _ => return Err(document.refusal("MCP project approval decision is invalid")),
                }
                // The operator's writable file owns explicit decisions. A revoke
                // must not reveal a grant in a profile or higher config layer.
                let source = std::path::Path::new(&document.source);
                // Only the spelling in use is ever read, so either spelling
                // here is the file the MCP writer edits.
                let writable =
                    crate::dogfood::legacy_names::every_config_file(&sources.environment.config)
                        .iter()
                        .any(|path| source == path);
                if writable {
                    decisions.insert(name.clone(), approval);
                } else {
                    selected.insert(name.clone(), approval);
                }
                if selected.len() > 256 || decisions.len() > 256 {
                    return Err(
                        document.refusal("MCP project approvals exceed 256 selected servers")
                    );
                }
            }
        }
        selected.extend(decisions);
        if selected.len() > 256 {
            return Err("MCP project approvals exceed 256 selected servers".into());
        }
        Ok(Self(selected))
    }

    pub(super) fn enabled(&self, name: &str, fingerprint: &str) -> Option<bool> {
        match self.0.get(name) {
            Some(Approval::Grant(grant)) if grant.fingerprint == fingerprint => Some(grant.enabled),
            Some(Approval::Grant(_) | Approval::Revoked(_)) | None => None,
        }
    }
}

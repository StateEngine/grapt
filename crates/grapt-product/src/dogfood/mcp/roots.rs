//! Workspace roots belong to the approved connection, outside model catalogs.
//! Client request dispatch extends the existing handlers.
// Copyright 2025 OpenAI. Modified by Grapt; see licenses/openai-codex/.
use super::{Declaration, McpRegistry};
mod authority;
use crate::dogfood::catalog_config::{user_owned, CatalogSources};
pub(super) use authority::Authority;
pub(in crate::dogfood) use authority::{EditGuard, Revision};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::dogfood) struct Config {
    // Keep the placement spelling for approval ownership; the advertised root
    // resolves parent segments before URL normalization can change their meaning.
    location: PathBuf,
    workspace: PathBuf,
    additional: Vec<PathBuf>,
}

impl Config {
    pub(in crate::dogfood) fn read(
        sources: &CatalogSources,
        workspace: &str,
    ) -> Result<Self, String> {
        let mut additional = Vec::new();
        for document in &sources.documents {
            let Some(roots) = document.value.get("mcpRoots") else {
                continue;
            };
            let roots = roots
                .value()
                .as_array()
                .filter(|roots| roots.len() <= 32)
                .ok_or_else(|| document.refusal("mcpRoots must be an array of at most 32 paths"))?;
            let paths = roots
                .iter()
                .map(|root| {
                    root.value()
                        .as_str()
                        .ok_or("mcpRoots entries must be absolute directory paths")
                        .and_then(path)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|reason| document.refusal(reason))?;
            // A repository can configure a server, but cannot disclose other
            // folders through a declaration that a model or checkout controls.
            if user_owned(document.origin) {
                additional = paths;
            }
        }
        additional.sort();
        additional.dedup();
        Ok(Self {
            location: PathBuf::from(workspace),
            workspace: workspace_path(workspace)?,
            additional,
        })
    }

    fn response(&self) -> Result<Value, String> {
        let mut seen = BTreeSet::new();
        let mut roots = Vec::new();
        for path in std::iter::once(&self.workspace).chain(&self.additional) {
            // These are validated declarations, not filesystem access grants.
            // Never block the protocol reader probing a disk or network share.
            let uri = url::Url::from_file_path(path)
                .map_err(|_| "an MCP root cannot be represented by a file URI")?
                .to_string();
            if uri.len() > 16_384 {
                return Err("an MCP root URI exceeds its bound".into());
            }
            if seen.insert(uri.clone()) {
                roots.push(json!({"uri":uri}));
            }
        }
        Ok(json!({"roots":roots}))
    }
}

fn absolute_path(value: &str) -> Result<&Path, &'static str> {
    let path = Path::new(value);
    if value.len() > 4096 || value.chars().any(char::is_control) || !path.is_absolute() {
        return Err("MCP roots require bounded absolute directory paths");
    }
    Ok(path)
}

fn path(value: &str) -> Result<PathBuf, &'static str> {
    let path = absolute_path(value)?;
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("mcpRoots entries must not contain parent traversal");
    }
    Ok(path.components().collect())
}

fn workspace_path(value: &str) -> Result<PathBuf, &'static str> {
    let path = absolute_path(value)?;
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        // Catalog preparation already reads this directory. Resolve only this
        // placement spelling here, never on the protocol reader or for a user root.
        return std::fs::canonicalize(path).map_err(|_| "MCP workspace is unreadable");
    }
    Ok(path.components().collect())
}

#[derive(Debug, Default)]
struct Snapshot {
    config: Option<Config>,
    asked: bool,
    changed: bool,
    retired: bool,
}

#[derive(Debug, Default)]
pub(super) struct State(Mutex<Snapshot>);

impl State {
    pub(super) fn new(config: Option<Config>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Snapshot {
            config,
            ..Snapshot::default()
        })))
    }

    pub(super) fn set(&self, config: Option<Config>) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "MCP roots custody unavailable")?;
        let config = config.filter(|_| !state.retired);
        if state.config != config {
            state.config = config;
            state.changed |= state.asked;
        }
        Ok(())
    }

    pub(super) fn revoke(&self) {
        // A poisoned state cannot answer a request, so it already fails closed.
        let _ = self.set(None);
    }

    pub(super) fn retire(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.retired = true;
            state.config = None;
            state.changed |= state.asked;
        }
    }

    pub(super) fn response(&self) -> Result<Value, String> {
        let config = {
            let mut state = self.0.lock().map_err(|_| "MCP roots custody unavailable")?;
            state.asked = true;
            state.changed = false;
            state.config.clone()
        };
        config.map_or_else(|| Ok(json!({"roots":[]})), |config| config.response())
    }

    pub(super) fn take_changed(&self) -> Result<bool, String> {
        let mut state = self.0.lock().map_err(|_| "MCP roots custody unavailable")?;
        Ok(std::mem::take(&mut state.changed))
    }
}

impl McpRegistry {
    pub(in crate::dogfood) fn roots_revision(&self) -> Result<Revision, String> {
        self.roots_authority
            .lock()
            .map_err(|_| "MCP roots custody unavailable")?
            .revision()
    }

    pub(in crate::dogfood) fn edit_roots(
        &self,
        workspace: Option<&str>,
        server: &str,
    ) -> Result<EditGuard, String> {
        Authority::edit(&self.roots_authority, workspace, server)
    }

    pub(in crate::dogfood) async fn configure_roots(
        &self,
        location: &str,
        roots: &Config,
        declared: &BTreeMap<String, Declaration>,
        revision: Revision,
    ) -> Result<(), String> {
        let runtime = self
            .runtime
            .lock()
            .await
            .get(location)
            .cloned()
            .unwrap_or_default();
        let mut locations = self.entries.lock().await;
        let mut authority = self
            .roots_authority
            .lock()
            .map_err(|_| "MCP roots custody unavailable")?;
        authority.validate(revision)?;
        if !super::release_over_bound(
            &mut locations,
            location,
            self.retained_locations,
            self.clock.now().as_millis(),
        ) {
            return Err("MCP location capacity reached".into());
        }
        let held = locations.entry(location.into()).or_default();
        authority.configure(location, roots, declared, &runtime)?;
        let mut permitted = declared.clone();
        permitted.extend(runtime);
        held.roots = Some(Prepared {
            config: roots.clone(),
            revision,
            permitted,
        });
        Ok(())
    }

    pub(super) async fn roots_for(
        &self,
        location: &str,
        server: &str,
        declaration: &Declaration,
    ) -> Arc<State> {
        let locations = self.entries.lock().await;
        let prepared = locations.get(location).and_then(|held| held.roots.as_ref());
        let Ok(mut authority) = self.roots_authority.lock() else {
            return State::new(None);
        };
        authority.connect(location, server, declaration, prepared)
    }
}

#[derive(Debug)]
pub(super) struct Prepared {
    config: Config,
    revision: Revision,
    permitted: BTreeMap<String, Declaration>,
}

impl grapt_mcp_host::WorkspaceRoots for State {
    fn response(&self) -> Result<Value, String> {
        State::response(self)
    }
    fn take_changed(&self) -> Result<bool, String> {
        State::take_changed(self)
    }
}

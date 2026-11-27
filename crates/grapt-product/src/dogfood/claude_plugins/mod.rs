//! Installed Claude packages compose into the existing catalogs, never the JS supervisor.
mod acquisition;
mod acquisition_guard;
pub(in crate::dogfood) mod agents;
mod components;
mod destination;
mod git_command;
mod git_tree;
pub(in crate::dogfood) mod hooks;
mod operation;
mod remote;
mod snapshot;
#[cfg(test)]
mod tests;

use super::catalog_config::{CatalogEnvironment, CatalogSources};
use grapt_plugin::claude::{Manifest, Marketplace, Name, PluginId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snapshot::{io, Snapshot};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) use components::{activate_skill, commands, skills};
pub(crate) use operation::Installer;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    #[serde(default)]
    marketplaces: BTreeMap<Name, Market>,
    #[serde(default)]
    installed: BTreeMap<PluginId, Installation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Market {
    root: PathBuf,
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote: Option<RemoteMarket>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteMarket {
    source: grapt_plugin::claude::GitSource,
    commit: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    root: PathBuf,
    digest: String,
    namespace: Name,
    bytes: usize,
    entry: Value,
}
struct Loaded {
    installation: Installation,
    snapshot: Snapshot,
    manifest: Manifest,
    data: PathBuf,
}
fn registry(value: Option<&Value>) -> Result<Registry, String> {
    let registry: Registry = match value {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| "Claude plugin registry is malformed")?,
        None => Registry::default(),
    };
    if registry.marketplaces.len() > 64 || registry.installed.len() > 64 {
        return Err("Claude plugin registry exceeds 64 marketplaces or installs".into());
    }
    if registry
        .installed
        .values()
        .try_fold(0usize, |sum, entry| sum.checked_add(entry.bytes))
        .is_none_or(|bytes| bytes > 128 * 1024 * 1024)
    {
        return Err("installed plugin assets exceed 128 MiB".into());
    }
    Ok(registry)
}
fn data_path(config: &Path, selector: &str) -> PathBuf {
    // A full selector hash avoids the upstream sanitization alias between IDs.
    let digest =
        grapt_plugin::claude::tree_digest(std::iter::once((selector, b"".as_slice(), false)));
    config.join("claude-plugins/data").join(digest)
}
fn loaded(sources: &CatalogSources) -> Result<Vec<Loaded>, String> {
    let mut records = BTreeMap::new();
    for document in &sources.documents {
        let Some(value) = document.value.get("claudePlugins") else {
            continue;
        };
        if !super::catalog_config::user_owned(document.origin) {
            return Err(document.refusal("Claude plugin installs require user-owned configuration"));
        }
        for (key, record) in
            registry(Some(&super::catalogs::config_json(value.value())?))?.installed
        {
            if records.insert(key, record).is_some() {
                return Err("duplicate Claude plugin installation in configuration sources".into());
            }
        }
    }
    let config = &sources.environment.config;
    let cache = config.join("claude-plugins/cache");
    if records.len() > 64
        || records
            .values()
            .try_fold(0usize, |sum, entry| sum.checked_add(entry.bytes))
            .is_none_or(|bytes| bytes > 128 * 1024 * 1024)
    {
        return Err("effective installed plugin catalog exceeds its count or 128 MiB bound".into());
    }
    let mut result = Vec::new();
    for (id, installation) in records {
        if installation.root.parent() != Some(cache.as_path()) {
            return Err("installed plugin root is outside its user cache".into());
        }
        let snapshot = Snapshot::capture(&installation.root)?;
        if snapshot.digest != installation.digest || snapshot.bytes != installation.bytes {
            return Err("installed plugin tree changed; activation refused".into());
        }
        let manifest = components::validate(&snapshot, &installation.entry)?;
        if manifest.name != installation.namespace {
            return Err("installed plugin namespace changed".into());
        }
        let data = data_path(config, &id.to_string());
        result.push(Loaded {
            installation,
            snapshot,
            manifest,
            data,
        });
    }
    if result.len() > 64 {
        return Err("effective Claude plugin catalog exceeds 64 installations".into());
    }
    Ok(result)
}
fn read_market(root: &Path) -> Result<(Marketplace, String), String> {
    let relative = Path::new(".claude-plugin/marketplace.json");
    let opened = super::source_file::open_contained(root, relative).map_err(io)?;
    let metadata = opened.file.metadata().map_err(io)?;
    snapshot::unlinked(&metadata)?;
    if !metadata.is_file() || !opened.inside(root).map_err(io)? {
        return Err("marketplace catalog must be a contained regular file".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    (&opened.file)
        .take(grapt_plugin::claude::DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    parse_market(&bytes)
}
fn parse_market(bytes: &[u8]) -> Result<(Marketplace, String), String> {
    let marketplace = Marketplace::parse(bytes)?;
    let digest =
        grapt_plugin::claude::tree_digest(std::iter::once(("marketplace.json", bytes, false)));
    Ok((marketplace, digest))
}

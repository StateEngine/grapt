//! Host-admitted filesystem roots and HTTP origins for one plugin process.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PluginAccess {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    network: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Declaration {
    read: Vec<String>,
    write: Vec<String>,
    network: Vec<String>,
}

impl PluginAccess {
    pub(crate) fn resolve(
        value: Option<&serde_json::Value>,
        directory: &Path,
    ) -> Result<Self, String> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let declaration: Declaration = serde_json::from_value(value.clone()).map_err(|_| {
            "plugin permissions must declare read/write path lists and network HTTP(S) origins"
                .to_owned()
        })?;
        if declaration.read.len() + declaration.write.len() > 64 || declaration.network.len() > 64 {
            return Err(
                "plugin permissions exceed 64 filesystem roots or 64 network origins".into(),
            );
        }
        let mut network = Vec::new();
        for raw in declaration.network {
            let url = parse_url(&raw)?;
            if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
                return Err("plugin network permissions require exact HTTP(S) origins without paths, queries or fragments".into());
            }
            network.push(url.origin().ascii_serialization());
        }
        network.sort();
        network.dedup();
        Ok(Self {
            read: resolve_roots(declaration.read, directory)?,
            write: resolve_roots(declaration.write, directory)?,
            network,
        })
    }

    pub fn readable_roots(&self) -> &[PathBuf] {
        &self.read
    }
    pub fn writable_roots(&self) -> &[PathBuf] {
        &self.write
    }

    pub(crate) fn admit_url(&self, raw: &str) -> Result<url::Url, String> {
        let url = parse_url(raw)?;
        let origin = url.origin().ascii_serialization();
        if !self.network.contains(&origin) {
            return Err(format!("plugin_network_denied: add {origin} to this plugin's permissions.network in user-global or launch-explicit configuration"));
        }
        Ok(url)
    }
}

fn resolve_roots(paths: Vec<String>, directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    for raw in paths {
        if raw.is_empty() || raw.len() > 4096 || raw.contains(['\0', '*']) {
            return Err("plugin permission path is empty, wildcarded or oversized".to_owned());
        }
        let path = Path::new(&raw);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            directory.join(path)
        };
        let path = std::fs::canonicalize(path).map_err(|_| "plugin permission path is unavailable; create the intended directory before declaring it".to_owned())?;
        if path.parent().is_none() {
            return Err("plugin permission cannot grant a filesystem root; declare the specific workspace directory".to_owned());
        }
        roots.push(path);
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

fn parse_url(raw: &str) -> Result<url::Url, String> {
    if raw.len() > 8192 || raw.chars().any(char::is_control) {
        return Err("plugin_network_invalid: URL exceeds its bound or contains controls".into());
    }
    let url = url::Url::parse(raw)
        .map_err(|_| "plugin_network_invalid: use an absolute HTTP(S) URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_some_and(|host| host.contains('*'))
    {
        return Err("plugin_network_invalid: use a bounded HTTP(S) URL without credentials or wildcard hosts".into());
    }
    Ok(url)
}

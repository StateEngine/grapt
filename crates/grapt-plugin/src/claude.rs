//! Claude plugin packages are data; the product retains activation and effects.
//! Manifest path forms.
//! Copyright 2023-2026 SpaceXAI.
//! Apache-2.0 (licenses/xai-grok-build/). Modified for strict slice capabilities,
//! bounded input, namespaced identities and explicit failures.
mod manifest;
mod marketplace;
pub use grapt_plugin_prompt as prompt;
mod source;
pub use manifest::{CommandSource, HookSource, Manifest};
pub use marketplace::Marketplace;
pub use source::{GitSource, PluginSource};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Component, Path, PathBuf};

pub const DOCUMENT_BYTES: usize = 1024 * 1024;

/// One namespace segment; never a filesystem path or an unvalidated registry key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Name(String);
impl Name {
    pub fn new(value: &str) -> Result<Self, String> {
        if value.is_empty()
            || value.len() > 128
            || value.contains("..")
            || !value
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err("plugin or marketplace name must be a bounded identifier".into());
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Name {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}
impl From<Name> for String {
    fn from(value: Name) -> Self {
        value.0
    }
}

pub fn document(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > DOCUMENT_BYTES {
        return Err("Claude plugin JSON exceeds 1 MiB".into());
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "Claude plugin JSON is malformed")?;
    if !value.is_object() {
        return Err("Claude plugin JSON must be an object".into());
    }
    Ok(value)
}

pub fn relative(value: &str) -> Result<PathBuf, String> {
    let value = if matches!(value, "." | "./") {
        return Ok(PathBuf::new());
    } else {
        value
            .strip_prefix("./")
            .ok_or("plugin component path must start with ./")?
    };
    if value.len() > 1024 || value.contains('\\') || value.contains(':') {
        return Err("plugin component path is not a bounded relative path".into());
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("plugin component path escapes its tree".into());
    }
    Ok(path.to_path_buf())
}

fn fields(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "Claude plugin field {} is not supported yet",
            key.escape_debug()
        ));
    }
    Ok(())
}

/// A domain-separated digest of ordered paths, contents and owner executable bits.
pub fn tree_digest<'a>(files: impl Iterator<Item = (&'a str, &'a [u8], bool)>) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"secode.claude-plugin.tree.v1\0");
    for (path, bytes, executable) in files {
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
        digest.update([u8::from(executable)]);
    }
    format!("{:x}", digest.finalize())
}

/// A marketplace-qualified installed identity. Equal names in two catalogs stay distinct.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PluginId {
    name: Name,
    marketplace: Name,
}
impl PluginId {
    pub fn new(value: &str) -> Result<Self, String> {
        let (name, marketplace) = value
            .split_once('@')
            .ok_or("plugin requires name@marketplace")?;
        Ok(Self {
            name: Name::new(name)?,
            marketplace: Name::new(marketplace)?,
        })
    }
    pub fn name(&self) -> &Name {
        &self.name
    }
    pub fn marketplace(&self) -> &Name {
        &self.marketplace
    }
}
impl std::fmt::Display for PluginId {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            output,
            "{}@{}",
            self.name.as_str(),
            self.marketplace.as_str()
        )
    }
}
impl TryFrom<String> for PluginId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        Self::new(&value)
    }
}
impl From<PluginId> for String {
    fn from(value: PluginId) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marketplace_identity_and_manifest_capabilities_stay_distinct() -> Result<(), String> {
        let market=Marketplace::parse(br#"{"name":"team","owner":{"name":"team"},"plugins":[{"name":"listed","source":"./tools"},{"description":"incomplete"}]}"#)?;
        assert_eq!(market.refused.len(), 1);
        let entry = market
            .entries
            .get(&Name::new("listed")?)
            .ok_or("entry missing")?;
        let manifest = Manifest::parse(
            Some(br#"{"name":"runtime-name","commands":{"hello":{"content":"Hello"}}}"#),
            entry,
        )?;
        assert_eq!(manifest.name.as_str(), "runtime-name");
        assert!(PluginId::new("listed@team").is_ok());
        assert!(PluginId::new("listed@team@other").is_err());
        assert!(Manifest::parse(Some(br#"{"name":"x","userConfig":{}}"#), entry).is_err());
        for path in [
            "../outside",
            "./../outside",
            "/absolute",
            "./C:/outside",
            "./nested\\outside",
        ] {
            assert!(relative(path).is_err());
        }
        Ok(())
    }
}

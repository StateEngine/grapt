//! Captured agent files feed the existing role catalog, never a second runtime.
use super::super::agent_definitions::{claude_plugin, Definition};
use super::*;
use std::collections::BTreeSet;

pub(in crate::dogfood) struct Layer {
    pub definitions: BTreeMap<String, (Definition, Vec<String>)>,
    pub root: PathBuf,
}

fn definitions(
    snapshot: &Snapshot,
    manifest: &Manifest,
    aliases: &BTreeMap<String, String>,
    paths: Option<&Value>,
) -> Result<BTreeMap<String, (Definition, Vec<String>)>, String> {
    let paths_to_read: BTreeSet<_> = match &manifest.agents {
        Some(paths) => paths
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect(),
        None => snapshot
            .files
            .keys()
            .filter(|path| path.starts_with("agents/") && path.ends_with(".md"))
            .cloned()
            .collect(),
    };
    if paths_to_read.len() > 64 {
        return Err("plugin exceeds 64 agents".into());
    }
    let mut definitions = BTreeMap::new();
    for path in paths_to_read {
        let bytes = snapshot
            .bytes(&path)
            .ok_or("declared plugin agent is not a file")?;
        if bytes.len() > 256 * 1024 {
            return Err("plugin agent Markdown exceeds 256 KiB".into());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| "plugin agent Markdown is not UTF-8")?;
        let document = super::super::catalog_markdown::parse(text)?;
        components::validate_body(&document.content)?;
        let body = match paths {
            Some(paths) => components::expand(&document.content, paths)?,
            None => document.content,
        };
        let (mut definition, notes) = claude_plugin::parse(
            &document.metadata,
            &body,
            Path::new(&path),
            manifest.name.as_str(),
            aliases,
        )?;
        if manifest.agents.is_none() {
            if let Some((folders, _)) = path
                .strip_prefix("agents/")
                .and_then(|path| path.rsplit_once('/'))
            {
                let leaf = definition
                    .name
                    .strip_prefix(&format!("{}:", manifest.name.as_str()))
                    .ok_or("plugin agent namespace is missing")?;
                definition.name = format!(
                    "{}:{}:{leaf}",
                    manifest.name.as_str(),
                    folders.replace('/', ":")
                );
                if !super::super::agent_definitions::valid_name(&definition.name) {
                    return Err(
                        "plugin agent scoped name exceeds its bounded identifier contract".into(),
                    );
                }
            }
        }
        if definitions
            .insert(definition.name.clone(), (definition, notes))
            .is_some()
        {
            return Err("plugin contains duplicate agent names".into());
        }
    }
    Ok(definitions)
}

pub(super) fn validate(snapshot: &Snapshot, manifest: &Manifest) -> Result<(), String> {
    definitions(snapshot, manifest, &BTreeMap::new(), None).map(|_| ())
}

pub(in crate::dogfood) fn load(
    sources: &CatalogSources,
    aliases: &BTreeMap<String, String>,
) -> Result<Vec<Layer>, String> {
    let project =
        super::super::locations::repository_root(&sources.directory)?.unwrap_or(&sources.directory);
    let mut layers = Vec::new();
    for plugin in loaded(sources)? {
        let paths = components::paths(&plugin, project);
        let definitions = definitions(&plugin.snapshot, &plugin.manifest, aliases, Some(&paths))?;
        if !definitions.is_empty() {
            layers.push(Layer {
                definitions,
                root: plugin.installation.root,
            });
        }
    }
    Ok(layers)
}

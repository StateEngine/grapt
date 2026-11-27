//! Captured plugin declarations enter the ordinary user-owned hook planner.
use super::*;
use crate::dogfood::catalog_config::CatalogDocument;
use crate::dogfood::hooks::plugin::Context;
use grapt_plugin::claude::HookSource;
use std::collections::BTreeSet;

pub(super) fn documents(
    snapshot: &Snapshot,
    manifest: &Manifest,
) -> Result<Vec<(String, Value)>, String> {
    let mut documents = Vec::new();
    let mut paths = BTreeSet::new();
    for (index, source) in manifest.hooks.iter().enumerate() {
        let (name, value) = match source {
            HookSource::Path(path) => {
                let key = path.to_string_lossy().replace('\\', "/");
                if !paths.insert(key.clone()) {
                    continue;
                }
                let Some(bytes) = snapshot.bytes(&key) else {
                    // Only the implicit conventional source is optional.
                    if index == 0 {
                        continue;
                    }
                    return Err("declared plugin hooks file is absent".into());
                };
                let mut value = grapt_plugin::claude::document(bytes)?;
                let object = value
                    .as_object_mut()
                    .ok_or("plugin hook file must be an object")?;
                if object
                    .keys()
                    .any(|key| !["hooks", "description", "$schema"].contains(&key.as_str()))
                {
                    return Err("plugin hook file contains an unsupported field".into());
                }
                let hooks = object
                    .remove("hooks")
                    .ok_or("plugin hook file requires a hooks wrapper")?;
                (key, hooks)
            }
            HookSource::Inline(value) => (
                format!(".claude-plugin/plugin.json#hooks-{index}"),
                value.clone(),
            ),
        };
        if !value.is_object() {
            return Err("plugin hooks must be an event object".into());
        }
        for (event, groups) in value
            .as_object()
            .ok_or("plugin hooks must be an event object")?
        {
            if ["PreToolUse", "PostToolUse", "PermissionRequest"].contains(&event.as_str()) {
                return Err(
                    "plugin tool hooks require a Claude tool payload adapter in a later slice"
                        .into(),
                );
            }
            if groups.as_array().is_some_and(|groups| {
                groups.iter().any(|group| {
                    group
                        .get("matcher")
                        .is_some_and(|matcher| matcher.as_str() != Some(""))
                })
            }) {
                return Err("plugin lifecycle matchers are not supported yet".into());
            }
        }
        documents.push((name, value));
    }
    Ok(documents)
}

pub(in crate::dogfood) fn load(
    sources: &CatalogSources,
) -> Result<Vec<(CatalogDocument, Context)>, String> {
    let project =
        super::super::locations::repository_root(&sources.directory)?.unwrap_or(&sources.directory);
    let mut result = Vec::new();
    for plugin in loaded(sources)? {
        for (name, hooks) in documents(&plugin.snapshot, &plugin.manifest)? {
            let bytes = serde_json::to_vec(&serde_json::json!({"hooks":hooks}))
                .map_err(|_| "plugin hook configuration cannot be encoded")?;
            let value = grapt_config::decode(&bytes, super::super::catalog_config::bounds()?)
                .map_err(|_| "plugin hook configuration exceeds the catalog bounds")?
                .into_value();
            result.push((
                CatalogDocument {
                    directory: project.to_path_buf(),
                    source: plugin
                        .installation
                        .root
                        .join(name)
                        .to_string_lossy()
                        .into_owned(),
                    origin: grapt_plugin::PluginOrigin::UserGlobal,
                    value,
                },
                Context {
                    root: plugin.installation.root.clone(),
                    data: plugin.data.clone(),
                    project: project.to_path_buf(),
                },
            ));
        }
    }
    Ok(result)
}

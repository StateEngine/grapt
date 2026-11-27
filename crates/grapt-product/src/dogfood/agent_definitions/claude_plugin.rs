//! Installed Claude agents adapt into the ordinary role and tool ceiling.
use super::{claude, Definition};
use std::{collections::BTreeMap, path::Path};
use yaml_rust2::Yaml;

pub(in crate::dogfood) fn parse(
    metadata: &Yaml,
    body: &str,
    path: &Path,
    namespace: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<(Definition, Vec<String>), String> {
    grapt_plugin::claude::Name::new(namespace)?;
    let fields = metadata
        .as_hash()
        .ok_or("plugin agent frontmatter must be an object")?;
    for key in fields.keys() {
        let key = key
            .as_str()
            .ok_or("plugin agent frontmatter keys must be strings")?;
        if ![
            "name",
            "description",
            "model",
            "tools",
            "disallowedTools",
            "color",
            // These fields are explicitly inactive for plugin agents upstream.
            "hooks",
            "mcpServers",
            "permissionMode",
            "initialPrompt",
        ]
        .contains(&key)
        {
            return Err(format!(
                "Claude plugin agent field {} is not supported yet",
                key.escape_debug()
            ));
        }
    }
    let mut translated = metadata.clone();
    let map = translated
        .as_mut_hash()
        .ok_or("plugin agent frontmatter must be an object")?;
    let offered = claude::known_tool_names();
    for field in ["tools", "disallowedTools"] {
        let key = Yaml::String(field.into());
        if let Some(value) = fields.get(&key).filter(|value| !value.is_null()) {
            let names = claude::tool_names(value)?;
            if names.len() > 256 || names.iter().any(|name| name.len() > 256) {
                return Err("plugin agent tools exceed 256 bounded names".into());
            }
            let mut converted = std::collections::BTreeSet::new();
            for name in names {
                let matched: Vec<_> = offered
                    .iter()
                    .filter(|native| {
                        *native == &name
                            || crate::dogfood::hooks::claude_tool_aliases(native)
                                .contains(&name.as_str())
                    })
                    .cloned()
                    .collect();
                if matched.is_empty() {
                    return Err(format!(
                        "Claude plugin agent tool {} is not supported yet",
                        name.escape_debug()
                    ));
                } else {
                    converted.extend(matched);
                }
            }
            map.insert(
                key,
                Yaml::Array(converted.into_iter().map(Yaml::String).collect()),
            );
        }
    }
    let (mut definition, notes) = claude::map(&translated, body, path, aliases)?;
    if !super::plain_name(&definition.name) {
        return Err(
            "plugin agent name must use 1..64 lowercase letters, digits, '_' or '-'".into(),
        );
    }
    definition.name = format!("{namespace}:{}", definition.name);
    definition.color = fields
        .get(&Yaml::String("color".into()))
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or("plugin agent color must be a string")
        })
        .transpose()?;
    definition.validate()?;
    Ok((definition, notes))
}

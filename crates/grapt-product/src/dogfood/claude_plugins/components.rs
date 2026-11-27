//! Interpret only the accepted prompt capability set from captured package bytes.
use super::*;
use crate::dogfood::legacy_names::{LEGACY_SKILL_AUTOINVOKE, LEGACY_SKILL_SLASH};
use grapt_plugin::claude::CommandSource;
use serde_json::json;
use yaml_rust2::Yaml;

struct Prompt {
    name: String,
    path: String,
    body: String,
    description: String,
    hint: String,
    slash: bool,
    autoinvoke: bool,
}
pub(super) fn validate(snapshot: &Snapshot, entry: &Value) -> Result<Manifest, String> {
    for path in snapshot.files.keys().chain(snapshot.directories.iter()) {
        let first = path.split('/').next().unwrap_or("");
        if ["output-styles", "themes", "monitors", "bin", "workflows"].contains(&first)
            || ["settings.json", ".mcp.json", ".lsp.json"].contains(&path.as_str())
        {
            return Err(format!(
                "Claude plugin component {} is not supported in this slice",
                first.escape_debug()
            ));
        }
    }
    let manifest = Manifest::parse(snapshot.bytes(".claude-plugin/plugin.json"), entry)?;
    for path in &manifest.required_paths {
        if !snapshot.contains(path) {
            return Err("declared plugin component path does not exist".into());
        }
    }
    let _ = super::hooks::documents(snapshot, &manifest)?;
    let _ = skill_prompts(snapshot, &manifest)?;
    let _ = command_prompts(snapshot, &manifest)?;
    super::agents::validate(snapshot, &manifest)?;
    Ok(manifest)
}
fn markdown(snapshot: &Snapshot, path: &str, name: String) -> Result<Prompt, String> {
    let bytes = snapshot
        .bytes(path)
        .ok_or("plugin Markdown source is missing")?;
    if bytes.len() > 256 * 1024 {
        return Err("plugin Markdown exceeds 256 KiB".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "plugin Markdown is not UTF-8")?;
    let document = super::super::catalog_markdown::parse(text)?;
    prompt(document.metadata, document.content, path, name)
}
fn prompt(metadata: Yaml, body: String, path: &str, name: String) -> Result<Prompt, String> {
    if body.len() > 65536 {
        return Err("plugin prompt exceeds 64 KiB".into());
    }
    let fields = metadata
        .as_hash()
        .ok_or("plugin frontmatter must be an object")?;
    for key in fields.keys() {
        let key = key
            .as_str()
            .ok_or("plugin frontmatter key must be a string")?;
        if ![
            "name",
            "description",
            "license",
            "argument-hint",
            "user-invocable",
            "disable-model-invocation",
            "metadata",
        ]
        .contains(&key)
        {
            return Err(format!(
                "Claude plugin frontmatter {} is not supported yet",
                key.escape_debug()
            ));
        }
    }
    validate_body(&body)?;
    let field = |name| super::super::catalogs::yaml_field(&metadata, name);
    let text = |name| {
        field(name)
            .map(|v| {
                v.as_str()
                    .ok_or("plugin frontmatter text has the wrong type")
            })
            .transpose()
    };
    let boolean = |name, default| {
        field(name)
            .map(|v| v.as_bool().ok_or("plugin invocation flag must be boolean"))
            .transpose()
            .map(|v| v.unwrap_or(default))
    };
    let description = text("description")?.unwrap_or("").to_owned();
    let hint = text("argument-hint")?.unwrap_or("").to_owned();
    let _ = text("license")?;
    if description.len() > 8192 || hint.len() > 8192 {
        return Err("plugin prompt metadata exceeds 8192 bytes".into());
    }
    let mut slash = true;
    let mut autoinvoke = true;
    if let Some(value) = field("metadata") {
        let map = value.as_hash().ok_or("skill metadata must be an object")?;
        // A skill written before the rename keeps its legacy key; the current
        // key wins when both are present.
        for (name, legacy, target) in [
            ("grapt/slash", LEGACY_SKILL_SLASH, &mut slash),
            ("grapt/autoinvoke", LEGACY_SKILL_AUTOINVOKE, &mut autoinvoke),
        ] {
            if let Some(value) = map
                .get(&Yaml::String(name.into()))
                .or_else(|| map.get(&Yaml::String(legacy.into())))
            {
                *target = match value {
                    Yaml::Boolean(v) => *v,
                    Yaml::String(v) if v == "true" => true,
                    Yaml::String(v) if v == "false" => false,
                    _ => return Err("plugin invocation metadata must be boolean".into()),
                };
            }
        }
    }
    slash = boolean("user-invocable", slash)?;
    autoinvoke = !boolean("disable-model-invocation", !autoinvoke)?;
    let name = text("name")?.unwrap_or(&name).to_owned();
    Name::new(&name)?;
    Ok(Prompt {
        name,
        path: path.to_owned(),
        body,
        description,
        hint,
        slash,
        autoinvoke,
    })
}
pub(super) fn validate_body(body: &str) -> Result<(), String> {
    if body.contains("!`")
        || body
            .lines()
            .any(|line| line.trim_start().starts_with("```!"))
    {
        return Err("plugin shell interpolation is not supported yet".into());
    }
    let mut rest = body;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            if after.starts_with("CLAUDE_") || after.starts_with("user_config.") {
                return Err("plugin body has an unclosed runtime variable".into());
            }
            break;
        };
        let key = &after[..end];
        if (key.starts_with("CLAUDE_") || key.starts_with("user_config."))
            && ![
                "CLAUDE_PLUGIN_ROOT",
                "CLAUDE_PLUGIN_DATA",
                "CLAUDE_PROJECT_DIR",
            ]
            .contains(&key)
        {
            return Err("plugin body requires an unsupported runtime variable".into());
        }
        rest = &after[end + 1..];
    }
    Ok(())
}
fn skill_prompts(snapshot: &Snapshot, manifest: &Manifest) -> Result<Vec<Prompt>, String> {
    let mut result = BTreeMap::new();
    let mut selected = std::collections::BTreeSet::new();
    for root in &manifest.skills {
        let prefix = root.to_string_lossy().replace('\\', "/");
        for path in snapshot.files.keys() {
            if (prefix.is_empty() || path.starts_with(&format!("{prefix}/")))
                && path.ends_with("/SKILL.md")
            {
                selected.insert(path.clone());
            }
            if prefix.is_empty() && path == "SKILL.md" {
                selected.insert(path.clone());
            }
        }
    }
    if manifest.root_skill
        && !snapshot.directories.contains("skills")
        && snapshot.bytes("SKILL.md").is_some()
    {
        selected.insert("SKILL.md".into());
    }
    for path in selected {
        let fallback = Path::new(&path)
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            .unwrap_or(manifest.name.as_str());
        let prompt = markdown(snapshot, &path, fallback.to_owned())?;
        if result.insert(prompt.name.clone(), prompt).is_some() {
            return Err("plugin contains duplicate skill names".into());
        }
    }
    if result.len() > 128 {
        return Err("plugin exceeds 128 skills".into());
    }
    Ok(result.into_values().collect())
}
fn command_prompts(snapshot: &Snapshot, manifest: &Manifest) -> Result<Vec<Prompt>, String> {
    let mut result = BTreeMap::new();
    for source in &manifest.commands {
        let prompts = match source {
            CommandSource::Path(path) => {
                let prefix = path.to_string_lossy().replace('\\', "/");
                let mut prompts = Vec::new();
                for key in snapshot.files.keys() {
                    if key == &prefix || key.starts_with(&format!("{prefix}/")) {
                        if !key.ends_with(".md") {
                            continue;
                        }
                        let relative = if key == &prefix {
                            Path::new(key)
                                .file_stem()
                                .and_then(|v| v.to_str())
                                .ok_or("command has no name")?
                        } else {
                            key.strip_prefix(&format!("{prefix}/"))
                                .ok_or("command path escaped its root")?
                                .strip_suffix(".md")
                                .ok_or("command is not Markdown")?
                        };
                        let mut value = markdown(snapshot, key, "command".into())?;
                        // Command paths determine identity; a skill-style name is presentation only.
                        value.name = relative.replace('/', ":");
                        prompts.push(value);
                    }
                }
                prompts
            }
            CommandSource::Inline {
                name,
                content,
                description,
                hint,
            } => {
                let mut value = prompt(
                    Yaml::Hash(Default::default()),
                    content.clone(),
                    ".claude-plugin/plugin.json",
                    name.as_str().to_owned(),
                )?;
                value.description = description.clone().unwrap_or_default();
                value.hint = hint.clone().unwrap_or_default();
                vec![value]
            }
            CommandSource::File {
                name,
                path,
                description,
                hint,
            } => {
                let mut value = markdown(
                    snapshot,
                    &path.to_string_lossy().replace('\\', "/"),
                    name.as_str().to_owned(),
                )?;
                value.name = name.as_str().to_owned();
                if let Some(description) = description {
                    value.description = description.clone();
                }
                if let Some(hint) = hint {
                    value.hint = hint.clone();
                }
                vec![value]
            }
        };
        for prompt in prompts {
            if result.insert(prompt.name.clone(), prompt).is_some() {
                return Err("plugin contains duplicate command names".into());
            }
        }
        if result.len() > 128 {
            return Err("plugin exceeds 128 commands".into());
        }
    }
    Ok(result.into_values().collect())
}
pub(super) fn paths(plugin: &Loaded, project: &Path) -> Value {
    let text = |path: &Path| path.to_string_lossy().replace('\\', "/");
    json!({"root":text(&plugin.installation.root),"data":text(&plugin.data),"project":text(project)})
}
pub(super) fn expand(body: &str, paths: &Value) -> Result<String, String> {
    let mut rest = body;
    let mut output = String::new();
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            output.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let key = match &after[..end] {
            "CLAUDE_PLUGIN_ROOT" => Some("root"),
            "CLAUDE_PLUGIN_DATA" => Some("data"),
            "CLAUDE_PROJECT_DIR" => Some("project"),
            _ => None,
        };
        if let Some(key) = key {
            output.push_str(
                paths[key]
                    .as_str()
                    .ok_or("plugin path context is malformed")?,
            );
        } else {
            output.push_str(&rest[start..start + end + 3]);
        }
        rest = &after[end + 1..];
        if output.len() > 65536 {
            return Err("expanded plugin content exceeds 64 KiB".into());
        }
    }
    output.push_str(rest);
    if output.len() > 65536 {
        return Err("expanded plugin content exceeds 64 KiB".into());
    }
    Ok(output)
}
pub(in crate::dogfood) fn skills(sources: &CatalogSources) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    let project =
        super::super::locations::repository_root(&sources.directory)?.unwrap_or(&sources.directory);
    for plugin in loaded(sources)? {
        let paths = paths(&plugin, project);
        for prompt in skill_prompts(&plugin.snapshot, &plugin.manifest)? {
            let id = format!("{}:{}", plugin.manifest.name.as_str(), prompt.name);
            result.push(json!({"type":"embedded","claude":true,"skill":{"id":id,"name":id,"description":prompt.description,"slash":prompt.slash,"autoinvoke":prompt.autoinvoke,"location":plugin.installation.root.join(&prompt.path),"content":expand(&prompt.body,&paths)?}}));
        }
    }
    Ok(result)
}
pub(in crate::dogfood) fn commands(sources: &CatalogSources) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    let project =
        super::super::locations::repository_root(&sources.directory)?.unwrap_or(&sources.directory);
    for plugin in loaded(sources)? {
        let paths = paths(&plugin, project);
        let mut prompts = BTreeMap::new();
        for prompt in command_prompts(&plugin.snapshot, &plugin.manifest)? {
            if prompt.slash {
                prompts.insert(prompt.name.clone(), prompt);
            }
        }
        for prompt in skill_prompts(&plugin.snapshot, &plugin.manifest)? {
            if prompt.slash {
                prompts.entry(prompt.name.clone()).or_insert(prompt);
            }
        }
        for prompt in prompts.into_values() {
            let name = format!("{}:{}", plugin.manifest.name.as_str(), prompt.name);
            let description = format!("{} {}", prompt.description, prompt.hint);
            let description = description.trim();
            if name.len() > 512
                || name.chars().any(|ch| ch.is_whitespace() || ch.is_control())
                || name.split(':').any(str::is_empty)
                || description.len() > 8192
            {
                return Err(
                    "plugin command name or description exceeds the command catalog contract"
                        .into(),
                );
            }
            result.push(json!({"name":name,"template":prompt.body,"description":description,"_origin":"user","_source":plugin.installation.root.join(&prompt.path),"_claude":paths}));
        }
    }
    Ok(result)
}

pub(in crate::dogfood) fn activate_skill(
    service: &Path,
    directory: &Path,
    id: &str,
    arguments: &str,
) -> Result<Option<String>, String> {
    if !id.contains(':') {
        return Ok(None);
    }
    let sources = CatalogSources::read(directory, CatalogEnvironment::read(service)?)?;
    activate_skill_from_sources(&sources, id, arguments)
}
pub(super) fn activate_skill_from_sources(
    sources: &CatalogSources,
    id: &str,
    arguments: &str,
) -> Result<Option<String>, String> {
    let project =
        super::super::locations::repository_root(&sources.directory)?.unwrap_or(&sources.directory);
    for plugin in loaded(sources)? {
        if !id.starts_with(&format!("{}:", plugin.manifest.name.as_str())) {
            continue;
        }
        for prompt in skill_prompts(&plugin.snapshot, &plugin.manifest)? {
            if id != format!("{}:{}", plugin.manifest.name.as_str(), prompt.name) {
                continue;
            }
            if !prompt.autoinvoke {
                return Err(
                    "plugin skill requires manual invocation through its slash command".into(),
                );
            }
            let paths = paths(&plugin, project);
            let context = grapt_plugin::claude::prompt::Paths {
                root: paths["root"].as_str().ok_or("plugin root absent")?,
                data: paths["data"].as_str().ok_or("plugin data path absent")?,
                project: paths["project"]
                    .as_str()
                    .ok_or("plugin project path absent")?,
            };
            return grapt_plugin::claude::prompt::expand(&prompt.body, arguments, &context)
                .map(Some);
        }
    }
    Ok(None)
}

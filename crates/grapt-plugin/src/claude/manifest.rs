use super::*;

const METADATA: &[&str] = &[
    "name",
    "displayName",
    "version",
    "description",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
    "metadata",
    "$schema",
    "commands",
    "skills",
    "agents",
    "hooks",
    "defaultEnabled",
];

#[derive(Debug, Clone)]
pub enum CommandSource {
    Path(PathBuf),
    Inline {
        name: Name,
        content: String,
        description: Option<String>,
        hint: Option<String>,
    },
    File {
        name: Name,
        path: PathBuf,
        description: Option<String>,
        hint: Option<String>,
    },
}
#[derive(Debug, Clone)]
pub enum HookSource {
    Path(PathBuf),
    Inline(Value),
}
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: Name,
    pub commands: Vec<CommandSource>,
    pub skills: Vec<PathBuf>,
    pub agents: Option<Vec<PathBuf>>,
    pub hooks: Vec<HookSource>,
    pub root_skill: bool,
    pub required_paths: Vec<PathBuf>,
}
impl Manifest {
    /// Capabilities are explicit. Unsupported declarations never become metadata.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn parse(bytes: Option<&[u8]>, entry: &Value) -> Result<Self, String> {
        let entry = entry.as_object().ok_or("plugin entry must be an object")?;
        let mut allowed = METADATA.to_vec();
        allowed.extend(["source", "strict", "category", "tags"]);
        fields(entry, &allowed)?;
        let own = bytes.map(document).transpose()?;
        let own = own.as_ref().and_then(Value::as_object);
        if let Some(own) = own {
            fields(own, METADATA)?;
        }
        let name = own
            .and_then(|m| m.get("name"))
            .or_else(|| entry.get("name"))
            .and_then(Value::as_str)
            .ok_or_else(|| "plugin requires name".to_owned())
            .and_then(Name::new)?;
        if entry.get("strict").is_some_and(|v| !v.is_boolean()) {
            return Err("marketplace strict must be boolean".into());
        }
        if own.is_some()
            && entry.get("strict") == Some(&Value::Bool(false))
            && ["commands", "skills", "agents", "hooks"]
                .iter()
                .any(|key| entry.contains_key(*key))
        {
            return Err("plugin has conflicting marketplace and plugin manifests".into());
        }
        let enabled = entry
            .get("defaultEnabled")
            .or_else(|| own.and_then(|m| m.get("defaultEnabled")));
        if enabled.is_some_and(|v| v != &Value::Bool(true)) {
            return Err(
                "disabled-by-default plugins require enable support in a later slice".into(),
            );
        }
        let mut commands = Vec::new();
        let mut hooks = vec![HookSource::Path("hooks/hooks.json".into())];
        let mut required_paths = Vec::new();
        let mut skills = vec![PathBuf::from("skills")];
        let mut agents = None::<Vec<PathBuf>>;
        let mut declared_commands = false;
        let mut declared_skills = false;
        let layers: Vec<_> = if let Some(own) = own {
            vec![own, entry]
        } else {
            vec![entry]
        };
        for layer in layers {
            if let Some(value) = layer.get("hooks") {
                let values = value
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(std::slice::from_ref(value));
                if values.len() > 128 {
                    return Err("plugin exceeds 128 hook sources".into());
                }
                for value in values {
                    if let Some(path) = value.as_str() {
                        let path = relative(path)?;
                        if path.extension().and_then(|v| v.to_str()) != Some("json") {
                            return Err("plugin hooks require JSON files".into());
                        }
                        required_paths.push(path.clone());
                        hooks.push(HookSource::Path(path));
                    } else if value.is_object() {
                        hooks.push(HookSource::Inline(value.clone()));
                    } else {
                        return Err("plugin hooks require JSON paths or event objects".into());
                    }
                }
                if hooks.len() > 129 {
                    return Err("plugin exceeds 128 declared hook sources".into());
                }
            }
            if let Some(value) = layer.get("commands") {
                declared_commands = true;
                let first = commands.len();
                command_sources(value, &mut commands)?;
                for command in &commands[first..] {
                    if let CommandSource::Path(path) | CommandSource::File { path, .. } = command {
                        required_paths.push(path.clone());
                    }
                }
            }
            if let Some(value) = layer.get("skills") {
                declared_skills = true;
                for path in paths(value)? {
                    let path = relative(path)?;
                    required_paths.push(path.clone());
                    skills.push(path);
                }
            }
            if let Some(value) = layer.get("agents") {
                for path in paths(value)? {
                    let path = relative(path)?;
                    if path.extension().and_then(|value| value.to_str()) != Some("md") {
                        return Err("declared plugin agents must name Markdown files".into());
                    }
                    required_paths.push(path.clone());
                    agents.get_or_insert_with(Vec::new).push(path);
                }
                agents.get_or_insert_with(Vec::new);
            }
        }
        if !declared_commands {
            commands.push(CommandSource::Path("commands".into()));
        }
        // Marketplace-root entries with explicit skill selections suppress the default.
        if matches!(
            entry.get("source").and_then(Value::as_str),
            Some("." | "./")
        ) && entry.contains_key("skills")
        {
            skills.remove(0);
        }
        skills.sort();
        skills.dedup();
        Ok(Self {
            name,
            commands,
            skills,
            agents,
            hooks,
            root_skill: !declared_skills,
            required_paths,
        })
    }
}
fn paths(value: &Value) -> Result<Vec<&str>, String> {
    if let Some(path) = value.as_str() {
        return Ok(vec![path]);
    }
    let values = value
        .as_array()
        .filter(|v| v.len() <= 128)
        .ok_or("component paths require a path or at most 128 paths")?;
    values
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| "component path must be a string".into())
        })
        .collect()
}
fn command_sources(value: &Value, output: &mut Vec<CommandSource>) -> Result<(), String> {
    if let Some(entries) = value.as_object() {
        if entries.len() > 128 {
            return Err("plugin exceeds 128 commands".into());
        }
        for (name, entry) in entries {
            let name = Name::new(name)?;
            let entry = entry
                .as_object()
                .ok_or("inline command must be an object")?;
            fields(entry, &["source", "content", "description", "argumentHint"])?;
            let description = text(entry, "description")?;
            let hint = text(entry, "argumentHint")?;
            output.push(match (text(entry, "source")?, text(entry, "content")?) {
                (Some(path), None) => CommandSource::File {
                    name,
                    path: relative(&path)?,
                    description,
                    hint,
                },
                (None, Some(content)) => CommandSource::Inline {
                    name,
                    content,
                    description,
                    hint,
                },
                _ => return Err("inline command requires exactly one source or content".into()),
            });
        }
    } else {
        for path in paths(value)? {
            output.push(CommandSource::Path(relative(path)?));
        }
    }
    if output.len() > 128 {
        return Err("plugin exceeds 128 command sources".into());
    }
    Ok(())
}
fn text(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    object
        .get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("command {key} must be a string"))
        })
        .transpose()
}

//! One command fold owns names, source provenance and trust projection.
use super::*;

// Built-ins retain their meaning on every frontend, including commands R handles locally.
const RESERVED: &[&str] = &[
    "init",
    "review",
    "help",
    "commands",
    "context",
    "status",
    "model",
    "effort",
    "permissions",
    "sandbox",
    "new",
    "clear",
    "resume",
    "rename",
    "undo",
    "rewind",
    "fork",
    "side",
    "quit",
    "exit",
    "diff",
    "worktree",
    "import",
    "mcp",
    "mcps",
    "compact",
    "reconcile",
    "plan",
    "config",
    "skills",
    "plugins",
    "plugin",
    "export",
    "copy",
    "terminal",
    "jobs",
    "theme",
];
fn safe_name(name: &str, origin: grapt_plugin::PluginOrigin) -> String {
    if RESERVED.contains(&name) || name.contains(':') {
        let prefix = if super::super::catalog_config::user_owned(origin) {
            "user"
        } else {
            "project"
        };
        format!("{prefix}:{name}")
    } else {
        name.to_owned()
    }
}

pub(crate) fn response<T: DeserializeOwned + Send + 'static>(
    application: super::super::ServerApplication,
    query: ProjectCurrentLocationQueryDto,
    inspection: bool,
) -> ApplicationFuture<T, InvalidRequestErrorDto> {
    Box::pin(async move {
        let selection = super::super::session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let location = &selection.location;
        let sources = &selection.sources;
        let roots = super::skill_sources(sources, Path::new(&location.directory))
            .map_err(invalid)?
            .iter()
            .filter_map(|source| source["path"].as_str().map(PathBuf::from))
            .collect();
        let watch_warning = application
            .skills_watch
            .track(&location.directory, roots)
            .err()
            .map(|error| {
                format!(
                    "Automatic skill refresh unavailable; /commands reloads: {}",
                    error.chars().take(512).collect::<String>()
                )
            });
        let originals = commands(sources, &location.project_directory).map_err(invalid)?;
        let plain: Vec<Value> = originals.iter().cloned().map(strip_source).collect();
        let transformed = application
            .plugins
            .transform_current("command", json!({"items":plain}))
            .await
            .map_err(invalid)?;
        let items = transformed
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("plugin command transform removed the item list"))?;
        // A transform cannot forge provenance or replace a built-in. Changed plugin
        // templates have no file-backed provenance and remain unavailable.
        let mut rows = Vec::new();
        for original in originals {
            let origin = original
                .get("_origin")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("command origin is absent"))?;
            let source = original
                .get("_source")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("command source is absent"))?;
            let command = strip_source(original.clone());
            let mut row = json!({"command":command,"origin":origin,"source":source});
            if let Some(paths) = original.get("_claude") {
                row["claude"] = paths.clone();
            }
            if origin != "builtin" && !items.iter().any(|item| item == &command) {
                row["unavailable"] =
                    json!("Plugin changed the template; file provenance cannot be verified");
            } else if origin == "project" {
                let repository =
                    locations::repository_root(Path::new(&location.directory)).map_err(invalid)?;
                let trusted = repository.as_ref().is_some_and(|root| {
                    grapt_platform::profile_configuration::repository_is_approved(
                        root,
                        Path::new(source),
                    )
                });
                if !trusted {
                    row["unavailable"] = json!("Repository is not trusted for its current profile contents; open it with an approved repository profile");
                }
            }
            if inspection {
                rows.push(row);
            } else {
                let mut command = command;
                if origin != "builtin" {
                    if let Some(fields) = command.as_object_mut() {
                        for name in ["agent", "model", "subtask"] {
                            fields.remove(name);
                        }
                    }
                    command["template"] = json!("");
                    command["description"] = json!("Unavailable here: use a client with command provenance preview and confirmation");
                }
                rows.push(command);
            }
        }
        let mut value = json!({"location":location.wire()});
        if inspection {
            let mut warnings = super::skill_source_warnings(
                &super::skill_sources(sources, Path::new(&location.directory)).map_err(invalid)?,
            )
            .map_err(invalid)?;
            warnings.extend(watch_warning);
            let warnings =
                super::super::catalog_mcp::append(&application, &selection, &mut rows, warnings)
                    .await;
            value["warnings"] = json!(warnings);
            value["mcp_scope"] = json!(super::super::catalog_mcp::scope(&selection.registry_key));
        }
        selection.validate_current(&application).map_err(invalid)?;
        value["data"] = json!(rows);
        decode(value).map_err(invalid)
    })
}
pub(super) fn strip_source(mut row: Value) -> Value {
    if let Some(object) = row.as_object_mut() {
        object.remove("_origin");
        object.remove("_source");
        object.remove("_claude");
    }
    row
}

pub(super) fn commands(sources: &CatalogSources, project: &str) -> Result<Vec<Value>, String> {
    let mut entries = BTreeMap::from([
        (
            "init".to_owned(),
            json!({"_origin":"builtin","_source":"Grapt built-in","name":"init", "description":"guided AGENTS.md setup", "template": include_str!("../../assets/commands/initialize.txt").replace("${path}", project)}),
        ),
        (
            "review".to_owned(),
            json!({"_origin":"builtin","_source":"Grapt built-in","name":"review", "description":"review changes [commit|branch|pr], defaults to uncommitted", "template": include_str!("../../assets/commands/review.txt").replace("${path}", project)}),
        ),
    ]);
    for source in &sources.command_sources {
        match source {
            CommandSource::Document(index) => {
                let doc = sources
                    .documents
                    .get(*index)
                    .ok_or_else(|| "catalog document index is invalid".to_owned())?;
                let Some(commands) = doc.value.get("commands") else {
                    continue;
                };
                let commands = commands
                    .value()
                    .as_object()
                    .ok_or_else(|| "configured commands must be an object".to_owned())?;
                for (name, entry) in commands {
                    merge_command(
                        &mut entries,
                        &safe_name(name.value().as_str(), doc.origin),
                        config_json(entry.value())?,
                        (doc.origin, &doc.source),
                    )?;
                }
            }
            CommandSource::Directory(directory) => {
                let mut files = Vec::new();
                for name in ["command", "commands"] {
                    files.extend(catalog_markdown::scan(&directory.join(name), false)?);
                }
                files.sort();
                for file in files {
                    let doc = catalog_markdown::read(&file)?;
                    let relative = file
                        .strip_prefix(directory)
                        .map_err(|_| "command escaped its source directory".to_owned())?;
                    let name = relative.components().skip(1).collect::<PathBuf>();
                    let name = name.with_extension("");
                    let name = name
                        .to_str()
                        .ok_or_else(|| "command name is not UTF-8".to_owned())?
                        .replace('\\', "/");
                    let mut value = yaml_json(&doc.metadata)?;
                    value
                        .as_object_mut()
                        .ok_or_else(|| "command frontmatter must be an object".to_owned())?
                        .insert(
                            "template".to_owned(),
                            Value::String(doc.content.trim().to_owned()),
                        );
                    let origin = super::super::catalog_config::source_origin(
                        &file,
                        grapt_plugin::PluginOrigin::UserGlobal,
                        Path::new(project),
                        sources.environment.service_root(),
                    )?;
                    merge_command(
                        &mut entries,
                        &safe_name(&name, origin),
                        value,
                        (origin, &file.to_string_lossy()),
                    )?;
                }
            }
        }
        check_catalog(&entries)?;
    }
    skill_commands(sources, project, &mut entries)?;
    for row in super::super::claude_plugins::commands(sources)? {
        let name = row["name"]
            .as_str()
            .ok_or("plugin command has no name")?
            .to_owned();
        if entries.insert(name, row).is_some() {
            return Err("plugin command namespace collides with an existing command".into());
        }
    }
    check_catalog(&entries)?;
    Ok(entries.into_values().collect())
}

// Copyright 2023-2026 SpaceXAI, Apache-2.0. Modified by Grapt.
// File-backed skill command identity and invocation flags.
// flags. Reuse the command fold so preview, trust and argument expansion agree.
fn skill_commands(
    sources: &CatalogSources,
    project: &str,
    entries: &mut BTreeMap<String, Value>,
) -> Result<(), String> {
    let source_values: Vec<_> = skill_sources(sources, &sources.directory)?
        .into_iter()
        .filter(|source| source.get("claude").is_none())
        .collect();
    for skill in skills_from_sources(&source_values, true)?.entries {
        if skill.get("slash").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let id = skill["id"]
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id
                        .chars()
                        .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.'))
            })
            .ok_or_else(|| "skill slash name must be a directory slug".to_owned())?;
        let source = skill["location"]
            .as_str()
            .ok_or_else(|| "skill command has no file source".to_owned())?;
        let origin = super::super::catalog_config::source_origin(
            Path::new(source),
            grapt_plugin::PluginOrigin::UserGlobal,
            Path::new(project),
            sources.environment.service_root(),
        )?;
        let name = safe_name(id, origin);
        // Existing custom commands keep their meaning when names collide.
        if entries.contains_key(&name) {
            continue;
        }
        let content = skill["content"]
            .as_str()
            .ok_or_else(|| "skill command has no content".to_owned())?;
        let directory = Path::new(source)
            .parent()
            .ok_or_else(|| "skill command source has no directory".to_owned())?;
        // Metadata is not an argument template. A dollar in a directory name
        // must not be expanded by the command's single-pass argument expander.
        let directory = serde_json::to_string(&directory.to_string_lossy())
            .map_err(|error| error.to_string())?
            .replace('$', "\\u0024");
        // Hint decoration belongs to command presentation, not model discovery:
        // a hint alone must not make an undescribed skill model-invocable.
        let hint = skill
            .get("_argument_hint")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let description = skill
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(id);
        let description = format!("{description} {hint}");
        merge_command(
            entries,
            &name,
            json!({
                "description":description.trim(),
                "template":format!("Skill: {id}\nBase directory for relative files (JSON string): {directory}\n\n{content}"),
            }),
            (origin, source),
        )?;
    }
    Ok(())
}

fn merge_command(
    entries: &mut BTreeMap<String, Value>,
    name: &str,
    value: Value,
    source: (grapt_plugin::PluginOrigin, &str),
) -> Result<(), String> {
    if name.is_empty() || name.len() > 1024 {
        return Err("configured command name is invalid".to_owned());
    }
    let object = value
        .as_object()
        .ok_or_else(|| "configured command must be an object".to_owned())?;
    let template = object
        .get("template")
        .and_then(Value::as_str)
        .ok_or_else(|| "configured command template is absent or invalid".to_owned())?;
    let held = entries
        .entry(name.to_owned())
        .or_insert_with(|| json!({"name":name}));
    let held = held
        .as_object_mut()
        .ok_or_else(|| "held command is malformed".to_owned())?;
    held.insert("template".to_owned(), Value::String(template.to_owned()));
    for key in ["description", "agent", "subtask"] {
        if let Some(value) = object.get(key) {
            held.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(model) = object.get("model") {
        held.insert("model".to_owned(), model_ref(model)?);
    }
    let (origin, source) = source;
    held.insert(
        "_origin".into(),
        json!(if super::super::catalog_config::user_owned(origin) {
            "user"
        } else {
            "project"
        }),
    );
    held.insert("_source".into(), json!(source));

    check_catalog(entries)
}

pub(super) fn model_ref(value: &Value) -> Result<Value, String> {
    if let Some(raw) = value.as_str() {
        let (provider, selection) = raw
            .split_once('/')
            .ok_or_else(|| "command model must name provider/model".to_owned())?;
        let (model, variant) = selection
            .split_once('#')
            .map_or((selection, None), |(model, variant)| (model, Some(variant)));
        if provider.is_empty()
            || provider.contains('#')
            || model.is_empty()
            || variant.is_some_and(|value| value.is_empty() || value.contains('#'))
        {
            return Err("command model selection is malformed".to_owned());
        }
        let mut result = json!({"providerID":provider,"id":model});
        if let Some(variant) = variant {
            result["variant"] = json!(variant);
        }
        return Ok(result);
    }
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "command model identity is absent".to_owned())?;
    let provider = value
        .get("providerID")
        .and_then(Value::as_str)
        .ok_or_else(|| "command model provider is absent".to_owned())?;
    let mut result = json!({"providerID":provider,"id":model});
    if let Some(variant) = value.get("variant") {
        result["variant"] = variant.clone();
    }
    Ok(result)
}

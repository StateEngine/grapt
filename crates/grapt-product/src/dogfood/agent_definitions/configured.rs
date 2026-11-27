//! Ordered native Markdown, TOML and configuration-defined agent layers.
use super::*;

struct Patch {
    description: Option<String>,
    system: Option<String>,
    model: Option<String>,
    tools: Option<BTreeSet<String>>,
    color: Option<String>,
    mode: Option<Mode>,
    steps: Option<std::num::NonZeroU32>,
    disabled: Option<bool>,
    hidden: Option<bool>,
}

fn apply(name: &str, patch: Value, previous: Option<&Definition>) -> Result<Definition, String> {
    let Value::Object(mut fields) = patch else {
        return Err("agent definition must be an object".into());
    };
    for (field, value) in &fields {
        if !matches!(
            field.as_str(),
            "description"
                | "system"
                | "developer_instructions"
                | "model"
                | "tools"
                | "color"
                | "mode"
                | "steps"
                | "disabled"
                | "hidden"
        ) {
            return Err(format!("unknown agent field {field}"));
        }
        if value.is_null() {
            return Err(format!("agent {field} cannot be null; omit it to inherit"));
        }
    }
    if let Some(value) = fields.remove("developer_instructions") {
        if fields.insert("system".into(), value).is_some() {
            return Err(
                "agent system and developer_instructions name the same field; choose one".into(),
            );
        }
    }
    let patch = Patch {
        description: field(&mut fields, "description")?,
        system: field(&mut fields, "system")?,
        model: field(&mut fields, "model")?,
        tools: field(&mut fields, "tools")?,
        color: field(&mut fields, "color")?,
        mode: field(&mut fields, "mode")?,
        steps: field(&mut fields, "steps")?,
        disabled: field(&mut fields, "disabled")?,
        hidden: field(&mut fields, "hidden")?,
    };
    let mut definition = previous.cloned().unwrap_or_else(|| Definition {
        name: name.to_owned(),
        description: "Custom agent.".to_owned(),
        developer_instructions: "Carry out the assigned task.".to_owned(),
        model: None,
        tools: None,
        color: None,
        mode: Mode::All,
        steps: None,
        disabled: false,
        hidden: false,
    });
    if let Some(value) = patch.description {
        definition.description = value;
    }
    if let Some(value) = patch.system {
        definition.developer_instructions = value;
    }
    if let Some(value) = patch.model {
        definition.model = Some(value);
    }
    if let Some(value) = patch.tools {
        definition.tools = Some(value);
    }
    if let Some(value) = patch.color {
        definition.color = Some(value);
    }
    if let Some(value) = patch.mode {
        definition.mode = value;
    }
    if let Some(value) = patch.steps {
        definition.steps = Some(value);
    }
    if let Some(value) = patch.disabled {
        definition.disabled = value;
    }
    if let Some(value) = patch.hidden {
        definition.hidden = value;
    }
    definition.validate()?;
    Ok(definition)
}

fn field<T: serde::de::DeserializeOwned>(
    fields: &mut serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<T>, String> {
    fields
        .remove(name)
        .map(|value| {
            serde_json::from_value(value).map_err(|error| format!("agent {name}: {error}"))
        })
        .transpose()
}

pub(super) fn load(
    catalog: &mut Catalog,
    sources: &CatalogSources,
    capture: &mut files::Capture,
) -> Result<(), String> {
    let mut directories = 0;
    for source in &sources.command_sources {
        let (layer, origin, narrow) = match source {
            CommandSource::Directory(directory) => {
                let roots = [directory.join("agent"), directory.join("agents")];
                catalog.roots.extend(roots.iter().cloned());
                let narrow = directories > 0;
                let layer = native_layer(catalog, &roots, capture, narrow)?;
                directories += 1;
                (
                    layer,
                    if narrow {
                        Source::Project
                    } else {
                        Source::User
                    },
                    narrow,
                )
            }
            CommandSource::Document(index) => {
                let document = sources
                    .documents
                    .get(*index)
                    .ok_or("agent configuration document is absent")?;
                let mut layer = BTreeMap::new();
                if let Some(agents) = document.value.get("agents") {
                    let agents = agents
                        .value()
                        .as_object()
                        .ok_or_else(|| document.refusal("agents must be an object"))?;
                    for (name, value) in agents {
                        let name = name.value().as_str();
                        if name == "modelAliases" {
                            continue;
                        }
                        let patch = super::super::catalogs::config_json(value.value())?;
                        let definition = apply(name, patch, catalog.definitions.get(name))
                            .map_err(|reason| document.refusal(&reason))?;
                        layer.insert(name.to_owned(), (definition, Vec::new()));
                    }
                }
                let narrow = !super::super::catalog_config::user_owned(document.origin);
                (
                    layer,
                    if narrow {
                        Source::Project
                    } else {
                        Source::User
                    },
                    narrow,
                )
            }
        };
        Catalog::install(
            &mut catalog.definitions,
            &mut catalog.sources,
            &mut catalog.notes,
            layer,
            origin,
            narrow,
        )
        .map_err(|reason| match source {
            CommandSource::Directory(directory) => format!("{}: {reason}", directory.display()),
            CommandSource::Document(index) => sources.documents[*index].refusal(&reason),
        })?;
        if catalog.definitions.len() > MAX_AGENTS {
            return Err("agent catalog exceeds 64 definitions".into());
        }
    }
    Ok(())
}

fn read(
    path: &Path,
    catalog: &Catalog,
    total: &mut usize,
    narrow: bool,
) -> Result<Definition, String> {
    let mut capture = || -> Result<Definition, String> {
        let mut patch = if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            files::read_toml(path, total)?
        } else {
            let (metadata, body) = files::read_markdown(path, total)?;
            let mut patch = super::super::catalogs::yaml_json(&metadata)?;
            if patch.is_null() {
                patch = json!({});
            }
            let fields = patch
                .as_object_mut()
                .ok_or("agent frontmatter must be an object")?;
            if !body.trim().is_empty() {
                if fields.contains_key("system") || fields.contains_key("developer_instructions") {
                    return Err("agent instructions must use either the Markdown body or a frontmatter field".into());
                }
                fields.insert("system".to_owned(), json!(body));
            }
            patch
        };
        let fields = patch
            .as_object_mut()
            .ok_or("agent definition must be an object")?;
        let name = match fields.remove("name") {
            Some(Value::String(name)) => name,
            Some(_) => return Err("agent name must be a string".into()),
            None => path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or("agent filename must be Unicode")?
                .to_owned(),
        };
        let previous = catalog.definitions.get(&name);
        let definition = apply(&name, patch, previous)?;
        if let Some(previous) = previous.filter(|_| narrow) {
            previous.mode.narrow(definition.mode, &name)?;
        }
        Ok(definition)
    };
    capture().map_err(|reason| format!("{}: {reason}", path.display()))
}

pub(super) fn native_layer(
    catalog: &Catalog,
    roots: &[PathBuf],
    capture: &mut files::Capture,
    narrow: bool,
) -> Result<BTreeMap<String, (Definition, Vec<String>)>, String> {
    let mut layer = BTreeMap::new();
    for root in roots {
        for path in files::collect(
            root,
            &mut capture.entries,
            None,
            !narrow,
            &mut capture.aliases,
        )? {
            let definition = read(&path, catalog, &mut capture.bytes, narrow)?;
            if layer
                .insert(definition.name.clone(), (definition, Vec::new()))
                .is_some()
            {
                return Err(format!(
                    "{}: duplicate agent name in {}",
                    path.display(),
                    root.display()
                ));
            }
        }
    }
    Ok(layer)
}

//! Product composition of the retained command and skill catalogs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grapt_api::u3a::{ApplicationFuture, InvalidRequestErrorDto, ProjectCurrentLocationQueryDto};
use grapt_config::ConfigValue;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use yaml_rust2::Yaml;

use super::catalog_config::{CatalogEnvironment, CatalogSources, CommandSource};
use super::catalog_markdown;
use super::legacy_names::{LEGACY_SKILL_AUTOINVOKE, LEGACY_SKILL_SLASH};
use super::support::{decode, invalid};
use super::{locations, CatalogProjection};

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

/// Validate a candidate user configuration before making its prompt sources active.
pub(super) fn validate_prompt_catalog(sources: &CatalogSources) -> Result<(), String> {
    let _ = commands(sources, &sources.directory.to_string_lossy())?;
    let _ = skills(sources, &sources.directory)?;
    Ok(())
}

pub(super) enum Kind {
    Commands,
    Skills,
    References,
}

pub(super) fn response<T: DeserializeOwned + Send + 'static>(
    catalog: Arc<CatalogProjection>,
    query: ProjectCurrentLocationQueryDto,
    kind: Kind,
) -> ApplicationFuture<T, InvalidRequestErrorDto> {
    Box::pin(async move {
        let location = locations::resolve(&catalog, &query)
            .await
            .map_err(invalid)?;
        let sources = CatalogSources::read(
            Path::new(&location.directory),
            CatalogEnvironment::read(Path::new(&catalog.directory)).map_err(invalid)?,
        )
        .map_err(invalid)?;
        let mut warnings = Vec::new();
        let data = match kind {
            Kind::Commands => commands(&sources, &location.project_directory),
            Kind::Skills => {
                let catalog = skills_from_sources(
                    &skill_sources(&sources, Path::new(&location.directory)).map_err(invalid)?,
                    false,
                )
                .map_err(invalid)?;
                warnings = catalog.warnings;
                Ok(catalog.entries)
            }
            Kind::References => references(&sources),
        }
        .map_err(invalid)?;
        let mut value = json!({"location": location.wire(), "data": data});
        if !warnings.is_empty() {
            value["warnings"] = json!(warnings);
        }
        decode(value).map_err(invalid)
    })
}

pub(super) fn transformed_response<T: DeserializeOwned + Send + 'static>(
    catalog: Arc<CatalogProjection>,
    plugins: Arc<super::plugins::PluginRuntime>,
    query: ProjectCurrentLocationQueryDto,
    kind: Kind,
) -> ApplicationFuture<T, InvalidRequestErrorDto> {
    Box::pin(async move {
        let location = locations::resolve(&catalog, &query)
            .await
            .map_err(invalid)?;
        let sources = CatalogSources::read(
            Path::new(&location.directory),
            CatalogEnvironment::read(Path::new(&catalog.directory)).map_err(invalid)?,
        )
        .map_err(invalid)?;
        let mut warnings = Vec::new();
        let data = match kind {
            Kind::Skills => {
                let catalog = current_skills(&plugins, Path::new(&location.directory))
                    .await
                    .map_err(invalid)?;
                warnings = catalog.warnings;
                catalog.entries
            }
            Kind::References => {
                let items = reference_draft(&sources).map_err(invalid)?;
                let transformed = plugins
                    .transform_current("reference", json!({"items":items}))
                    .await
                    .map_err(invalid)?;
                let items = transformed["items"]
                    .as_array()
                    .ok_or_else(|| invalid("plugin reference transform removed the source list"))?;
                references_from_draft(&sources, items).map_err(invalid)?
            }
            Kind::Commands => {
                return Err(invalid(
                    "command transforms use the resolved command draft path",
                ))
            }
        };
        let mut value = json!({"location": location.wire(), "data": data});
        if !warnings.is_empty() {
            value["warnings"] = json!(warnings);
        }
        decode(value).map_err(invalid)
    })
}

/// The skill catalog for `directory`, after plugin transforms. The HTTP catalog,
/// the model's skill guidance and the skill tool all read this one list.
pub(super) async fn current_skills(
    plugins: &Arc<super::plugins::PluginRuntime>,
    directory: &Path,
) -> Result<SkillCatalog, String> {
    let sources =
        CatalogSources::read(directory, CatalogEnvironment::read(plugins.service_root())?)?;
    let source_values = skill_sources(&sources, directory)?;
    let transformed = plugins
        .transform_current("skill", json!({"sources":source_values}))
        .await?;
    let source_values = transformed["sources"]
        .as_array()
        .ok_or_else(|| "plugin skill transform removed the source list".to_owned())?;
    skills_from_sources(source_values, false)
}

/// What the model is told about a skill before it activates one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkillSummary {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) description: String,
}

/// Skills offered to the model: those with a description that have not opted
/// out of model invocation (`autoinvoke: false`), ordered by id. Content stays
/// in the catalog until the skill tool loads one deliberately.
pub(super) fn offered(skills: &[Value]) -> Vec<SkillSummary> {
    let mut offered: Vec<SkillSummary> = skills
        .iter()
        .filter(|skill| skill.get("autoinvoke").and_then(Value::as_bool) != Some(false))
        .filter_map(|skill| {
            let id = skill.get("id")?.as_str()?.to_owned();
            let name = skill
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_owned();
            let description = skill.get("description")?.as_str()?.trim().to_owned();
            (!description.is_empty()).then_some(SkillSummary {
                id,
                name,
                description,
            })
        })
        .collect();
    offered.sort_by(|left, right| left.id.cmp(&right.id));
    offered
}

#[path = "catalog_commands.rs"]
mod command;
#[cfg(test)]
use command::model_ref;
pub(super) use command::response as command_response;
fn commands(sources: &CatalogSources, project: &str) -> Result<Vec<Value>, String> {
    command::commands(sources, project)
        .map(|rows| rows.into_iter().map(command::strip_source).collect())
}
pub(super) fn command_draft(catalog: &CatalogProjection) -> Result<Value, String> {
    let root = Path::new(&catalog.directory);
    let sources = CatalogSources::read(root, CatalogEnvironment::read(root)?)?;
    Ok(json!({"items":commands(&sources, &catalog.directory)?}))
}

fn skill_sources(sources: &CatalogSources, directory: &Path) -> Result<Vec<Value>, String> {
    // Skill host roots; see THIRD_PARTY_NOTICES.md.
    // Project discovery stops at the repository; explicit user/config roots
    // remain available even when the selected workspace is beneath them.
    let repository = locations::repository_root(directory)?;
    let pinned = [
        sources.environment.home.join(".claude/skills"),
        sources.environment.home.join(".agents/skills"),
        sources.environment.config.join("skill"),
        sources.environment.config.join("skills"),
    ];
    let mut values = sources
        .skill_directories
        .iter()
        .filter(|path| {
            repository.is_none_or(|root| path.starts_with(root)) || pinned.contains(*path)
        })
        .map(|path| {
            path.to_str()
                .map(|path| json!({"type":"directory","path":path}))
                .ok_or_else(|| "skill source path is not UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    for doc in &sources.documents {
        if let Some(value) = doc.value.get("skills") {
            let paths = value
                .value()
                .as_array()
                .ok_or_else(|| "configured skills must be an array".to_owned())?;
            for path in paths {
                let path = path
                    .value()
                    .as_str()
                    .ok_or_else(|| "configured skill source is not a string".to_owned())?;
                if path.starts_with("http://") || path.starts_with("https://") {
                    values.push(json!({"type":"url","url":path}));
                } else {
                    let path = expand_path(directory, &sources.environment.home, path)?;
                    values.push(json!({
                        "type":"directory",
                        "path":path.to_str().ok_or_else(|| "skill source path is not UTF-8".to_owned())?,
                    }));
                }
            }
        }
    }
    values.extend(super::claude_plugins::skills(sources)?);
    if values.len() > 512 {
        return Err("skill source census exceeds 512 directories".to_owned());
    }
    Ok(values)
}

fn skills(sources: &CatalogSources, directory: &Path) -> Result<Vec<Value>, String> {
    let source_values = skill_sources(sources, directory)?;
    Ok(skills_from_sources(&source_values, false)?.entries)
}

pub(super) struct SkillCatalog {
    pub(super) entries: Vec<Value>,
    pub(super) warnings: Vec<String>,
}

fn skill_source_warnings(sources: &[Value]) -> Result<Vec<String>, String> {
    sources.iter().enumerate().filter(|(_, source)| source["type"] == "url")
        .map(|(index, source)| {
            let url = source.get("url").and_then(Value::as_str)
                .and_then(|value| url::Url::parse(value).ok())
                .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
                .ok_or("remote skill source URL is invalid")?;
            let origin = url.origin().ascii_serialization();
            if origin.len() > 512 {
                return Err("remote skill source origin exceeds 512 bytes".to_owned());
            }
            Ok(format!("Remote skill source {} ({origin}) is unavailable: downloading remote skills is not supported. Install the skill files in a local directory and replace this skills entry with that directory's path.", index + 1))
        }).collect()
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn skills_from_sources(
    source_values: &[Value],
    command_presentation: bool,
) -> Result<SkillCatalog, String> {
    if source_values.len() > 512 {
        return Err("skill source census exceeds 512 entries".to_owned());
    }
    let mut seen = BTreeSet::new();
    let mut entries = BTreeMap::new();
    let warnings = skill_source_warnings(source_values)?;
    for source in source_values {
        let kind = source
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "skill source type is absent".to_owned())?;
        if kind == "url" {
            continue;
        }
        if kind == "embedded" {
            let skill = source
                .get("skill")
                .cloned()
                .ok_or_else(|| "embedded skill source has no skill".to_owned())?;
            let id = skill
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| "embedded skill identity is invalid".to_owned())?;
            entries.insert(id.to_owned(), skill);
            check_catalog(&entries)?;
            continue;
        }
        if kind != "directory" {
            return Err(format!("skill source type {kind} is unsupported"));
        }
        let root = source
            .get("path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| "skill directory source must be an absolute path".to_owned())?;
        let identity = root.to_string_lossy().into_owned();
        if !seen.insert(identity) {
            continue;
        }
        for file in catalog_markdown::scan(&root, true)? {
            let doc = catalog_markdown::read(&file)?;
            let id = if file.parent() == Some(root.as_path()) {
                file.file_stem()
            } else {
                file.parent().and_then(Path::file_name)
            }
            .and_then(|name| name.to_str())
            .ok_or_else(|| "skill identity is invalid".to_owned())?;
            let mut info = json!({"id":id, "name":id, "location":file.to_str().ok_or_else(|| "skill path is not UTF-8".to_owned())?, "content":doc.content});
            for key in ["name", "description"] {
                if let Some(value) = yaml_field(&doc.metadata, key) {
                    info[key] = Value::String(
                        value
                            .as_str()
                            .ok_or_else(|| format!("skill {key} is not a string"))?
                            .to_owned(),
                    );
                }
            }
            if let Some(value) = yaml_field(&doc.metadata, "slash") {
                info["slash"] = Value::Bool(
                    value
                        .as_bool()
                        .ok_or_else(|| "skill slash is not a boolean".to_owned())?,
                );
            }
            if let Some(metadata) = yaml_field(&doc.metadata, "metadata") {
                // The current key wins; a skill written before the rename
                // keeps its legacy key.
                for (source, legacy, key) in [
                    ("grapt/slash", LEGACY_SKILL_SLASH, "slash"),
                    ("grapt/autoinvoke", LEGACY_SKILL_AUTOINVOKE, "autoinvoke"),
                ] {
                    if let Some(value) = yaml_field(metadata, source)
                        .or_else(|| yaml_field(metadata, legacy))
                        .and_then(metadata_boolean)
                    {
                        info[key] = Value::Bool(value);
                    }
                }
            }
            // Copyright 2023-2026 SpaceXAI, Apache-2.0. Modified by Grapt.
            // SkillInfo invocation fields; standard
            // frontmatter takes precedence over the legacy Grapt aliases.
            for (source, target, inverted) in [
                ("user-invocable", "slash", false),
                ("disable-model-invocation", "autoinvoke", true),
            ] {
                if let Some(value) = yaml_field(&doc.metadata, source) {
                    let value = value
                        .as_bool()
                        .ok_or_else(|| format!("skill {source} is not a boolean"))?;
                    info[target] = Value::Bool(value != inverted);
                }
            }
            if let Some(hint) = yaml_field(&doc.metadata, "argument-hint") {
                let hint = hint
                    .as_str()
                    .ok_or_else(|| "skill argument-hint is not a string".to_owned())?;
                // Presentation-only metadata never enters Skill.Info or offered().
                if command_presentation {
                    info["_argument_hint"] = json!(hint);
                }
            }
            entries.insert(id.to_owned(), info);
            check_catalog(&entries)?;
        }
    }
    Ok(SkillCatalog {
        entries: entries.into_values().collect(),
        warnings,
    })
}

fn reference_draft(sources: &CatalogSources) -> Result<Vec<Value>, String> {
    references(sources)?
        .into_iter()
        .map(|info| {
            let name = info
                .get("name")
                .cloned()
                .ok_or_else(|| "resolved reference name is absent".to_owned())?;
            let source = info
                .get("source")
                .cloned()
                .ok_or_else(|| "resolved reference source is absent".to_owned())?;
            Ok(json!([name, source]))
        })
        .collect()
}

fn references_from_draft(sources: &CatalogSources, items: &[Value]) -> Result<Vec<Value>, String> {
    let mut entries = BTreeMap::new();
    for item in items {
        let pair = item
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or_else(|| "reference transform entry must be a name/source pair".to_owned())?;
        let name = pair[0]
            .as_str()
            .filter(|name| {
                !name.is_empty()
                    && !name
                        .chars()
                        .any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '`' | ','))
            })
            .ok_or_else(|| "transformed reference name is invalid".to_owned())?;
        let source = pair[1]
            .as_object()
            .ok_or_else(|| "transformed reference source is invalid".to_owned())?;
        let kind = source
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "transformed reference source type is absent".to_owned())?;
        let path = match kind {
            "local" => source
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| "transformed local reference path must be absolute".to_owned())?,
            "git" => {
                let repository = source
                    .get("repository")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "transformed git reference repository is absent".to_owned())?;
                let branch = source
                    .get("branch")
                    .map(|value| {
                        value.as_str().ok_or_else(|| {
                            "transformed reference branch is not a string".to_owned()
                        })
                    })
                    .transpose()?;
                repository_path(&sources.environment.repositories, repository, branch)?
            }
            _ => {
                return Err(format!(
                    "transformed reference source type {kind} is unsupported"
                ))
            }
        };
        let mut info = json!({
            "name":name,
            "path":path.to_str().ok_or_else(|| "reference cache path is not UTF-8".to_owned())?,
            "source":pair[1],
        });
        for key in ["description", "hidden"] {
            if let Some(value) = source.get(key) {
                info[key] = value.clone();
            }
        }
        entries.insert(name.to_owned(), info);
        check_catalog(&entries)?;
    }
    Ok(entries.into_values().collect())
}

fn references(sources: &CatalogSources) -> Result<Vec<Value>, String> {
    let mut entries = BTreeMap::new();
    for doc in &sources.documents {
        let Some(references) = doc.value.get("references") else {
            continue;
        };
        let references = references
            .value()
            .as_object()
            .ok_or_else(|| "configured references must be an object".to_owned())?;
        for (name, entry) in references {
            let name = name.value().as_str();
            if name.is_empty()
                || name
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '`' | ','))
            {
                return Err("configured reference name is invalid".to_owned());
            }
            let entry = config_json(entry.value())?;
            let local = entry
                .as_str()
                .filter(|path| path.starts_with(['.', '/', '~']) || Path::new(path).is_absolute())
                .or_else(|| entry.get("path").and_then(Value::as_str));
            let (path, mut source) = if let Some(local) = local {
                let path = expand_path(&doc.directory, &sources.environment.home, local)?;
                (
                    path.clone(),
                    json!({"type":"local","path":path.to_str().ok_or_else(|| "reference path is not UTF-8".to_owned())?}),
                )
            } else {
                let repository = entry
                    .as_str()
                    .or_else(|| entry.get("repository").and_then(Value::as_str))
                    .ok_or_else(|| "reference has no local path or repository".to_owned())?;
                let branch = entry
                    .get("branch")
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| "reference branch is not a string".to_owned())
                    })
                    .transpose()?;
                let path = repository_path(&sources.environment.repositories, repository, branch)?;
                let mut source = json!({"type":"git","repository":repository});
                if let Some(branch) = branch {
                    source["branch"] = Value::String(branch.to_owned());
                }
                (path, source)
            };
            let mut info = json!({"name":name,"path":path.to_str().ok_or_else(|| "reference cache path is not UTF-8".to_owned())?});
            for key in ["description", "hidden"] {
                if let Some(value) = entry.get(key) {
                    info[key] = value.clone();
                    source[key] = value.clone();
                }
            }
            info["source"] = source;
            entries.insert(name.to_owned(), info);
            check_catalog(&entries)?;
        }
    }
    Ok(entries.into_values().collect())
}

fn repository_path(root: &Path, repository: &str, branch: Option<&str>) -> Result<PathBuf, String> {
    if repository.len() > 4096 {
        return Err("reference repository exceeds its bound".to_owned());
    }
    let repository = repository
        .trim()
        .strip_prefix("git+")
        .unwrap_or(repository.trim());
    let (host, path) = if repository.contains("://") {
        let url = url::Url::parse(repository)
            .map_err(|_| "reference repository URL is invalid".to_owned())?;
        if url.scheme() == "file" {
            return Err("file repository references are not remote sources".to_owned());
        }
        let host = url
            .host_str()
            .ok_or_else(|| "reference repository has no host".to_owned())?;
        let host = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        (host, url.path().to_owned())
    } else if let Some(path) = repository.strip_prefix("github:") {
        ("github.com".to_owned(), path.to_owned())
    } else if let Some((host, path)) = repository.split_once(':') {
        (
            host.rsplit('@').next().unwrap_or(host).to_owned(),
            path.to_owned(),
        )
    } else {
        let (first, rest) = repository
            .split_once('/')
            .ok_or_else(|| "reference repository must name a host/path or owner/repo".to_owned())?;
        if first.contains('.') || first == "localhost" {
            (first.to_owned(), rest.to_owned())
        } else {
            ("github.com".to_owned(), repository.to_owned())
        }
    };
    let mut result = root.to_path_buf();
    for segment in host.split(':').chain(path.trim_matches('/').split('/')) {
        let segment = segment.strip_suffix(".git").unwrap_or(segment);
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.starts_with('-')
            || segment
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '\\' | '\0' | '?' | '#' | ':' | '*'))
        {
            return Err("reference repository contains an unsafe cache path segment".to_owned());
        }
        result.push(segment);
    }
    if let Some(branch) = branch {
        if branch.is_empty()
            || branch.starts_with('-')
            || branch.contains("..")
            || !branch
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/_.-".contains(&c))
        {
            return Err("reference branch is invalid".to_owned());
        }
        let encoded: String = url::form_urlencoded::byte_serialize(branch.as_bytes()).collect();
        let mut path = result.into_os_string();
        path.push(format!("@{encoded}"));
        result = path.into();
    }
    if result.as_os_str().len() > 4096 {
        return Err("reference cache path exceeds its bound".to_owned());
    }
    Ok(result)
}

pub(super) fn yaml_field<'a>(value: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    value.as_hash()?.get(&Yaml::String(key.to_owned()))
}

fn metadata_boolean(value: &Yaml) -> Option<bool> {
    value.as_bool().or_else(
        || match value.as_str()?.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
    )
}

fn expand_path(directory: &Path, home: &Path, path: &str) -> Result<PathBuf, String> {
    if path.len() > 4096 || path.contains('\0') {
        return Err("catalog path exceeds its bound or contains NUL".to_owned());
    }
    let path = if let Some(relative) = path.strip_prefix("~/") {
        home.join(relative)
    } else {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            directory.join(path)
        }
    };
    if path.as_os_str().len() > 4096 {
        return Err("expanded catalog path exceeds its bound".to_owned());
    }
    Ok(path)
}

fn check_catalog(entries: &BTreeMap<String, Value>) -> Result<(), String> {
    if entries.len() > 256 {
        return Err("catalog exceeds 256 entries".to_owned());
    }
    let bytes = entries.values().try_fold(0usize, |sum, value| {
        serde_json::to_vec(value)
            .map(|bytes| sum + bytes.len())
            .map_err(|_| "catalog encoding failed".to_owned())
    })?;
    if bytes > 4 * 1024 * 1024 {
        return Err("catalog exceeds the four MiB aggregate bound".to_owned());
    }
    Ok(())
}

pub(super) fn config_json(value: &ConfigValue) -> Result<Value, String> {
    match value {
        ConfigValue::Null => Ok(Value::Null),
        ConfigValue::Bool(value) => Ok(Value::Bool(*value)),
        ConfigValue::String(value) => Ok(Value::String(value.clone())),
        ConfigValue::Number(value) => serde_json::from_str(value.literal())
            .map_err(|_| "catalog number is not JSON representable".to_owned()),
        ConfigValue::Array(values) => values
            .iter()
            .map(|value| config_json(value.value()))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        ConfigValue::Object(entries) => entries
            .iter()
            .map(|(key, value)| Ok((key.value().as_str().to_owned(), config_json(value.value())?)))
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(Value::Object),
    }
}

pub(super) fn yaml_json(value: &Yaml) -> Result<Value, String> {
    match value {
        Yaml::Null => Ok(Value::Null),
        Yaml::Boolean(value) => Ok(Value::Bool(*value)),
        Yaml::String(value) => Ok(Value::String(value.clone())),
        Yaml::Integer(value) => Ok(json!(value)),
        Yaml::Real(value) => serde_json::from_str(value)
            .map_err(|_| "catalog YAML number is not finite JSON".to_owned()),
        Yaml::Array(values) => values
            .iter()
            .map(yaml_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Yaml::Hash(entries) => entries
            .iter()
            .map(|(key, value)| {
                let key = key
                    .as_str()
                    .ok_or_else(|| "catalog YAML mapping key is not a string".to_owned())?;
                Ok((key.to_owned(), yaml_json(value)?))
            })
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(Value::Object),
        Yaml::Alias(_) | Yaml::BadValue => {
            Err("catalog YAML contains an unsupported value".to_owned())
        }
    }
}

#[cfg(test)]
#[path = "catalog_skill_roots_tests.rs"]
mod skill_roots_tests;

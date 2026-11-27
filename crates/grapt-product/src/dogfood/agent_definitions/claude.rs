//! Claude-format markdown agents, loaded as the lowest catalog layer.
//!
//! `.claude/agents/*.md` files (user home, then project directory) map onto
//! the existing definition type: the YAML frontmatter carries name,
//! description, tools, disallowedTools and model; the Markdown body is the
//! prompt. No parallel system: the same bounded capture, the same name and
//! model validation and the same narrow-only tool ceiling apply, so an
//! admitted Claude file can never widen what the user's own definitions
//! allow.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use yaml_rust2::Yaml;

use super::{provider_model, Definition};
use crate::dogfood::catalog_config::{user_owned, CatalogSources};
use crate::dogfood::catalogs::yaml_field;

/// Model spellings for Claude aliases, plus the explicit inherit.
const CLAUDE_MODEL_ALIASES: &[&str] = &["sonnet", "opus", "haiku", "inherit"];
const MAX_ALIAS_MAPPINGS: usize = 64;

/// The user's mapping from a Claude alias to a provider/model. Only
/// user-owned configuration may name it: a repository document can narrow
/// tools, not choose the model a Claude file inherits.
pub(super) fn model_aliases(sources: &CatalogSources) -> Result<BTreeMap<String, String>, String> {
    let mut aliases = BTreeMap::new();
    for document in &sources.documents {
        if !user_owned(document.origin) {
            continue;
        }
        let Some(agents) = document.value.get("agents") else {
            continue;
        };
        let Some(mappings) = agents.value().get("modelAliases") else {
            continue;
        };
        let Some(pairs) = mappings.value().as_object() else {
            return Err(document.refusal("agents.modelAliases must be an object"));
        };
        for (alias, target) in pairs {
            let alias = alias.value().as_str();
            let Some(target) = target.value().as_str() else {
                return Err(document.refusal("agents.modelAliases targets must be strings"));
            };
            if alias.len() > 64 {
                return Err(document.refusal("agents.modelAliases keys must use at most 64 bytes"));
            }
            provider_model(target).ok_or_else(|| {
                document.refusal("agents.modelAliases targets must be provider/model")
            })?;
            if aliases.len() == MAX_ALIAS_MAPPINGS {
                return Err(document.refusal("agents.modelAliases exceeds 64 mappings"));
            }
            aliases.insert(alias.to_ascii_lowercase(), target.to_owned());
        }
    }
    Ok(aliases)
}

/// The two Claude roots, lowest layer first: the user's home, then the
/// project's own directory.
pub(super) fn roots(sources: &CatalogSources) -> Result<Vec<std::path::PathBuf>, String> {
    let mut roots = vec![sources.environment.home.join(".claude").join("agents")];
    if sources.directory != sources.environment.config {
        let project = sources.directory.join(".claude").join("agents");
        let resolve = crate::dogfood::authority_path::resolve;
        if resolve(&project)? != resolve(&roots[0])? {
            roots.push(project);
        }
    }
    Ok(roots)
}

pub(super) fn read(
    path: &Path,
    total: &mut usize,
    aliases: &BTreeMap<String, String>,
) -> Result<(Definition, Vec<String>), String> {
    let (metadata, body) = super::files::read_markdown(path, total)?;
    let (definition, notes) = map(&metadata, &body, path, aliases)?;
    if !super::plain_name(&definition.name) {
        return Err("standalone Claude agent names must use an unqualified role name".into());
    }
    definition
        .validate()
        .map_err(|reason| format!("{}: {reason}", path.display()))?;
    Ok((definition, notes))
}

pub(super) fn map(
    metadata: &Yaml,
    body: &str,
    path: &Path,
    aliases: &BTreeMap<String, String>,
) -> Result<(Definition, Vec<String>), String> {
    // The format derives the name from the file name when the frontmatter
    // omits it; our identity space then decides whether that name is ours.
    let name = match yaml_field(metadata, "name") {
        None => path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_owned(),
        Some(value) => value
            .as_str()
            .ok_or("agent name must be a string")?
            .trim()
            .to_owned(),
    };
    let description = match yaml_field(metadata, "description") {
        None => return Err("agent frontmatter requires a description".into()),
        Some(value) => value
            .as_str()
            .ok_or("agent description must be a string")?
            .trim()
            .to_owned(),
    };
    if description.is_empty() {
        return Err("agent description must contain text".into());
    }
    let developer_instructions = if body.trim().is_empty() {
        return Err("agent Markdown body must contain the prompt".into());
    } else {
        body.to_owned()
    };
    let model = match yaml_field(metadata, "model") {
        None | Some(Yaml::Null) => None,
        Some(value) => map_model(
            value.as_str().ok_or("agent model must be a string")?,
            aliases,
        ),
    };
    let raw_tools = match yaml_field(metadata, "tools") {
        None | Some(Yaml::Null) => None,
        Some(value) => Some(tool_names(value)?),
    };
    // The ceiling keeps exactly the names this harness offers. Everything
    // else is dropped with a visible note; nothing is ever widened to cover
    // a name we do not have.
    let (mut tools, notes) = match &raw_tools {
        Some(raw) => {
            let mut kept = BTreeSet::new();
            let mut dropped = Vec::new();
            for tool in raw {
                if known_tool_name(tool) {
                    kept.insert(tool.clone());
                } else {
                    dropped.push(tool.clone());
                }
            }
            let mut notes = Vec::new();
            if !dropped.is_empty() {
                notes.push(format!("dropped unknown tools: {}", dropped.join(", ")));
            }
            (Some(kept), notes)
        }
        None => (None, Vec::new()),
    };
    if let Some(value) = yaml_field(metadata, "disallowedTools") {
        if !value.is_null() {
            let disallowed = tool_names(value)?;
            // An exclusion we cannot apply would silently widen the agent,
            // so the file refuses instead of importing it half-enforced.
            for tool in &disallowed {
                if !known_tool_name(tool) {
                    return Err(format!(
                        "disallowedTools names a tool this harness does not offer: {tool}"
                    ));
                }
            }
            let mut kept = match tools {
                Some(kept) => kept,
                // An exclusion binds even when the file names no tools: the
                // ceiling starts as everything this harness offers, so the
                // list can only narrow, never be dropped.
                None => known_tool_names(),
            };
            kept.retain(|tool| !disallowed.contains(tool));
            tools = Some(kept);
        }
    }
    Ok((
        Definition {
            mode: super::Mode::All,
            steps: None,
            disabled: false,
            hidden: false,
            name,
            description,
            developer_instructions,
            model,
            tools,
            color: None,
        },
        notes,
    ))
}

/// An alias the user mapped wins over the default inherit; the four Claude
/// aliases inherit; any other provider/model-shaped value rides the
/// existing routing; anything else inherits as well, never as a guess.
fn map_model(value: &str, aliases: &BTreeMap<String, String>) -> Option<String> {
    let key = value.trim();
    if key.is_empty() {
        return None;
    }
    if let Some(mapped) = aliases.get(&key.to_ascii_lowercase()) {
        return Some(mapped.clone());
    }
    if CLAUDE_MODEL_ALIASES.contains(&key.to_ascii_lowercase().as_str()) {
        return None;
    }
    provider_model(key).map(|(provider, id)| format!("{provider}/{id}"))
}

pub(super) fn tool_names(value: &Yaml) -> Result<BTreeSet<String>, String> {
    match value {
        Yaml::String(text) => Ok(text
            .split(',')
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(str::to_owned)
            .collect()),
        Yaml::Array(items) => {
            let mut names = BTreeSet::new();
            for item in items {
                let tool = item
                    .as_str()
                    .ok_or("agent tools must name strings only")?
                    .trim();
                if tool.is_empty() {
                    return Err("agent tools must not name the empty string".into());
                }
                names.insert(tool.to_owned());
            }
            Ok(names)
        }
        _ => Err("agent tools must be a comma list or an array of names".into()),
    }
}

fn known_tool_name(tool: &str) -> bool {
    super::super::tools::BUILTIN_TOOL_NAMES.contains(&tool)
        || super::super::plugins::RESERVED_TOOL_NAMES.contains(&tool)
}

/// The full name set a ceiling can be cut from: every tool this harness
/// offers, built-in and reserved.
pub(super) fn known_tool_names() -> BTreeSet<String> {
    super::super::tools::BUILTIN_TOOL_NAMES
        .iter()
        .chain(super::super::plugins::RESERVED_TOOL_NAMES.iter())
        .map(|name| (*name).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogfood::tools::WRITE_TOOL;

    #[test]
    fn model_aliases_inherit_by_default_and_honor_a_user_mapping() {
        let none: BTreeMap<String, String> = BTreeMap::new();
        for alias in ["sonnet", "opus", "haiku", "inherit", "SONNET", ""] {
            assert_eq!(map_model(alias, &none), None, "{alias} should inherit");
        }
        assert_eq!(map_model("some-unknown-alias", &none), None);
        assert_eq!(
            map_model("anthropic/claude-sonnet-4-5", &none),
            Some("anthropic/claude-sonnet-4-5".to_owned())
        );
        assert_eq!(map_model("not a model", &none), None);
        let mut mapped = BTreeMap::new();
        mapped.insert("sonnet".to_owned(), "fixture/fixture-model".to_owned());
        assert_eq!(
            map_model("sonnet", &mapped),
            Some("fixture/fixture-model".to_owned())
        );
        assert_eq!(map_model("opus", &mapped), None);
    }

    #[test]
    fn tool_names_accept_a_comma_list_or_an_array_and_refuse_other_shapes() {
        let comma = tool_names(&Yaml::String("read , grep,,exec_command".to_owned()))
            .unwrap_or_else(|error| panic!("comma list: {error}"));
        assert_eq!(comma.len(), 3);
        assert!(comma.contains("exec_command"));
        let array = tool_names(&Yaml::Array(vec![
            Yaml::String("read".to_owned()),
            Yaml::String(" grep ".to_owned()),
        ]))
        .unwrap_or_else(|error| panic!("array: {error}"));
        assert_eq!(array.len(), 2);
        assert!(array.contains("grep"));
        for invalid in [
            Yaml::Integer(1),
            Yaml::Hash(Default::default()),
            Yaml::Array(vec![Yaml::String("".to_owned())]),
            Yaml::Array(vec![Yaml::Integer(2)]),
        ] {
            assert!(
                tool_names(&invalid).is_err(),
                "{invalid:?} became a tool list"
            );
        }
    }

    #[test]
    fn frontmatter_maps_onto_the_existing_definition_shape() {
        let metadata = yaml_rust2::YamlLoader::load_from_str(
            "name: helper\ndescription: Help things\nmodel: sonnet\ntools: read, Bash, Grep\ndisallowedTools: [read]",
        )
        .unwrap_or_else(|error| panic!("test frontmatter: {error}"))
        .pop()
        .unwrap_or_else(|| panic!("frontmatter lost its document"));
        let (definition, notes) = map(
            &metadata,
            "Do the helper work.\n",
            Path::new("helper.md"),
            &BTreeMap::new(),
        )
        .unwrap_or_else(|error| panic!("frontmatter mapping failed: {error}"));
        assert_eq!(definition.name, "helper");
        assert_eq!(definition.description, "Help things");
        assert_eq!(definition.developer_instructions, "Do the helper work.\n");
        assert!(
            matches!(definition.model(), Ok(None)),
            "sonnet must inherit"
        );
        let tools = definition
            .tools
            .as_ref()
            .unwrap_or_else(|| panic!("ceiling lost"));
        assert_eq!(
            tools.len(),
            0,
            "Bash and Grep are unknown, read is disallowed"
        );
        assert_eq!(notes, vec!["dropped unknown tools: Bash, Grep"]);
        let no_name = yaml_rust2::YamlLoader::load_from_str("description: Nameless\n")
            .unwrap_or_else(|error| panic!("test frontmatter: {error}"))
            .pop()
            .unwrap_or_else(|| panic!("frontmatter lost its document"));
        let (definition, _) = map(
            &no_name,
            "Body.\n",
            Path::new("from-stem.md"),
            &BTreeMap::new(),
        )
        .unwrap_or_else(|error| panic!("stem name failed: {error}"));
        assert_eq!(definition.name, "from-stem");
        let disallowed_only = yaml_rust2::YamlLoader::load_from_str(
            "name: guard\ndescription: Guard\ndisallowedTools: [write]",
        )
        .unwrap_or_else(|error| panic!("test frontmatter: {error}"))
        .pop()
        .unwrap_or_else(|| panic!("frontmatter lost its document"));
        let (definition, notes) = map(
            &disallowed_only,
            "Body.\n",
            Path::new("guard.md"),
            &BTreeMap::new(),
        )
        .unwrap_or_else(|error| panic!("disallowed mapping failed: {error}"));
        assert!(notes.is_empty(), "{notes:?}");
        let tools = definition
            .tools
            .as_ref()
            .unwrap_or_else(|| panic!("exclusion lost its ceiling"));
        let known = known_tool_names();
        assert!(!tools.contains(WRITE_TOOL));
        assert_eq!(tools.len(), known.len() - 1);
        for name in known.iter().filter(|name| *name != WRITE_TOOL) {
            assert!(tools.contains(name.as_str()), "{name} missing");
        }
        // A supported tool in a different spelling, or a tool we do not
        // offer, cannot be applied: the file refuses instead of importing
        // with the exclusion silently unenforced.
        for unsupported in ["Write", "Bash", "TodoWrite"] {
            let text = format!("name: guard\ndescription: Guard\ndisallowedTools: [{unsupported}]");
            let metadata = yaml_rust2::YamlLoader::load_from_str(&text)
                .unwrap_or_else(|error| panic!("test frontmatter: {error}"))
                .pop()
                .unwrap_or_else(|| panic!("frontmatter lost its document"));
            let error = map(
                &metadata,
                "Body.\n",
                Path::new("guard.md"),
                &BTreeMap::new(),
            )
            .err()
            .unwrap_or_else(|| panic!("{unsupported} imported with an unenforced exclusion"));
            assert!(error.contains(unsupported), "{error}");
        }
    }
}

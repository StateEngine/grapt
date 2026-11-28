//! The permission preset a Session's calls are decided under, and the
//! authority stack built from it.
//!
//! An explicit root Session selection wins over configured defaults. Global
//! config and then service.conf choose the default; project config only narrows.
//! Delegated Sessions also obey every ancestor's ceiling and may narrow further.
//! All resolved authority sources, including absent ones, require attended edits.

mod config;
mod execution;

pub(super) use super::authority_path::resolve as resolved_path;
pub(super) use config::validate_agent_scopes;

use std::path::Path;

use grapt_permission::preset::{Compose, Effect, Layer, LayerName, Preset, Rule, DEFAULT_PRESET};

use super::catalog_config::{user_owned, CatalogEnvironment, CatalogSources};

/// The configuration key that names a preset.
const KEY: &str = "permission_preset";

#[derive(Debug, Clone)]
pub(super) struct ResolvedPolicy {
    pub preset: Preset,
    owner_paths: std::collections::BTreeSet<String>,
    /// The Session's own directory: destructive commands that write past it ask.
    workspace: String,
    configured: Vec<config::ConfiguredLayer>,
    agent_tools: Vec<std::collections::BTreeSet<String>>,
    agent_roots: std::collections::BTreeSet<String>,
    ceilings: Vec<Ceiling>,
}

/// An ancestor keeps its own ordered policy until the actual request is known.
/// Only its resulting decision becomes the child's floor.
#[derive(Debug, Clone)]
struct Ceiling {
    base: Vec<Layer>,
    configured: Vec<config::ConfiguredLayer>,
}

impl Ceiling {
    fn layers(&self, checks: &execution::Checks<'_>) -> Vec<Layer> {
        let mut layers = self.base.clone();
        layers.extend(
            self.configured
                .iter()
                .flat_map(|layer| layer.materialize(checks)),
        );
        layers
    }
}

const MAX_PARENT_DEPTH: usize = 64;
const MAX_AUTHORITY_PATHS: usize = 512;

pub(super) fn effective(
    service_root: &Path,
    record: &grapt_session::SessionRecord,
    mut parent_record: impl FnMut(
        &grapt_types::SessionId,
    ) -> Result<grapt_session::SessionRecord, String>,
) -> Result<ResolvedPolicy, String> {
    let mut policy = resolve(service_root, record)?;
    let mut configured_rules = policy
        .configured
        .iter()
        .map(config::ConfiguredLayer::rule_count)
        .sum::<usize>();
    let mut parent = record.parent.clone();
    let mut visited = std::collections::BTreeSet::from([record.session.clone()]);
    for _ in 0..MAX_PARENT_DEPTH {
        let Some(id) = parent else {
            return Ok(policy);
        };
        if !visited.insert(id.clone()) {
            return Err("Session permission lineage contains a cycle".to_owned());
        }
        let ancestor = parent_record(&id)?;
        let inherited = resolve(service_root, &ancestor)?;
        configured_rules += inherited
            .configured
            .iter()
            .map(config::ConfiguredLayer::rule_count)
            .sum::<usize>();
        if configured_rules > 4096 {
            return Err("effective permission configuration exceeds 4096 rules".into());
        }
        policy.ceilings.push(inherited.ceiling());
        policy.agent_tools.extend(inherited.agent_tools);
        policy.agent_roots.extend(inherited.agent_roots);
        policy.preset = policy.preset.strictest(inherited.preset);
        policy.owner_paths.extend(inherited.owner_paths);
        if policy.owner_paths.len() > MAX_AUTHORITY_PATHS {
            return Err("permission authority source bound exceeded".to_owned());
        }
        parent = ancestor.parent;
    }
    if parent.is_none() {
        Ok(policy)
    } else {
        Err("Session permission lineage exceeds 64 parents".to_owned())
    }
}

/// The configuration an agent edit must ask about. Both configuration
/// spellings are owner paths whichever one is in use: creating the unused one
/// would change which file is read.
fn owner_paths(
    service_root: &Path,
    sources: CatalogSources,
) -> Result<std::collections::BTreeSet<String>, String> {
    let mut owner_paths = std::collections::BTreeSet::new();
    for path in sources
        .document_paths
        .into_iter()
        .chain([service_root.join("service.conf")])
        .chain(super::legacy_names::every_config_file(service_root))
        .chain(super::legacy_names::every_config_file(
            &sources.environment.config,
        ))
    {
        owner_paths.insert(resolved_path(&path)?);
    }
    if owner_paths.len() > MAX_AUTHORITY_PATHS {
        return Err("permission authority source bound exceeded".to_owned());
    }
    Ok(owner_paths)
}

fn resolve(
    service_root: &Path,
    record: &grapt_session::SessionRecord,
) -> Result<ResolvedPolicy, String> {
    let sources = CatalogSources::read(
        Path::new(record.placement.directory()),
        CatalogEnvironment::read(service_root)?,
    )?;
    let selected = record
        .preset
        .as_deref()
        .map(|name| parse(name, "the Session's permission preset"))
        .transpose()?;
    let mut preset = DEFAULT_PRESET;
    if selected == Some(Preset::Bypass) || (record.parent.is_none() && selected.is_some()) {
        // Explicit bypass applies to children too; effective() still narrows
        // against every parent's preset before any call can use this policy.
        preset = selected.unwrap_or(DEFAULT_PRESET);
    } else {
        let mut narrowing = Vec::new();
        for document in &sources.documents {
            let Some(value) = document.value.get(KEY) else {
                continue;
            };
            let name = value
                .value()
                .as_str()
                .ok_or_else(|| format!("{}: {KEY} must be a string", document.source))?;
            let named = parse(name, &document.source)?;
            if user_owned(document.origin) {
                preset = named;
            } else {
                narrowing.push(named);
            }
        }
        if let Some(name) = crate::input::read_permission_preset(&service_root.join("service.conf"))
            .map_err(|error| error.to_string())?
        {
            let named = parse(&name, "service.conf")?;
            let origin = super::catalog_config::source_origin(
                &service_root.join("service.conf"),
                grapt_plugin::PluginOrigin::UserGlobal,
                Path::new(record.placement.directory()),
                service_root,
            )?;
            if user_owned(origin) {
                preset = named;
            } else {
                narrowing.push(named);
            }
        }
        // Only a user-owned default may choose bypass. Repository rules cannot
        // activate it, and cannot re-enable prompts after the user chose it.
        if preset != Preset::Bypass {
            preset = narrowing.into_iter().fold(preset, Preset::strictest);
        }
        if let Some(selected) = selected {
            // A delegated selection is a ceiling; its configuration may narrow further.
            preset = preset.strictest(selected);
        }
    }
    let mut configured = config::layers(
        &sources.documents,
        record.agent.as_deref().unwrap_or("default"),
    )?;
    let agents = super::agent_definitions::Catalog::from_sources(&sources)?;
    let mut agent_tools: Vec<_> = agents
        .selected(record.agent.as_deref().unwrap_or("default"))?
        .and_then(|agent| agent.tools.clone())
        .into_iter()
        .collect();
    if record.agent.as_deref() == Some(super::agent_definitions::PLAN_AGENT) {
        let workspace = resolved_path(Path::new(record.placement.directory()))?;
        let plans = Path::new(&workspace).join("plans");
        configured.push(config::ConfiguredLayer::planning(
            plans
                .to_str()
                .ok_or("plan directory is not UTF-8")?
                .to_owned(),
        ));
        agent_tools.push(super::agent_definitions::planning_tools());
    }
    let agent_roots = agents
        .roots
        .iter()
        .map(|path| resolved_path(path))
        .collect::<Result<_, _>>()?;
    let owner_paths = owner_paths(service_root, sources)?;
    Ok(ResolvedPolicy {
        preset,
        owner_paths,
        workspace: record.placement.directory().to_owned(),
        agent_tools,
        agent_roots,
        configured,
        ceilings: Vec::new(),
    })
}

/// A configured name that is not a preset is refused without echoing it:
/// configuration values stay out of refusals.
fn parse(name: &str, source: &str) -> Result<Preset, String> {
    Preset::parse(name).ok_or_else(|| {
        let available: Vec<&str> = Preset::ALL.iter().map(|preset| preset.name()).collect();
        format!(
            "{source}: {KEY} names no permission preset; available: {}",
            available.join(", ")
        )
    })
}

/// What a tool call is told when its permission cannot be started: a
/// preset's denial in its own words, or why the request failed.
pub(super) fn refusal(error: grapt_permission::PermissionHttpError, preset: Preset) -> String {
    match error {
        grapt_permission::PermissionHttpError::Denied {
            action,
            layer: "preset",
        } => {
            format!(
                "{action} is denied by this Session's {} permission preset; do not repeat the call",
                preset.name()
            )
        }
        grapt_permission::PermissionHttpError::Denied { action, layer } => {
            format!("{action} is denied by the {layer} permission policy; do not repeat the call")
        }
        error => format!("tool permission could not be started: {error}"),
    }
}

/// The narrowing authorities a call is decided by: the owner floor, the
/// preset, and an agent's configured allowance for `allowed`, which lifts an
/// `ask` as a user's own allow rule does but never a deny.
impl ResolvedPolicy {
    fn base_layers(&self, allowed: Option<&str>) -> Vec<Layer> {
        let mut owner = Layer::owner();
        owner.rules.extend(
            self.owner_paths
                .iter()
                .map(|path| Rule::path("edit", path, Effect::Ask)),
        );
        let mut layers = vec![owner, Layer::preset_in(self.preset, Some(&self.workspace))];
        if let Some(action) = allowed {
            layers.push(Layer {
                name: LayerName::Config,
                rules: vec![Rule::new(action, "*", Effect::Allow)],
                floor: false,
                compose: Compose::Override,
            });
        }
        layers
    }

    fn own_layers(&self, allowed: Option<&str>, checks: &execution::Checks<'_>) -> Vec<Layer> {
        let mut layers = self.base_layers(allowed);
        layers.extend(
            self.configured
                .iter()
                .flat_map(|layer| layer.materialize(checks)),
        );
        layers
    }

    fn ceiling(&self) -> Ceiling {
        Ceiling {
            base: self.base_layers(None),
            configured: self.configured.clone(),
        }
    }

    /// Build the same stack for API requests and prepared tool consequences.
    /// Each ancestor is evaluated under its own ordered rules before its
    /// decision becomes a floor for this request; flattening would lose grants
    /// within that ancestor or let a child override its restrictions.
    pub(super) fn layers(
        &self,
        allowed: Option<&str>,
        checks: &[grapt_permission::PermissionConsequence],
    ) -> Vec<Layer> {
        self.layers_readings(allowed, checks, &execution::Checks::new(checks))
    }

    /// A terminal's command comes from its prepared target, never from a
    /// footer inferred from caller-controlled resource text.
    pub(super) fn terminal_layers(
        &self,
        checks: &[grapt_permission::PermissionConsequence],
        command: &str,
        allow_grants: bool,
    ) -> Vec<Layer> {
        self.layers_readings(
            None,
            checks,
            &execution::Checks::terminal_exec(checks, command, allow_grants),
        )
    }

    fn layers_readings(
        &self,
        allowed: Option<&str>,
        checks: &[grapt_permission::PermissionConsequence],
        readings: &execution::Checks<'_>,
    ) -> Vec<Layer> {
        let mut layers = self.own_layers(allowed, readings);
        let rules = checks
            .iter()
            .filter(|check| {
                // An unrestricted host process can replace its own definition,
                // including through scripts or stdin. Tool-restricted agents
                // need an owner decision before handing it that authority.
                (check.action == "shell" && !self.agent_tools.is_empty())
                    || (matches!(
                        check.action.as_str(),
                        "edit"
                            | "fs.create"
                            | "fs.modify"
                            | "fs.overwrite"
                            | "fs.delete"
                            | "fs.move"
                    ) && agent_directory(&self.agent_roots, &check.resource))
            })
            .map(|check| Rule::literal(&check.action, &check.resource, Effect::Ask))
            .collect::<Vec<_>>();
        if !rules.is_empty() {
            layers.push(Layer {
                name: LayerName::Owner,
                rules,
                floor: true,
                compose: Compose::Narrow,
            });
        }
        let ceilings: Vec<_> = self
            .ceilings
            .iter()
            .map(|ceiling| ceiling.layers(readings))
            .collect();
        let mut rules = Vec::new();
        for check in checks {
            let effect = ceilings
                .iter()
                .map(|ceiling| {
                    grapt_permission::preset::decide(&check.action, &[&check.resource], ceiling)
                        .effect
                })
                .max()
                .unwrap_or(Effect::Allow);
            if effect != Effect::Allow {
                rules.push(Rule::literal(&check.action, &check.resource, effect));
            }
        }
        if !rules.is_empty() {
            layers.push(Layer {
                name: LayerName::Ancestor,
                rules,
                floor: true,
                compose: Compose::Narrow,
            });
        }
        layers
    }

    /// Definition tool lists only narrow the Session and every descendant.
    pub(super) fn permits_agent_tool(&self, tool: &str) -> bool {
        self.agent_tools.iter().all(|tools| tools.contains(tool))
    }

    /// Whether the model is never offered `tool`: every action one of its
    /// calls can be decided under is withheld by this Session's narrowing
    /// layers (the oracle's tool materialisation, `tool.ts`). A tool denied
    /// for only some of its actions or resources stays offered and is refused
    /// when a call names them.
    pub(super) fn withholds(&self, tool: &str) -> bool {
        if !self.permits_agent_tool(tool) {
            return true;
        }
        let probes: Vec<_> = self
            .configured
            .iter()
            .chain(
                self.ceilings
                    .iter()
                    .flat_map(|ceiling| ceiling.configured.iter()),
            )
            .filter_map(config::ConfiguredLayer::planning_root)
            .flat_map(|root| {
                tool_actions(tool).into_iter().map(move |action| {
                    grapt_permission::PermissionConsequence {
                        action: action.to_owned(),
                        resource: format!("{root}/plan.md"),
                    }
                })
            })
            .collect();
        let checks = execution::Checks::new(&probes);
        let own = self.own_layers(None, &checks);
        tool_actions(tool).iter().all(|action| {
            grapt_permission::preset::withheld(action, &own)
                || self.ceilings.iter().any(|ceiling| {
                    grapt_permission::preset::withheld(action, &ceiling.layers(&checks))
                })
        })
    }
}

/// Compare components rather than interpreting configuration path characters as globs.
fn agent_directory(roots: &std::collections::BTreeSet<String>, resource: &str) -> bool {
    let normalize = |path: &str| grapt_types::path::comparison_key(&path.replace('\\', "/"));
    let resource = normalize(resource);
    roots.iter().any(|root| {
        let root = normalize(root);
        resource == root
            || resource.starts_with(&format!("{root}/"))
            || root.starts_with(&format!("{resource}/"))
    })
}

#[cfg(test)]
fn withheld_under(tool: &str, layers: &[Layer]) -> bool {
    tool_actions(tool)
        .iter()
        .all(|action| grapt_permission::preset::withheld(action, layers))
}

/// The permission actions a call of `tool` may be decided under, as
/// `tools::prepare_effect` names them. Any other tool (MCP, plugin) is
/// decided under its own name.
fn tool_actions(tool: &str) -> Vec<&str> {
    use super::tools::{APPLY_PATCH_TOOL, EDIT_TOOL, PATCH_TOOL, WRITE_TOOL};
    match tool {
        "exec_command" => vec!["shell"],
        "request_sandbox_access" => vec![grapt_permission::SANDBOX_WRITE_ACTION, "shell"],
        "write_stdin" => vec!["shell", "shell.poll"],
        "view_image" => vec!["read"],
        EDIT_TOOL => vec!["fs.modify"],
        WRITE_TOOL => vec!["fs.create", "fs.overwrite"],
        PATCH_TOOL => vec!["fs.create", "fs.modify", "fs.delete", "fs.move"],
        "web_fetch" => vec!["webfetch"],
        APPLY_PATCH_TOOL => vec![
            "fs.create",
            "fs.modify",
            "fs.overwrite",
            "fs.delete",
            "fs.move",
        ],
        other => vec![other],
    }
}

#[cfg(test)]
mod withholding_tests {
    use super::super::tools::{
        APPLY_PATCH_TOOL, EDIT_TOOL, PATCH_TOOL, READ_TOOL, SHELL_TOOL, WRITE_TOOL,
    };
    use super::*;

    fn stack(preset: Preset) -> Vec<Layer> {
        vec![Layer::owner(), Layer::preset(preset)]
    }

    #[test]
    fn native_fetch_uses_the_existing_read_only_webfetch_permission_action() {
        for preset in Preset::ALL {
            assert!(!withheld_under("web_fetch", &stack(preset)));
        }
    }

    /// A tool is withheld only when every action its calls can be decided
    /// under is withheld: inspect withholds the writing tools whole.
    #[test]
    fn a_tool_is_withheld_only_when_all_its_actions_are() {
        for tool in [
            WRITE_TOOL,
            EDIT_TOOL,
            PATCH_TOOL,
            APPLY_PATCH_TOOL,
            SHELL_TOOL,
            "exec_command",
            "mcp__server__tool",
        ] {
            assert!(withheld_under(tool, &stack(Preset::Inspect)), "{tool}");
        }
        for tool in [
            READ_TOOL,
            WRITE_TOOL,
            PATCH_TOOL,
            APPLY_PATCH_TOOL,
            SHELL_TOOL,
            "exec_command",
            "write_stdin",
        ] {
            assert!(!withheld_under(tool, &stack(Preset::Maintainer)), "{tool}");
        }
        assert!(withheld_under(SHELL_TOOL, &stack(Preset::Author)));
        assert!(withheld_under("exec_command", &stack(Preset::Author)));
        assert!(!withheld_under("write_stdin", &stack(Preset::Author)));
        assert!(!withheld_under(PATCH_TOOL, &stack(Preset::Author)));
    }

    #[test]
    fn codex_add_overwrites_stay_offered_when_other_patch_effects_are_denied() {
        let mut layers = stack(Preset::Maintainer);
        layers.push(Layer {
            name: LayerName::Config,
            rules: ["fs.create", "fs.modify", "fs.delete", "fs.move"]
                .into_iter()
                .map(|action| Rule::new(action, "*", Effect::Deny))
                .collect(),
            floor: false,
            compose: Compose::Narrow,
        });
        assert!(withheld_under(PATCH_TOOL, &layers));
        assert!(!withheld_under(APPLY_PATCH_TOOL, &layers));
        assert_eq!(
            grapt_permission::preset::decide("fs.overwrite", &["/repo/existing"], &layers).effect,
            Effect::Ask
        );
    }

    #[test]
    fn stdin_polls_remain_offered_when_shell_writes_are_denied() {
        for preset in Preset::ALL {
            let mut layers = stack(preset);
            assert!(!withheld_under("write_stdin", &layers));
            layers.push(Layer {
                name: LayerName::Config,
                rules: vec![Rule::new("shell.poll", "*", Effect::Deny)],
                floor: false,
                compose: Compose::Narrow,
            });
            assert_eq!(
                withheld_under("write_stdin", &layers),
                !matches!(preset, Preset::Maintainer | Preset::Bypass)
            );
            let last = layers
                .last_mut()
                .unwrap_or_else(|| panic!("missing config layer"));
            last.rules.push(Rule::new("shell", "*", Effect::Deny));
            assert_eq!(
                withheld_under("write_stdin", &layers),
                preset != Preset::Bypass
            );
        }
    }

    #[test]
    fn images_use_read_availability_and_an_explicit_read_deny_hides_them() {
        let mut layers = stack(Preset::Inspect);
        assert!(!withheld_under("view_image", &layers));
        layers.push(Layer {
            name: LayerName::Config,
            rules: vec![Rule::new("read", "*", Effect::Deny)],
            floor: false,
            compose: Compose::Narrow,
        });
        assert!(withheld_under("view_image", &layers));
    }

    /// A tool denied in part stays offered, and the
    /// denied part is still refused once a call names it.
    #[test]
    fn a_partly_denied_tool_stays_offered_and_its_denied_part_is_refused() {
        let mut layers = stack(Preset::Maintainer);
        layers.push(Layer {
            name: LayerName::Config,
            rules: vec![
                Rule::new("fs.delete", "*", Effect::Deny),
                Rule::new("fs.move", "*", Effect::Deny),
                Rule::new("shell", "rm *", Effect::Deny),
            ],
            floor: false,
            compose: Compose::Narrow,
        });
        assert!(!withheld_under(PATCH_TOOL, &layers));
        assert!(!withheld_under(SHELL_TOOL, &layers));
        let decide = |action: &str, resource: &str| {
            grapt_permission::preset::decide(action, &[resource], &layers).effect
        };
        assert_eq!(decide("fs.delete", "/repo/old.rs"), Effect::Deny);
        assert_eq!(decide("fs.modify", "/repo/main.rs"), Effect::Allow);
        assert_eq!(decide("shell", "rm -rf build"), Effect::Deny);
        assert_eq!(decide("shell", "cargo test"), Effect::Ask);
    }

    /// Under Maintainer, shell commands ask rather than running unasked,
    /// including both workspace-local and out-of-workspace writes.
    #[test]
    fn the_session_directory_bounds_destructive_shell_writes() {
        let policy = ResolvedPolicy {
            preset: Preset::Maintainer,
            owner_paths: std::collections::BTreeSet::new(),
            agent_tools: Vec::new(),
            agent_roots: Default::default(),
            workspace: "/repo".to_owned(),
            configured: Vec::new(),
            ceilings: Vec::new(),
        };
        let layers = policy.layers(None, &[]);
        let decide =
            |command: &str| grapt_permission::preset::decide("shell", &[command], &layers).effect;
        assert_eq!(decide("echo x > /repo/out.txt"), Effect::Ask);
        assert_eq!(decide("echo x > /etc/hosts"), Effect::Ask);
        assert_eq!(decide("rm -rf build"), Effect::Ask);
        assert_eq!(decide("cargo test"), Effect::Ask);
    }

    /// Definition directories protect their own files: a write into an
    /// agent root, or any shell from a tool-restricted agent, asks even
    /// under a preset that would otherwise allow it.
    #[test]
    fn agent_roots_and_a_tool_restricted_shell_ask_for_their_own_writes() {
        let policy = ResolvedPolicy {
            preset: Preset::Maintainer,
            owner_paths: std::collections::BTreeSet::new(),
            agent_tools: vec![std::collections::BTreeSet::from([READ_TOOL.to_owned()])],
            agent_roots: std::collections::BTreeSet::from([
                "/repo/agents".to_owned(),
                "/home/example/.claude/agents".to_owned(),
            ]),
            workspace: "/repo".to_owned(),
            configured: Vec::new(),
            ceilings: Vec::new(),
        };
        let check = |action: &str, resource: &str| grapt_permission::PermissionConsequence {
            action: action.to_owned(),
            resource: resource.to_owned(),
        };
        let checks = [
            check("shell", "cargo test"),
            check("fs.create", "/home/example/.claude/agents/evil.md"),
            check("fs.modify", "/repo/agents/main.toml"),
            check("fs.create", "/repo/other/notes.md"),
        ];
        let layers = policy.layers(None, &checks);
        let decide = |action: &str, resource: &str| {
            grapt_permission::preset::decide(action, &[resource], &layers).effect
        };
        assert_eq!(decide("shell", "cargo test"), Effect::Ask);
        assert_eq!(
            decide("fs.create", "/home/example/.claude/agents/evil.md"),
            Effect::Ask
        );
        assert_eq!(decide("fs.modify", "/repo/agents/main.toml"), Effect::Ask);
        assert_eq!(decide("fs.create", "/repo/other/notes.md"), Effect::Allow);
    }
}

#[cfg(test)]
#[path = "permission_preset/config_tests.rs"]
mod config_tests;

#[cfg(test)]
#[path = "permission_preset/execution_config_tests.rs"]
mod execution_config_tests;

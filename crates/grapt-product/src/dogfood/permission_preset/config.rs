//! Ordered user rules and repository narrowing over existing permission inputs.
use super::*;
use crate::dogfood::catalog_config::CatalogDocument;
use grapt_config::ConfigValue;

pub(super) const MAX_RULES: usize = 1024;
const MAX_PATTERN_BYTES: usize = 4096;

/// One source's ordinary and argv rules share its authority and ordering.
#[derive(Debug, Clone)]
pub(super) struct ConfiguredLayer {
    ordinary: Layer,
    execution: Vec<execution::PrefixRule>,
    planning_root: Option<String>,
}

impl ConfiguredLayer {
    pub(super) fn planning(root: String) -> Self {
        Self {
            ordinary: Layer {
                name: LayerName::Config,
                rules: vec![Rule::new("edit", "*", Effect::Deny)],
                floor: true,
                compose: Compose::Narrow,
            },
            execution: Vec::new(),
            planning_root: Some(root),
        }
    }

    pub(super) fn planning_root(&self) -> Option<&str> {
        self.planning_root.as_deref()
    }

    pub(super) fn rule_count(&self) -> usize {
        self.ordinary.rules.len() + self.execution.len()
    }

    pub(super) fn materialize(&self, checks: &execution::Checks<'_>) -> Vec<Layer> {
        let mut ordinary = self.ordinary.clone();
        if let Some(root) = &self.planning_root {
            ordinary.rules.extend(checks.planning_rules(root));
        }
        let mut layers = vec![ordinary];
        let rules = checks.rules(&self.execution);
        if !rules.is_empty() {
            layers.push(Layer {
                name: self.ordinary.name,
                rules,
                floor: self.ordinary.floor,
                compose: self.ordinary.compose,
            });
        }
        layers
    }
}

/// The singular key configures authority rules, never role definitions.
pub(in crate::dogfood) fn validate_agent_scopes(
    documents: &[CatalogDocument],
) -> Result<(), String> {
    for document in documents {
        let Some(legacy) = document.value.get("agent") else {
            continue;
        };
        let entries = legacy.value().as_object().ok_or_else(|| {
            document.refusal("agent must be an object; define roles under agents")
        })?;
        for (_, entry) in entries {
            let fields = entry
                .value()
                .as_object()
                .ok_or_else(|| document.refusal("agent entries must be objects"))?;
            if fields
                .iter()
                .any(|(name, _)| !matches!(name.value().as_str(), "permission" | "execpolicy"))
            {
                return Err(document.refusal(
                    "agent accepts only permission and execpolicy; define roles under agents",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn layers(
    documents: &[CatalogDocument],
    agent: &str,
) -> Result<Vec<ConfiguredLayer>, String> {
    validate_agent_scopes(documents)?;
    let mut layers = Vec::new();
    let mut total = 0;
    for document in documents {
        let mut rules = Vec::new();
        let mut execution = Vec::new();
        let mut parse_scope = |scope: &ConfigValue| -> Result<(), String> {
            if let Some(permission) = scope.get("permission") {
                parse(permission.value(), &mut rules)?;
            }
            if let Some(value) = scope.get("execpolicy") {
                execution::parse(value.value(), &mut execution)?;
            }
            Ok(())
        };
        parse_scope(&document.value).map_err(|reason| document.refusal(&reason))?;
        if let Some(agents) = document.value.get("agent") {
            let agents = agents.value();
            if agents.as_object().is_none() {
                return Err(document.refusal("agent configuration must be an object"));
            }
            if let Some(selected) = agents.get(agent) {
                if selected.value().as_object().is_none() {
                    return Err(document.refusal("selected agent configuration must be an object"));
                }
                parse_scope(selected.value()).map_err(|reason| document.refusal(&reason))?;
            }
        }
        total += rules.len() + execution.len();
        if total > MAX_RULES {
            return Err(document.refusal("permission configuration exceeds 1024 rules"));
        }
        if !rules.is_empty() || !execution.is_empty() {
            layers.push(ConfiguredLayer {
                planning_root: None,
                ordinary: Layer {
                    name: LayerName::Config,
                    rules,
                    floor: !user_owned(document.origin),
                    compose: if user_owned(document.origin) {
                        Compose::Override
                    } else {
                        Compose::Narrow
                    },
                },
                execution,
            });
        }
    }
    Ok(layers)
}

pub(super) fn effect(value: &ConfigValue) -> Result<Effect, String> {
    match value.as_str() {
        Some("allow") => Ok(Effect::Allow),
        Some("ask") => Ok(Effect::Ask),
        Some("deny") => Ok(Effect::Deny),
        _ => Err("permission effect must be allow, ask or deny".into()),
    }
}

fn push(
    rules: &mut Vec<Rule>,
    action: &str,
    resource: &str,
    value: &ConfigValue,
) -> Result<(), String> {
    if action.is_empty()
        || resource.is_empty()
        || action.len() > MAX_PATTERN_BYTES
        || resource.len() > MAX_PATTERN_BYTES
    {
        return Err("permission pattern must contain 1..4096 bytes".into());
    }
    if rules.len() >= MAX_RULES {
        return Err("permission configuration exceeds 1024 rules".into());
    }
    let action = match action {
        "bash" => "shell",
        "web_fetch" => "webfetch",
        "view_image" => "read",
        action => action,
    };
    rules.push(Rule::new(action, resource, effect(value)?));
    Ok(())
}

fn parse(value: &ConfigValue, rules: &mut Vec<Rule>) -> Result<(), String> {
    if value.as_str().is_some() {
        return push(rules, "*", "*", value);
    }
    let actions = value
        .as_object()
        .ok_or("permission must be an effect or an action object")?;
    for (action, value) in actions {
        let action = action.value().as_str();
        if let Some(resources) = value.value().as_object() {
            for (resource, effect) in resources {
                push(rules, action, resource.value().as_str(), effect.value())?;
            }
        } else {
            push(rules, action, "*", value.value())?;
        }
    }
    Ok(())
}

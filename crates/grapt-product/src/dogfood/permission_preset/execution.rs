//! Configured exact argv rules; shell syntax is owned by command_safety.
//!
//! Matching and strictest-rule aggregation follow the bounded
//! execpolicy matcher.
//! Apache-2.0, Copyright 2025 OpenAI. Modified: existing JSON configuration,
//! existing permission effects and no grant from a partly unmatched expression.
use super::*;
use grapt_config::ConfigValue;
use grapt_permission::prefix::PrefixPattern;

#[derive(Debug, Clone)]
pub(super) struct PrefixRule {
    pattern: PrefixPattern,
    effect: Effect,
}

pub(super) fn parse(value: &ConfigValue, rules: &mut Vec<PrefixRule>) -> Result<(), String> {
    let entries = value.as_array().ok_or("execpolicy must be a rule array")?;
    if entries.len() > super::config::MAX_RULES.saturating_sub(rules.len()) {
        return Err("permission configuration exceeds 1024 rules".into());
    }
    for entry in entries {
        let entry = entry.value();
        let fields = entry
            .as_object()
            .ok_or("execution rule must be an object")?;
        if fields
            .iter()
            .any(|(key, _)| !matches!(key.value().as_str(), "pattern" | "decision"))
        {
            return Err("execution rule accepts only pattern and decision".into());
        }
        let effect = super::config::effect(
            entry
                .get("decision")
                .ok_or("execution rule decision is absent")?
                .value(),
        )?;
        let positions = entry
            .get("pattern")
            .and_then(|value| value.value().as_array())
            .ok_or("execution rule pattern must be an argument array")?;
        if positions.is_empty() || positions.len() > 64 {
            return Err("execution prefix must contain 1..64 argument positions".into());
        }
        let positions = positions
            .iter()
            .map(|position| {
                let value = position.value();
                if let Some(literal) = value.as_str() {
                    return Ok(vec![literal.to_owned()]);
                }
                let alternatives = value
                    .as_array()
                    .ok_or("execution argument must be a literal or literal alternatives")?;
                if alternatives.is_empty() || alternatives.len() > 64 {
                    return Err("execution prefix position must contain 1..64 alternatives");
                }
                alternatives
                    .iter()
                    .map(|alternative| {
                        alternative
                            .value()
                            .as_str()
                            .map(str::to_owned)
                            .ok_or("execution prefix alternative must be a literal")
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        rules.push(PrefixRule {
            pattern: PrefixPattern::new(positions)?,
            effect,
        });
    }
    Ok(())
}

/// Deny and Ask survive any other command. Allow covers the whole expression
/// only when every parsed simple command has a matching rule. An unmatched
/// expression keeps the ordinary permission/classifier decision.
pub(super) fn decision(rules: &[PrefixRule], commands: &[Vec<String>]) -> Option<Effect> {
    let mut strictest = None;
    let mut covered = !commands.is_empty();
    for command in commands {
        let effect = rules
            .iter()
            .filter(|rule| rule.pattern.matches(command))
            .map(|rule| rule.effect)
            .max();
        covered &= effect.is_some();
        strictest = strictest.max(effect);
    }
    match strictest {
        Some(Effect::Allow) if !covered => None,
        effect => effect,
    }
}

/// Parse each shell consequence once, shared by this Session and its ancestors.
/// The classifier's lexer and wrapper walker own every argv reading.
pub(super) struct Checks<'a> {
    consequences: &'a [grapt_permission::PermissionConsequence],
    allow_grants: bool,
    shell: Vec<(
        &'a grapt_permission::PermissionConsequence,
        grapt_permission::command_safety::CommandReading,
    )>,
}

impl<'a> Checks<'a> {
    pub(super) fn new(checks: &'a [grapt_permission::PermissionConsequence]) -> Self {
        Self {
            consequences: checks,
            allow_grants: true,
            shell: checks
                .iter()
                .filter(|check| check.action == "shell")
                .map(|check| {
                    (
                        check,
                        grapt_permission::command_safety::simple_commands(&check.resource),
                    )
                })
                .collect(),
        }
    }

    /// Only the typed, prepared terminal target supplies this command. Its
    /// display resource still matches the final literal policy decision.
    pub(super) fn terminal_exec(
        checks: &'a [grapt_permission::PermissionConsequence],
        command: &str,
        allow_grants: bool,
    ) -> Self {
        Self {
            consequences: checks,
            allow_grants,
            shell: checks
                .iter()
                .filter(|check| check.action == "shell")
                .map(|check| {
                    (
                        check,
                        grapt_permission::command_safety::simple_commands(command),
                    )
                })
                .collect(),
        }
    }

    pub(super) fn rules(&self, rules: &[PrefixRule]) -> Vec<Rule> {
        if rules.is_empty() {
            return Vec::new();
        }
        self.shell
            .iter()
            .filter_map(|(check, reading)| {
                let effect = decision(rules, reading.commands())?;
                // Known restrictions survive partial input; partial input never grants.
                if effect == Effect::Allow && (!self.allow_grants || !reading.is_complete()) {
                    return None;
                }
                Some(Rule::literal(&check.action, &check.resource, effect))
            })
            .collect()
    }

    pub(super) fn planning_rules(&self, root: &str) -> Vec<Rule> {
        let root = grapt_types::path::comparison_key(&root.replace('\\', "/"));
        let unlinked = |path: &str| {
            let path = Path::new(path);
            for ancestor in path.ancestors() {
                match std::fs::symlink_metadata(ancestor) {
                    Ok(metadata) => {
                        if crate::dogfood::authority_path::linked(&metadata) {
                            return false;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return false,
                }
                if ancestor == Path::new(&root) {
                    return true;
                }
            }
            false
        };
        let document = |path: &str| {
            path.strip_prefix(&root)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|rest| {
                    unlinked(path)
                        && rest.ends_with(".md")
                        && rest
                            .split('/')
                            .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
                })
        };
        // Prepared patches include directory creation with their file effects.
        // Only required parents of a new plan document share its exception.
        let mut directories = std::collections::BTreeSet::new();
        let root_prefix = format!("{root}/");
        for check in self
            .consequences
            .iter()
            .filter(|check| check.action == "fs.create")
        {
            let path = grapt_types::path::comparison_key(&check.resource.replace('\\', "/"));
            if !document(&path) {
                continue;
            }
            let mut parent = path.rsplit_once('/').map(|(parent, _)| parent);
            while let Some(path) = parent {
                if path != root && !path.starts_with(&root_prefix) {
                    break;
                }
                directories.insert(path.to_owned());
                parent = path.rsplit_once('/').map(|(parent, _)| parent);
            }
        }
        self.consequences
            .iter()
            .filter(|check| {
                matches!(
                    check.action.as_str(),
                    "edit" | "fs.create" | "fs.modify" | "fs.overwrite" | "fs.delete" | "fs.move"
                )
            })
            .map(|check| {
                let path = grapt_types::path::comparison_key(&check.resource.replace('\\', "/"));
                let writable = (document(&path)
                    && matches!(
                        check.action.as_str(),
                        "fs.create" | "fs.modify" | "fs.overwrite"
                    ))
                    || (check.action == "fs.create" && directories.contains(&path));
                Rule::literal(
                    &check.action,
                    &check.resource,
                    if writable {
                        Effect::Allow
                    } else {
                        Effect::Deny
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;

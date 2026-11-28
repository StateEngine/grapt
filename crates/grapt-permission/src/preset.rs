//! Permission presets and the layered authority that decides with them.
//!
//! Ported from the frozen oracle's `preset.ts`
//! and `authority.ts`. A preset is a TRUST LEVEL for a Session: how much
//! leash its agent has. It is one layer of an ordered stack:
//!
//! 1. the owner floor: editing `grapt.json(c)` or `service.conf` asks under
//!    ordinary presets;
//! 2. the preset;
//! 3. user configuration overrides and repository narrowing;
//! 4. attended approvals ("always" replies and saved rules).
//!
//! Narrowing layers may only make a decision stricter. Override layers may
//! lift an `ask` to `allow`, never a deny and never the owner floor's ask.
//! Within one layer the last matching rule wins.
//! Explicit bypass skips all permission layers.

/// What a permission decision does with a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    Allow,
    Ask,
    Deny,
}

/// One rule: an action pattern, a resource pattern, and its effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub action: String,
    pub resource: String,
    pub effect: Effect,
    matcher: Matcher,
}

/// How a rule's resource is compared with a call's.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Matcher {
    /// `resource` is a wildcard pattern.
    Pattern,
    /// `resource` is a literal path.
    Path,
    /// Exact command bytes, without path or wildcard normalization.
    Literal,
    /// Any shell command the classifier finds destructive, judged against
    /// the Session's workspace when one is known.
    Destructive { workspace: Option<String> },
}

impl Rule {
    pub fn new(action: impl Into<String>, resource: impl Into<String>, effect: Effect) -> Self {
        Self {
            action: action.into(),
            resource: resource.into(),
            effect,
            matcher: Matcher::Pattern,
        }
    }

    /// A resolved authority path is literal, even when its filename contains glob characters.
    pub fn path(action: impl Into<String>, resource: impl Into<String>, effect: Effect) -> Self {
        Self {
            action: action.into(),
            resource: resource.into(),
            effect,
            matcher: Matcher::Path,
        }
    }

    /// A materialized command decision matches exact bytes, preserving shell
    /// argument case, backslashes and glob characters on every platform.
    pub fn literal(action: impl Into<String>, resource: impl Into<String>, effect: Effect) -> Self {
        Self {
            action: action.into(),
            resource: resource.into(),
            effect,
            matcher: Matcher::Literal,
        }
    }

    /// Matches a shell command [`crate::command_safety::classify`] finds
    /// destructive, with writes judged against `workspace`.
    pub fn destructive(action: impl Into<String>, effect: Effect, workspace: Option<&str>) -> Self {
        Self {
            action: action.into(),
            resource: "destructive command".to_owned(),
            effect,
            matcher: Matcher::Destructive {
                workspace: workspace.map(str::to_owned),
            },
        }
    }
}

/// A built-in rule, as the preset tables hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Builtin {
    action: &'static str,
    resource: &'static str,
    effect: Effect,
    destructive: bool,
}

impl Builtin {
    fn rule(self, workspace: Option<&str>) -> Rule {
        if self.destructive {
            Rule::destructive(self.action, self.effect, workspace)
        } else {
            Rule::new(self.action, self.resource, self.effect)
        }
    }
}

const fn rule(action: &'static str, resource: &'static str, effect: Effect) -> Builtin {
    Builtin {
        action,
        resource,
        effect,
        destructive: false,
    }
}

/// A rule for every command `command_safety` finds destructive.
const fn destructive(action: &'static str, effect: Effect) -> Builtin {
    Builtin {
        action,
        resource: "",
        effect,
        destructive: true,
    }
}

/// Session trust levels. Bypass requires an explicit user selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Preset {
    /// Read, search, query. Every mutation denied.
    Inspect,
    /// Write files, run nothing. Destructive changes asked.
    Author,
    /// Ordinary work allowed; destructive commands and authority edits asked.
    Maintainer,
    /// Explicitly skip permission checks, including owner and config floors.
    Bypass,
}

/// The preset a Session runs at when nothing chose one.
pub const DEFAULT_PRESET: Preset = Preset::Maintainer;

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Inspect,
        Preset::Author,
        Preset::Maintainer,
        Preset::Bypass,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Author => "author",
            Self::Maintainer => "maintainer",
            Self::Bypass => "bypass",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.name() == name)
    }

    /// Of two presets, the one granting less: careless `--inspect
    /// --maintainer` never grants more than was asked for.
    pub fn strictest(self, other: Self) -> Self {
        self.min(other)
    }

    fn table(self) -> &'static [Builtin] {
        match self {
            Self::Inspect => INSPECT,
            Self::Author => AUTHOR,
            Self::Maintainer => MAINTAINER,
            Self::Bypass => BYPASS,
        }
    }
}

/// Read-only surfaces. `list`, `read_lines` and `history` are this product's
/// read-only tools, which the oracle's table predates.
macro_rules! inspection {
    () => {
        [
            rule("read", "*", Effect::Allow),
            rule("read_lines", "*", Effect::Allow),
            rule("list", "*", Effect::Allow),
            rule("grep", "*", Effect::Allow),
            rule("glob", "*", Effect::Allow),
            rule("history", "*", Effect::Allow),
            rule("update_plan", "task checklist", Effect::Allow),
            rule("request_user_input", "user questions", Effect::Allow),
            rule("shell.poll", "session terminal output", Effect::Allow),
            rule("websearch", "*", Effect::Allow),
            rule("webfetch", "*", Effect::Allow),
        ]
    };
}

/// Author allows ordinary edits and asks before replacing, moving or deleting files.
macro_rules! authoring {
    () => {
        [
            rule("fs.create", "*", Effect::Allow),
            rule("fs.modify", "*", Effect::Allow),
            rule("fs.overwrite", "*", Effect::Ask),
            rule("fs.move", "*", Effect::Ask),
            rule("fs.delete", "*", Effect::Ask),
        ]
    };
}

const fn concat<const N: usize>(parts: &[&[Builtin]]) -> [Builtin; N] {
    let mut out = [rule("", "", Effect::Ask); N];
    let (mut index, mut part) = (0, 0);
    while part < parts.len() {
        let mut item = 0;
        while item < parts[part].len() {
            out[index] = parts[part][item];
            index += 1;
            item += 1;
        }
        part += 1;
    }
    assert!(index == N, "preset table length");
    out
}

const INSPECT: &[Builtin] = &concat::<13>(&[
    &[rule("*", "*", Effect::Deny)],
    &inspection!(),
    &[rule("question", "*", Effect::Allow)],
]);

const AUTHOR: &[Builtin] = &concat::<20>(&[
    &[rule("*", "*", Effect::Ask)],
    &inspection!(),
    &authoring!(),
    &[
        rule("shell", "*", Effect::Deny),
        rule("execute", "*", Effect::Deny),
        rule("external_directory", "*", Effect::Deny),
    ],
]);

// Default Ask with explicit read-only inspection and authoring allowances;
// shell commands, external directories, and unknown tools ask unless approved.
const MAINTAINER: &[Builtin] = &concat::<21>(&[
    &[rule("*", "*", Effect::Ask)],
    &inspection!(),
    &authoring!(),
    &[
        rule(crate::SANDBOX_WRITE_ACTION, "*", Effect::Ask),
        rule(crate::SANDBOX_MSYS_ACTION, "*", Effect::Ask),
        destructive("shell", Effect::Ask),
        rule("external_directory", "*", Effect::Ask),
    ],
]);

const BYPASS: &[Builtin] = &[rule("*", "*", Effect::Allow)];

/// A profile from before the product rename keeps its configuration under
/// these names, and the floor protects them the same way.
const LEGACY_JSON: &str = "*secode.json"; // grapt-rename: keep
const LEGACY_JSONC: &str = "*secode.jsonc"; // grapt-rename: keep

/// The owner floor: the config that grants authority is never edited unseen.
const OWNER_FLOOR: &[Builtin] = &concat::<30>(&[
    &[
        rule("fs.create", "*grapt.json", Effect::Ask),
        rule("fs.modify", "*grapt.json", Effect::Ask),
        rule("fs.overwrite", "*grapt.json", Effect::Ask),
        rule("fs.move", "*grapt.json", Effect::Ask),
        rule("fs.delete", "*grapt.json", Effect::Ask),
        rule("edit", "*grapt.json", Effect::Ask),
    ],
    &[
        rule("fs.create", "*grapt.jsonc", Effect::Ask),
        rule("fs.modify", "*grapt.jsonc", Effect::Ask),
        rule("fs.overwrite", "*grapt.jsonc", Effect::Ask),
        rule("fs.move", "*grapt.jsonc", Effect::Ask),
        rule("fs.delete", "*grapt.jsonc", Effect::Ask),
        rule("edit", "*grapt.jsonc", Effect::Ask),
    ],
    &[
        rule("fs.create", "*service.conf", Effect::Ask),
        rule("fs.modify", "*service.conf", Effect::Ask),
        rule("fs.overwrite", "*service.conf", Effect::Ask),
        rule("fs.move", "*service.conf", Effect::Ask),
        rule("fs.delete", "*service.conf", Effect::Ask),
        rule("edit", "*service.conf", Effect::Ask),
    ],
    &[
        rule("fs.create", LEGACY_JSON, Effect::Ask),
        rule("fs.modify", LEGACY_JSON, Effect::Ask),
        rule("fs.overwrite", LEGACY_JSON, Effect::Ask),
        rule("fs.move", LEGACY_JSON, Effect::Ask),
        rule("fs.delete", LEGACY_JSON, Effect::Ask),
        rule("edit", LEGACY_JSON, Effect::Ask),
    ],
    &[
        rule("fs.create", LEGACY_JSONC, Effect::Ask),
        rule("fs.modify", LEGACY_JSONC, Effect::Ask),
        rule("fs.overwrite", LEGACY_JSONC, Effect::Ask),
        rule("fs.move", LEGACY_JSONC, Effect::Ask),
        rule("fs.delete", LEGACY_JSONC, Effect::Ask),
        rule("edit", LEGACY_JSONC, Effect::Ask),
    ],
]);

/// Which authority a layer speaks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerName {
    /// An explicit preset selection, never a configurable rule source.
    Bypass,
    Owner,
    Preset,
    Config,
    Ancestor,
    Approval,
}

impl LayerName {
    /// How a refusal names the authority that refused.
    pub fn label(self) -> &'static str {
        match self {
            Self::Bypass => "bypass",
            Self::Owner => "owner",
            Self::Preset => "preset",
            Self::Config => "configured",
            Self::Ancestor => "ancestor",
            Self::Approval => "approval",
        }
    }
}

/// How a layer combines with the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compose {
    /// May only make the decision stricter.
    Narrow,
    /// May lift an `ask` to `allow`; never a deny, never the owner floor.
    Override,
}

/// One authority's rules, in its own order (last match wins).
#[derive(Debug, Clone)]
pub struct Layer {
    pub name: LayerName,
    pub rules: Vec<Rule>,
    /// Deny and ask are floors: nothing below may widen them.
    pub floor: bool,
    pub compose: Compose,
}

impl Layer {
    pub fn owner() -> Self {
        Self {
            name: LayerName::Owner,
            rules: OWNER_FLOOR.iter().map(|rule| rule.rule(None)).collect(),
            floor: true,
            compose: Compose::Narrow,
        }
    }

    /// A preset with no known workspace: every absolute path a destructive
    /// command check meets is outside it.
    pub fn preset(preset: Preset) -> Self {
        Self::preset_in(preset, None)
    }

    /// A preset for a Session working in `workspace`.
    pub fn preset_in(preset: Preset, workspace: Option<&str>) -> Self {
        Self {
            name: if preset == Preset::Bypass {
                LayerName::Bypass
            } else {
                LayerName::Preset
            },
            rules: preset
                .table()
                .iter()
                .map(|rule| rule.rule(workspace))
                .collect(),
            floor: false,
            compose: Compose::Narrow,
        }
    }

    /// A remembered concrete call matches exact resource bytes. This also
    /// applies to legacy saved approvals: their text carries no typed pattern
    /// intent, so interpreting metacharacters as syntax would widen consent.
    /// Configured rules and typed argv prefixes have their own pattern paths.
    pub fn approvals(action: &str, resources: &[String]) -> Self {
        Self {
            name: LayerName::Approval,
            rules: resources
                .iter()
                .map(|resource| Rule::literal(action, resource.as_str(), Effect::Allow))
                .collect(),
            floor: false,
            compose: Compose::Override,
        }
    }
}

/// What the stack decided, and which authority decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub effect: Effect,
    /// `None` when no layer had an opinion and the `ask` fallback applied.
    pub layer: Option<LayerName>,
}

/// The rule a producer's `edit` action used to cover still covers every
/// semantic filesystem action, so no older rule silently stops governing.
fn matches_action(action: &str, pattern: &str) -> bool {
    wildcard(pattern, action)
        || (pattern == "edit"
            && matches!(
                action,
                "fs.create" | "fs.modify" | "fs.overwrite" | "fs.delete" | "fs.move"
            ))
}

fn find_rule<'a>(action: &str, resource: &str, rules: &'a [Rule]) -> Option<&'a Rule> {
    rules.iter().rev().find(|rule| {
        matches_action(action, &rule.action)
            && match &rule.matcher {
                Matcher::Literal => rule.resource == resource,
                Matcher::Path => {
                    let expected =
                        grapt_types::path::comparison_key(&rule.resource.replace('\\', "/"));
                    let actual = grapt_types::path::comparison_key(&resource.replace('\\', "/"));
                    expected == actual
                }
                Matcher::Pattern if path_action(action) => wildcard(
                    &grapt_types::path::comparison_key(&rule.resource),
                    &grapt_types::path::comparison_key(resource),
                ),
                Matcher::Pattern => wildcard(&rule.resource, resource),
                Matcher::Destructive { workspace } => {
                    crate::command_safety::classify(resource, workspace.as_deref()).is_some()
                }
            }
    })
}

/// Path spelling is filesystem-specific; command and MCP resource bytes are not.
fn path_action(action: &str) -> bool {
    matches!(
        action,
        "read"
            | "read_lines"
            | "glob"
            | "grep"
            | "list"
            | "edit"
            | "fs.create"
            | "fs.modify"
            | "fs.overwrite"
            | "fs.delete"
            | "fs.move"
            | crate::SANDBOX_WRITE_ACTION
    )
}

/// Decide `action` on every resource. Floors' denies first; then the
/// strictest narrowing opinion; then overrides, unless that opinion is a deny
/// or the owner floor's; across resources the strictest result wins; with no
/// opinion anywhere, ask.
pub fn decide(action: &str, resources: &[&str], layers: &[Layer]) -> Decision {
    if bypasses(layers) {
        return Decision {
            effect: Effect::Allow,
            layer: Some(LayerName::Bypass),
        };
    }
    for layer in layers.iter().filter(|layer| layer.floor) {
        for resource in resources {
            if find_rule(action, resource, &layer.rules).map(|rule| rule.effect)
                == Some(Effect::Deny)
            {
                return Decision {
                    effect: Effect::Deny,
                    layer: Some(layer.name),
                };
            }
        }
    }
    let mut decided: Vec<(Effect, Option<LayerName>)> = Vec::new();
    for resource in resources {
        let mut opinion: Option<(Effect, LayerName)> = None;
        for layer in layers
            .iter()
            .filter(|layer| layer.compose == Compose::Narrow)
        {
            if let Some(found) = find_rule(action, resource, &layer.rules) {
                if opinion.is_none_or(|(effect, by)| {
                    found.effect > effect
                        || (found.effect == effect && layer.floor && by != LayerName::Owner)
                }) {
                    opinion = Some((found.effect, layer.name));
                }
            }
        }
        let locked = opinion.is_some_and(|(effect, _)| effect == Effect::Deny)
            || layers.iter().filter(|layer| layer.floor).any(|layer| {
                find_rule(action, resource, &layer.rules)
                    .is_some_and(|rule| rule.effect == Effect::Ask)
            });
        for layer in layers
            .iter()
            .filter(|layer| layer.compose == Compose::Override)
        {
            if let Some(found) = find_rule(action, resource, &layer.rules) {
                // A floor can prevent widening, never prevent a configured
                // deny from making its ask stricter.
                if found.effect == Effect::Deny || !locked {
                    opinion = Some((found.effect, layer.name));
                }
                if found.effect == Effect::Deny {
                    break;
                }
            }
        }
        decided.push(match opinion {
            Some((effect, by)) => (effect, Some(by)),
            None => (Effect::Ask, None),
        });
    }
    let (effect, layer) = decided
        .into_iter()
        .max_by_key(|(effect, _)| *effect)
        .unwrap_or((Effect::Ask, None));
    Decision { effect, layer }
}

/// Whether `action` is denied outright, before any resource is known: in
/// some layer, the last rule for the action denies every resource.
/// A tool whose actions are all withheld is never offered to the model (the
/// oracle's `withheldByAuthority`, `authority.ts`). One way only: withheld
/// implies every resource decides `deny`, but a tool that is not withheld may
/// still be denied for particular resources once a call names them.
pub fn withheld(action: &str, layers: &[Layer]) -> bool {
    if bypasses(layers) {
        return false;
    }
    layers.iter().any(|layer| {
        layer
            .rules
            .iter()
            .rev()
            .find(|rule| matches_action(action, &rule.action))
            .is_some_and(|rule| {
                rule.matcher == Matcher::Pattern
                    && rule.resource == "*"
                    && rule.effect == Effect::Deny
            })
    })
}

fn bypasses(layers: &[Layer]) -> bool {
    layers.iter().any(|layer| layer.name == LayerName::Bypass)
}

/// `*` matches any run (including none), `?` one character; `\` reads as
/// `/`. The whole input must match.
fn wildcard(pattern: &str, input: &str) -> bool {
    let pattern: Vec<char> = pattern.replace('\\', "/").chars().collect();
    let input: Vec<char> = input.replace('\\', "/").chars().collect();
    let (mut p, mut i) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < input.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == input[i]) {
            p += 1;
            i += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, i));
            p += 1;
        } else if let Some((at, from)) = star {
            p = at + 1;
            i = from + 1;
            star = Some((at, from + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
#[path = "preset/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "preset/seat02_tests.rs"]
mod seat02_tests;

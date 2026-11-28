//! Deterministic repair of a builtin call's parameter names and scalar types,
//! and bounded recovery hints when a call still does not fit its tool.
//!
//! Repairs run before plugin hooks, preparation and permission planning, so
//! every later check, authorization and sandbox decision sees the repaired
//! call. Only a builtin's own closed schema drives them: a key is renamed only
//! from a fixed alias to a declared parameter the call did not supply, and only
//! a string holding exactly an unsigned integer or `true`/`false` becomes that
//! integer or boolean where the schema asks for one. String values (paths,
//! commands, edit anchors) are never changed, and published MCP or plugin tools
//! are never touched.
use grapt_runtime::BoundedToolInput;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Parameter names models often send in place of a declared one.
const ALIASES: &[(&str, &str)] = &[
    ("file_path", "path"),
    ("filePath", "path"),
    ("filepath", "path"),
    ("old_string", "old_text"),
    ("oldText", "old_text"),
    ("new_string", "new_text"),
    ("newText", "new_text"),
    ("replaceAll", "replace_all"),
];
/// The most unknown parameters or suggested tools one hint names.
const MAX_LISTED: usize = 4;
/// Characters of a model-chosen name a hint repeats.
const MAX_NAME_CHARS: usize = 64;
/// The furthest edit distance a suggestion may be from what was asked.
const MAX_DISTANCE: usize = 2;

/// What a repair changed, in closed terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Repair {
    Alias,
    Coercion,
    AliasAndCoercion,
}

impl Repair {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::Coercion => "coercion",
            Self::AliasAndCoercion => "alias_and_coercion",
        }
    }
}

/// Why a call did not fit, in closed terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hint {
    UnknownTool,
    UnknownParameter,
    MissingParameter,
}

impl Hint {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::UnknownTool => "unknown_tool",
            Self::UnknownParameter => "unknown_parameter",
            Self::MissingParameter => "missing_parameter",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Integer,
    Boolean,
    Other,
}

/// A builtin's closed top-level parameters, read from its published schema.
#[derive(Debug)]
struct Fields {
    kinds: HashMap<String, Kind>,
    required: Vec<String>,
}

fn builtin(tool: &str) -> Option<&'static Fields> {
    static SCHEMAS: OnceLock<HashMap<String, Fields>> = OnceLock::new();
    if !super::catalog::BUILTIN_TOOL_NAMES.contains(&tool) {
        return None;
    }
    SCHEMAS
        .get_or_init(|| {
            super::catalog::definitions()
                .into_iter()
                .filter_map(|definition| {
                    fields(&definition.input_schema).map(|fields| (definition.name, fields))
                })
                .collect()
        })
        .get(tool)
}

/// Only an object schema that refuses unknown properties is used.
fn fields(schema: &str) -> Option<Fields> {
    let schema: Value = serde_json::from_str(schema).ok()?;
    if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        return None;
    }
    let kinds = schema
        .get("properties")?
        .as_object()?
        .iter()
        .map(|(name, property)| {
            let kind = match property.get("type").and_then(Value::as_str) {
                Some("integer") => Kind::Integer,
                Some("boolean") => Kind::Boolean,
                _ => Kind::Other,
            };
            (name.clone(), kind)
        })
        .collect();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(Fields { kinds, required })
}

/// The call with any safe repair applied, and what changed. A call that is not
/// a builtin's JSON object, or that needs no repair, comes back as it was.
pub(super) fn repair(tool: &str, input: BoundedToolInput) -> (BoundedToolInput, Option<Repair>) {
    let Some(fields) = builtin(tool) else {
        return (input, None);
    };
    let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(input.as_bytes()) else {
        return (input, None);
    };
    let repair = match (rename(&mut object, fields), coerce(&mut object, fields)) {
        (true, true) => Repair::AliasAndCoercion,
        (true, false) => Repair::Alias,
        (false, true) => Repair::Coercion,
        (false, false) => return (input, None),
    };
    match serde_json::to_vec(&Value::Object(object))
        .ok()
        .and_then(|bytes| BoundedToolInput::new(bytes).ok())
    {
        Some(repaired) => (repaired, Some(repair)),
        None => (input, None),
    }
}

/// Rename an alias to its declared parameter when only the alias was sent.
fn rename(object: &mut Map<String, Value>, fields: &Fields) -> bool {
    let mut renamed = false;
    for (alias, declared) in ALIASES {
        if fields.kinds.contains_key(*declared)
            && !fields.kinds.contains_key(*alias)
            && !object.contains_key(*declared)
        {
            if let Some(value) = object.remove(*alias) {
                object.insert((*declared).to_owned(), value);
                renamed = true;
            }
        }
    }
    renamed
}

/// Turn exact textual integers and booleans into the type the schema declares.
fn coerce(object: &mut Map<String, Value>, fields: &Fields) -> bool {
    let mut coerced = false;
    for (name, value) in object.iter_mut() {
        let Value::String(text) = value else {
            continue;
        };
        let replacement = match fields.kinds.get(name) {
            Some(Kind::Integer)
                if (1..=19).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit()) =>
            {
                text.parse::<u64>().ok().map(Value::from)
            }
            Some(Kind::Boolean) if text == "true" => Some(Value::Bool(true)),
            Some(Kind::Boolean) if text == "false" => Some(Value::Bool(false)),
            _ => None,
        };
        if let Some(replacement) = replacement {
            *value = replacement;
            coerced = true;
        }
    }
    coerced
}

/// Why a builtin call does not fit its parameters, naming the fix; `None` when
/// it fits or is not a builtin JSON object. No parameter value is repeated.
pub(super) fn parameter_hint(tool: &str, input: &BoundedToolInput) -> Option<(Hint, String)> {
    let fields = builtin(tool)?;
    let Value::Object(object) = serde_json::from_slice::<Value>(input.as_bytes()).ok()? else {
        return None;
    };
    let unknown: Vec<&String> = object
        .keys()
        .filter(|key| !fields.kinds.contains_key(*key))
        .collect();
    let missing: Vec<&String> = fields
        .required
        .iter()
        .filter(|name| !object.contains_key(*name))
        .collect();
    if unknown.is_empty() && missing.is_empty() {
        return None;
    }
    let mut expected: Vec<&str> = fields.kinds.keys().map(String::as_str).collect();
    expected.sort_unstable();
    let mut parts = Vec::new();
    for key in unknown.iter().take(MAX_LISTED) {
        let mut part = format!("unknown parameter `{}`", bounded(key));
        if let Some(declared) = suggest_parameter(key, &expected) {
            part.push_str(&format!(" (did you mean `{declared}`?)"));
        }
        parts.push(part);
    }
    if unknown.len() > MAX_LISTED {
        parts.push(format!(
            "{} more unknown parameters",
            unknown.len() - MAX_LISTED
        ));
    }
    parts.extend(
        missing
            .iter()
            .map(|name| format!("missing required parameter `{name}`")),
    );
    let hint = if unknown.is_empty() {
        Hint::MissingParameter
    } else {
        Hint::UnknownParameter
    };
    Some((
        hint,
        format!(
            "{}; {tool} takes: {}",
            parts.join("; "),
            expected.join(", ")
        ),
    ))
}

/// A suffix naming the builtins closest to an unknown tool name, or nothing.
pub(super) fn unknown_tool_hint(tool: &str) -> String {
    let wanted = tool.to_ascii_lowercase();
    let mut close: Vec<&str> = super::catalog::BUILTIN_TOOL_NAMES
        .iter()
        .copied()
        .filter(|name| distance(&wanted, name).is_some_and(|d| d <= MAX_DISTANCE))
        .take(MAX_LISTED)
        .collect();
    close.sort_unstable();
    if close.is_empty() {
        return String::new();
    }
    format!(" (did you mean `{}`?)", close.join("`, `"))
}

fn suggest_parameter<'a>(key: &str, expected: &[&'a str]) -> Option<&'a str> {
    if let Some((_, declared)) = ALIASES.iter().find(|(alias, _)| *alias == key) {
        return expected.iter().copied().find(|name| name == declared);
    }
    let wanted = key.to_ascii_lowercase();
    expected
        .iter()
        .copied()
        .filter_map(|name| distance(&wanted, name).map(|d| (d, name)))
        .filter(|(d, _)| *d <= MAX_DISTANCE)
        .min()
        .map(|(_, name)| name)
}

/// Levenshtein distance between two short names; `None` past the name bound.
fn distance(left: &str, right: &str) -> Option<usize> {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len() > MAX_NAME_CHARS || right.len() > MAX_NAME_CHARS {
        return None;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, l) in left.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, r) in right.iter().enumerate() {
            let substitute = previous[j] + usize::from(l != r);
            current.push(substitute.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous.last().copied()
}

/// A model-chosen name as a hint repeats it: printable and bounded.
fn bounded(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect()
}

#[cfg(test)]
mod tests;

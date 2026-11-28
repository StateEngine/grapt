//! Deterministic repair of a builtin call's parameter names and scalar types,
//! and bounded recovery hints when a call still does not fit its tool.
//!
//! Repairs run before plugin hooks, preparation and permission planning, so
//! every later check, authorization and sandbox decision sees the repaired
//! call. Only a builtin's own closed schema drives them: a key is renamed only
//! from a fixed alias to a declared parameter the call did not supply, and only
//! a string holding exactly an unsigned integer or `true`/`false` becomes that
//! integer or boolean where the schema asks for one. String values (paths,
//! commands, edit anchors) are never changed, published MCP or plugin tools
//! are never touched, and a builtin call that repeats a JSON key is refused
//! before anything reads it as a value, which would keep one repeat. Hints name only schema-owned parameters and builtin
//! tools, never a key or value the model chose.
use grapt_runtime::BoundedToolInput;
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
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
/// The most suggestions one hint names.
const MAX_LISTED: usize = 4;
/// The longest name a suggestion is computed for.
const MAX_NAME_CHARS: usize = 64;
/// The furthest edit distance a suggestion may be from what was asked.
const MAX_DISTANCE: usize = 2;
/// The most unknown keys compared against the schema for a suggestion.
const MAX_EXAMINED: usize = 16;

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
    RepeatedParameter,
}

impl Hint {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::UnknownTool => "unknown_tool",
            Self::UnknownParameter => "unknown_parameter",
            Self::MissingParameter => "missing_parameter",
            Self::RepeatedParameter => "repeated_parameter",
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
    let Parsed::Unique(Value::Object(mut object)) = parse(input.as_bytes()) else {
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

/// A call refused before preparation, and the hint its refusal shows.
#[derive(Debug)]
pub(super) struct Refusal {
    pub(super) hint: Hint,
    pub(super) detail: String,
}

/// Refuse a builtin call that repeats a JSON key anywhere, naming only the
/// tool's own parameters.
pub(super) fn repeated(tool: &str, input: &BoundedToolInput) -> Option<Refusal> {
    let fields = builtin(tool)?;
    matches!(parse(input.as_bytes()), Parsed::Repeated).then(|| Refusal {
        hint: Hint::RepeatedParameter,
        detail: format!(
            "tool input is invalid: a parameter is repeated; {tool} takes: {}",
            declared(fields).join(", ")
        ),
    })
}

/// Refuse, before preparation, a call that does not fit: a tool nothing
/// serves, when a builtin name is close enough to suggest, or a builtin call
/// whose parameters are unknown or missing. `None` leaves the call to the
/// ordinary path, which refuses any other unknown tool without a hint.
pub(super) fn screen(
    tool: &str,
    input: &BoundedToolInput,
    served: impl FnOnce() -> bool,
) -> Option<Refusal> {
    if !served() {
        let names = close_tools(tool);
        return (!names.is_empty()).then(|| Refusal {
            hint: Hint::UnknownTool,
            detail: format!(
                "provider requested an unknown tool (did you mean `{}`?)",
                names.join("`, `")
            ),
        });
    }
    parameter_refusal(tool, input)
}

/// Why a builtin call does not fit its parameters, naming the fix with
/// schema-owned names and counts only; `None` when it fits, is not a builtin
/// JSON object, or repeats a key ([`repeated`] refuses that).
fn parameter_refusal(tool: &str, input: &BoundedToolInput) -> Option<Refusal> {
    let fields = builtin(tool)?;
    let Parsed::Unique(Value::Object(object)) = parse(input.as_bytes()) else {
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
    let expected = declared(fields);
    let mut parts = Vec::new();
    if !unknown.is_empty() {
        let mut suggested: Vec<&str> = unknown
            .iter()
            .take(MAX_EXAMINED)
            .filter_map(|key| suggest_parameter(key, &expected))
            .filter(|name| !object.contains_key(*name))
            .collect();
        suggested.sort_unstable();
        suggested.dedup();
        suggested.truncate(MAX_LISTED);
        let plural = if unknown.len() == 1 { "" } else { "s" };
        let mut part = format!("{} unknown parameter{plural}", unknown.len());
        if !suggested.is_empty() {
            part.push_str(&format!(" (did you mean `{}`?)", suggested.join("`, `")));
        }
        parts.push(part);
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
    Some(Refusal {
        hint,
        detail: format!(
            "tool input is invalid: {}; {tool} takes: {}",
            parts.join("; "),
            expected.join(", ")
        ),
    })
}

/// A builtin's declared parameter names, sorted.
fn declared(fields: &Fields) -> Vec<&str> {
    let mut names: Vec<&str> = fields.kinds.keys().map(String::as_str).collect();
    names.sort_unstable();
    names
}

/// The builtin names closest to an unknown tool name.
fn close_tools(tool: &str) -> Vec<&'static str> {
    let wanted = tool.to_ascii_lowercase();
    let mut close: Vec<&str> = super::catalog::BUILTIN_TOOL_NAMES
        .iter()
        .copied()
        .filter(|name| distance(&wanted, name).is_some_and(|d| d <= MAX_DISTANCE))
        .take(MAX_LISTED)
        .collect();
    close.sort_unstable();
    close
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

/// What a call's bytes hold as JSON.
enum Parsed {
    Unique(Value),
    /// Some object repeats a key: any value read from it keeps one repeat.
    Repeated,
    Invalid,
}

fn parse(bytes: &[u8]) -> Parsed {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    match UniqueKeys.deserialize(&mut deserializer) {
        // Valid JSON fails the walk only on its repeated-key refusal.
        Err(error) if error.is_data() => return Parsed::Repeated,
        Err(_) => return Parsed::Invalid,
        Ok(()) => {}
    }
    if deserializer.end().is_err() {
        return Parsed::Invalid;
    }
    serde_json::from_slice(bytes).map_or(Parsed::Invalid, Parsed::Unique)
}

/// Walks a JSON document and refuses the first object that repeats a key.
struct UniqueKeys;

impl<'de> DeserializeSeed<'de> for UniqueKeys {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for UniqueKeys {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without repeated object keys")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(UniqueKeys)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key) {
                return Err(de::Error::custom("an object repeats a key"));
            }
            map.next_value_seed(UniqueKeys)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

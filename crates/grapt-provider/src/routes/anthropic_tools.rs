//! Anthropic's wire schema is a hint; the tool owner validates the original.
use serde_json::{json, Map, Value};

use crate::ToolDefinition;

const COMBINATORS: [&str; 3] = ["oneOf", "anyOf", "allOf"];
const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_PROJECTED_BYTES: usize = 256 * 1024;

pub(super) fn project(tool: &ToolDefinition) -> Result<Value, &'static str> {
    let schema: Value = serde_json::from_str(&tool.input_schema)
        .map_err(|_| "Anthropic tool schema is not JSON")?;
    let mut body = json!({"name":tool.name, "input_schema":schema});
    if let Some(description) = &tool.description {
        body["description"] = json!(description);
    }
    if !COMBINATORS.iter().any(|key| schema.get(key).is_some()) {
        return Ok(body);
    }
    if tool.input_schema.len() > MAX_SCHEMA_BYTES {
        return Err("Anthropic composite tool schema exceeds 64 KiB");
    }
    let mut properties = Map::new();
    let mut budget = Budget {
        visits: 4096,
        bytes: MAX_PROJECTED_BYTES,
        permissive: false,
    };
    let closed = collect(&schema, &schema, &mut properties, &mut budget, 0)?;
    let mut flat = schema
        .as_object()
        .cloned()
        .ok_or("Anthropic tool schema is not an object")?;
    for key in COMBINATORS {
        flat.remove(key);
    }
    flat.remove("$ref");
    flat.insert("type".into(), json!("object"));
    // Branch patterns and open alternatives can accept keys/values absent
    // from other branches. Their exact constraints remain in the original
    // schema; an Anthropic hint must not exclude those valid arguments.
    if budget.permissive {
        for (name, value) in &mut properties {
            // Root constraints apply in every branch and remain safe hints.
            *value = schema
                .get("properties")
                .and_then(|fields| fields.get(name))
                .cloned()
                .unwrap_or_else(|| json!({}));
        }
        if !flat.contains_key("additionalProperties") {
            flat.insert("additionalProperties".into(), json!(true));
        }
    }
    flat.insert("properties".into(), Value::Object(properties));
    // Only close the flattened envelope when every alternative is closed,
    // or an intersection member closes it. Original branch rules still decide
    // which combinations and additional-property values the tool may execute.
    if !budget.permissive && (closed || !flat.contains_key("additionalProperties")) {
        flat.insert("additionalProperties".into(), json!(!closed));
    }
    body["input_schema"] = Value::Object(flat);
    body["description"] = json!(format!(
        "{}{}Arguments must satisfy this original JSON Schema, validated locally before execution: {}",
        tool.description.as_deref().unwrap_or(""),
        if tool.description.is_some() { "\n" } else { "" },
        tool.input_schema,
    ));
    if body.to_string().len() > MAX_PROJECTED_BYTES {
        return Err("Anthropic tool schema projection exceeds 256 KiB");
    }
    Ok(body)
}

struct Budget {
    visits: usize,
    bytes: usize,
    permissive: bool,
}

/// Combinators constrain the same object; properties constrain its data.
/// The traversal follows only the former, including local branch references.
fn collect(
    schema: &Value,
    root: &Value,
    properties: &mut Map<String, Value>,
    budget: &mut Budget,
    depth: usize,
) -> Result<bool, &'static str> {
    if depth > 32 || budget.visits == 0 {
        return Err("Anthropic tool schema expansion exceeds its structural bound");
    }
    budget.visits -= 1;
    if let Some(value) = schema.as_bool() {
        return Ok(!value);
    }
    let object = schema
        .as_object()
        .ok_or("Anthropic schema branch is not an object")?;
    if object.get("patternProperties").is_some_and(|patterns| {
        patterns
            .as_object()
            .is_some_and(|patterns| !patterns.is_empty())
    }) {
        budget.permissive = true;
    }
    if let Some(fields) = object.get("properties") {
        for (name, value) in fields
            .as_object()
            .ok_or("Anthropic schema properties are not an object")?
        {
            let bytes = name.len().saturating_add(value.to_string().len());
            budget.bytes = budget
                .bytes
                .checked_sub(bytes)
                .ok_or("Anthropic tool schema expansion exceeds its byte bound")?;
            match properties.get_mut(name) {
                Some(previous) if previous != value => {
                    // A union is a permissive wire hint even for allOf. The
                    // original intersection remains the execution constraint.
                    if previous.as_object().is_some_and(|fields| fields.len() == 1) {
                        if let Some(alternatives) =
                            previous.get_mut("anyOf").and_then(Value::as_array_mut)
                        {
                            if !alternatives.contains(value) {
                                alternatives.push(value.clone());
                            }
                            continue;
                        }
                    }
                    *previous = json!({"anyOf":[previous, value]});
                }
                Some(_) => {}
                None => {
                    properties.insert(name.clone(), value.clone());
                }
            }
        }
    }
    let mut closed = object.get("additionalProperties") == Some(&Value::Bool(false));
    if let Some(reference) = object.get("$ref") {
        let pointer = reference
            .as_str()
            .and_then(|value| value.strip_prefix('#'))
            .filter(|value| value.is_empty() || value.starts_with('/'))
            .ok_or("Anthropic schema projection requires a local reference")?;
        let target = root
            .pointer(pointer)
            .ok_or("Anthropic schema reference is absent")?;
        closed |= collect(target, root, properties, budget, depth + 1)?;
    }
    for key in COMBINATORS {
        if let Some(branches) = object.get(key) {
            let branches = branches
                .as_array()
                .filter(|branches| !branches.is_empty())
                .ok_or("Anthropic schema combinator requires nonempty branches")?;
            let mut every_closed = true;
            let mut any_closed = false;
            for branch in branches {
                let branch_closed = collect(branch, root, properties, budget, depth + 1)?;
                every_closed &= branch_closed;
                any_closed |= branch_closed;
            }
            if key != "allOf" && !every_closed {
                budget.permissive = true;
            }
            closed |= if key == "allOf" {
                any_closed
            } else {
                every_closed
            };
        }
    }
    Ok(closed)
}

#[cfg(test)]
mod tests;

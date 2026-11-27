//! Lossless, bounded native Messages web-search history. Encrypted provider
//! fields remain opaque and are never converted to portable assistant prose.
use super::{AdapterState, ReplayBinding};
use grapt_types::ToolCallId;
use serde_json::{json, Value};

const MAX_NATIVE_BYTES: usize = 128 * 1024;
const MAX_RENDERED_BYTES: usize = 64 * 1024;
const MALFORMED: &str = "Anthropic native replay is malformed or exceeds its bound";

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Search {
    pub(super) call: ToolCallId,
    pub(super) item: Value,
    pub(super) bytes: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Text {
    pub(super) item: Value,
    pub(super) rendered: String,
    pub(super) bytes: usize,
}

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AnthropicSearch([REDACTED])")
    }
}

impl std::fmt::Debug for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AnthropicText([REDACTED])")
    }
}

// Check shape and aggregate scalar bytes before serializing or cloning. Opaque
// extensions share the same depth, node and byte bounds as recognized fields.
fn bounded(value: &Value) -> Result<usize, &'static str> {
    fn visit(
        value: &Value,
        depth: usize,
        nodes: &mut usize,
        bytes: &mut usize,
    ) -> Result<(), &'static str> {
        *nodes += 1;
        if depth > 16 || *nodes > 4096 {
            return Err(MALFORMED);
        }
        *bytes = bytes.saturating_add(match value {
            Value::String(text) => text.len(),
            _ => 4,
        });
        match value {
            Value::Array(items) => {
                if items.len() > 1024 {
                    return Err(MALFORMED);
                }
                for item in items {
                    visit(item, depth + 1, nodes, bytes)?;
                }
            }
            Value::Object(fields) => {
                if fields.len() > 128 {
                    return Err(MALFORMED);
                }
                for (key, item) in fields {
                    if key.len() > 256 {
                        return Err(MALFORMED);
                    }
                    *bytes = bytes.saturating_add(key.len());
                    visit(item, depth + 1, nodes, bytes)?;
                }
            }
            _ => {}
        }
        if *bytes > MAX_NATIVE_BYTES {
            return Err(MALFORMED);
        }
        Ok(())
    }
    visit(value, 0, &mut 0, &mut 0)?;
    let bytes = value.to_string().len();
    (bytes <= MAX_NATIVE_BYTES)
        .then_some(bytes)
        .ok_or(MALFORMED)
}

fn nonempty(value: &Value, maximum: usize) -> Result<&str, &'static str> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= maximum)
        .ok_or(MALFORMED)
}

fn url(value: &Value) -> Result<(), &'static str> {
    let raw = nonempty(value, 8192)?;
    let parsed = reqwest::Url::parse(raw).map_err(|_| MALFORMED)?;
    if raw.chars().any(char::is_control)
        || !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(MALFORMED);
    }
    Ok(())
}

impl Search {
    pub(super) fn restore(server_use: &Value, result: &Value) -> Result<Self, &'static str> {
        let use_bytes = bounded(server_use)?;
        let result_bytes = bounded(result)?;
        if server_use["type"] != "server_tool_use"
            || server_use["name"] != "web_search"
            || !server_use["input"].is_object()
            || result["type"] != "web_search_tool_result"
        {
            return Err(MALFORMED);
        }
        // Dynamic/programmatic callers are not admitted by the basic tool.
        for block in [server_use, result] {
            if block
                .get("caller")
                .is_some_and(|caller| caller["type"] != "direct")
            {
                return Err(MALFORMED);
            }
        }
        nonempty(&server_use["input"]["query"], 4096)?;
        let id = nonempty(&server_use["id"], 256)?;
        let call = ToolCallId::new(id).ok_or(MALFORMED)?;
        if result["tool_use_id"].as_str() != Some(call.as_str()) {
            return Err(MALFORMED);
        }
        match &result["content"] {
            Value::Array(results) => {
                for result in results {
                    if result["type"] != "web_search_result"
                        || !result["title"].is_string()
                        || result
                            .get("page_age")
                            .is_some_and(|age| !age.is_null() && !age.is_string())
                    {
                        return Err(MALFORMED);
                    }
                    url(&result["url"])?;
                    nonempty(&result["encrypted_content"], MAX_NATIVE_BYTES)?;
                }
            }
            Value::Object(error)
                if error.get("type").and_then(Value::as_str)
                    == Some("web_search_tool_result_error") =>
            {
                nonempty(&result["content"]["error_code"], 256)?;
            }
            _ => return Err(MALFORMED),
        }
        let bytes = use_bytes.saturating_add(result_bytes).saturating_add(3);
        if bytes > MAX_NATIVE_BYTES {
            return Err(MALFORMED);
        }
        Ok(Self {
            call,
            item: json!([server_use, result]),
            bytes,
        })
    }
}

impl Text {
    pub(super) fn restore(block: &Value, rendered: &str) -> Result<Self, &'static str> {
        let native_bytes = bounded(block)?;
        if block["type"] != "text"
            || !block["text"].is_string()
            || rendered.len() > MAX_RENDERED_BYTES
        {
            return Err(MALFORMED);
        }
        if let Some(citations) = block.get("citations").filter(|value| !value.is_null()) {
            for citation in citations.as_array().ok_or(MALFORMED)? {
                if citation["type"] != "web_search_result_location"
                    || !citation["title"].is_string()
                    || !citation["cited_text"].is_string()
                {
                    return Err(MALFORMED);
                }
                url(&citation["url"])?;
                nonempty(&citation["encrypted_index"], MAX_NATIVE_BYTES)?;
            }
        }
        Ok(Self {
            item: block.clone(),
            rendered: rendered.into(),
            bytes: native_bytes.saturating_add(rendered.len()),
        })
    }
}

pub(crate) fn anthropic_search_state(
    server_use: &Value,
    result: &Value,
    binding: &ReplayBinding,
) -> Result<AdapterState, &'static str> {
    let search = Search::restore(server_use, result)?;
    AdapterState::retain(
        json!({"kind":"anthropic-web-search-v1","item":search.item,"binding":binding.0})
            .to_string(),
    )
    .map_err(|_| MALFORMED)
}

pub(crate) fn anthropic_text_state(
    block: &Value,
    rendered: &str,
    binding: &ReplayBinding,
) -> Result<AdapterState, &'static str> {
    let text = Text::restore(block, rendered)?;
    AdapterState::retain(
        json!({"kind":"anthropic-text-v1","item":text.item,"rendered":text.rendered,"binding":binding.0})
            .to_string(),
    )
    .map_err(|_| MALFORMED)
}

#[cfg(test)]
mod tests;

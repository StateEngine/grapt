// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bound terminal native history, retain search sources,
// keep provider-native calls distinct from local functions, and redact Debug.
// (ResponseItem::WebSearchCall, WebSearchAction),
// commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.

use grapt_types::ToolCallId;
use serde_json::{json, Map, Value};

pub const MAX_SEARCH_ITEM_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("native web search history is malformed, unfinished, or exceeds its bound")]
pub struct SearchItemError;

#[derive(Clone, PartialEq, Eq)]
pub struct SearchHistoryItem {
    call: ToolCallId,
    item: Value,
    bytes: usize,
}

impl std::fmt::Debug for SearchHistoryItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SearchHistoryItem([REDACTED])")
    }
}

fn text(value: &Value, maximum: usize) -> Result<&str, SearchItemError> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= maximum)
        .ok_or(SearchItemError)
}

fn optional_text(
    value: &Value,
    name: &str,
    maximum: usize,
    out: &mut Map<String, Value>,
) -> Result<(), SearchItemError> {
    match value.get(name) {
        None | Some(Value::Null) => {}
        Some(value) => {
            out.insert(name.into(), text(value, maximum)?.into());
        }
    }
    Ok(())
}

fn url(value: &Value) -> Result<&str, SearchItemError> {
    let raw = text(value, 8192)?;
    if raw.chars().any(char::is_control) {
        return Err(SearchItemError);
    }
    let parsed = reqwest::Url::parse(raw).map_err(|_| SearchItemError)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(SearchItemError);
    }
    Ok(raw)
}

fn optional_url(value: &Value, out: &mut Map<String, Value>) -> Result<(), SearchItemError> {
    match value.get("url") {
        None | Some(Value::Null) => {}
        Some(value) => {
            out.insert("url".into(), url(value)?.into());
        }
    }
    Ok(())
}

fn action(value: &Value) -> Result<Value, SearchItemError> {
    let kind = text(&value["type"], 32)?;
    let mut out = Map::from_iter([("type".to_owned(), Value::String(kind.into()))]);
    match kind {
        "search" => {
            optional_text(value, "query", 4096, &mut out)?;
            if let Some(queries) = value.get("queries").filter(|value| !value.is_null()) {
                let queries = queries
                    .as_array()
                    .filter(|queries| queries.len() <= 64)
                    .ok_or(SearchItemError)?;
                let queries = queries
                    .iter()
                    .map(|query| text(query, 4096).map(Value::from))
                    .collect::<Result<Vec<_>, _>>()?;
                out.insert("queries".into(), Value::Array(queries));
            }
            if let Some(sources) = value.get("sources").filter(|value| !value.is_null()) {
                let sources = sources
                    .as_array()
                    .filter(|sources| sources.len() <= 1024)
                    .ok_or(SearchItemError)?;
                let sources = sources
                    .iter()
                    .map(|source| {
                        if source["type"] != "url" {
                            return Err(SearchItemError);
                        }
                        Ok(json!({"type":"url", "url":url(&source["url"])?}))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                out.insert("sources".into(), Value::Array(sources));
            }
        }
        "open_page" => {
            optional_url(value, &mut out)?;
        }
        "find_in_page" => {
            out.insert("url".into(), url(&value["url"])?.into());
            out.insert("pattern".into(), text(&value["pattern"], 4096)?.into());
        }
        _ => return Err(SearchItemError),
    }
    Ok(Value::Object(out))
}

impl SearchHistoryItem {
    /// Call only after the frame owner has bounded and parsed the provider
    /// frame. This reduces it to the fields accepted back as native history.
    pub fn from_output(item: &Value) -> Result<Self, SearchItemError> {
        if item["type"] != "web_search_call" {
            return Err(SearchItemError);
        }
        let id = text(&item["id"], 256)?;
        let status = text(&item["status"], 32)?;
        if !matches!(status, "completed" | "failed" | "incomplete") {
            return Err(SearchItemError);
        }
        let call = ToolCallId::new(id).ok_or(SearchItemError)?;
        let mut canonical = json!({"type":"web_search_call", "id":id, "status":status});
        if let Some(value) = item.get("action").filter(|value| !value.is_null()) {
            canonical["action"] = action(value)?;
        }
        // Codex's private backend returns this opaque field. It has no local
        // interpretation or permission effect and shares the whole-item cap.
        if let Some(metadata) = item.get("internal_chat_message_metadata_passthrough") {
            if metadata.to_string().len() > MAX_SEARCH_ITEM_BYTES {
                return Err(SearchItemError);
            }
            canonical["internal_chat_message_metadata_passthrough"] = metadata.clone();
        }
        let bytes = canonical.to_string().len();
        if bytes > MAX_SEARCH_ITEM_BYTES {
            return Err(SearchItemError);
        }
        Ok(Self {
            call,
            item: canonical,
            bytes,
        })
    }
    pub fn call(&self) -> &ToolCallId {
        &self.call
    }
    pub fn item(&self) -> &Value {
        &self.item
    }
    pub fn byte_len(&self) -> usize {
        self.bytes
    }
    pub fn failed(&self) -> bool {
        self.item["status"] != "completed"
    }

    /// Portable recorded data when a native item cannot cross its generating
    /// account/route/model binding. Never a callable function exchange.
    /// It costs no more context bytes than the original native item.
    pub fn fallback_text(&self) -> String {
        let text = format!(
            "Recorded provider-hosted web search ({}): {}",
            self.item["status"].as_str().unwrap_or("unknown"),
            self.item.get("action").unwrap_or(&Value::Null)
        );
        if text.len() <= self.bytes {
            text
        } else {
            // Opaque backend metadata is redeemable only under its exact
            // binding; it must never enter portable assistant text.
            json!({"web_search":self.item.get("action").unwrap_or(&Value::Null),
                "status":self.item["status"]})
            .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_item_retains_action_sources_and_has_a_bounded_non_callable_fallback(
    ) -> Result<(), SearchItemError> {
        let wire = json!({"type":"web_search_call", "id":"ws-fixture", "status":"completed",
            "action":{"type":"search", "queries":["fixture search"], "sources":[{"type":"url","url":"https://example.com/Case?token=secret#anchor"}]},
            "unrecognized_future_field":true});
        let item = SearchHistoryItem::from_output(&wire)?;
        assert_eq!(item.call().as_str(), "ws-fixture");
        assert_eq!(item.item()["action"], wire["action"]);
        assert!(item.item().get("unrecognized_future_field").is_none());
        assert!(!item.failed());
        assert!(item.fallback_text().len() <= item.byte_len());
        assert!(item
            .fallback_text()
            .contains("example.com/Case?token=secret#anchor"));
        assert!(!format!("{item:?}").contains("secret"));
        Ok(())
    }
    #[test]
    fn failed_incomplete_and_page_actions_remain_distinct() -> Result<(), SearchItemError> {
        for status in ["failed", "incomplete"] {
            let item = SearchHistoryItem::from_output(
                &json!({"type":"web_search_call", "id":"ws-fail", "status":status}),
            )?;
            assert!(item.failed());
            assert!(item.fallback_text().contains(status));
        }
        for action in [
            json!({"type":"open_page","url":"https://example.com/page"}),
            json!({"type":"find_in_page","url":"https://example.com/page","pattern":"needle"}),
        ] {
            let item = SearchHistoryItem::from_output(
                &json!({"type":"web_search_call","id":"ws-action","status":"completed","action":action}),
            )?;
            assert_eq!(item.item()["action"], action);
        }
        Ok(())
    }
    #[test]
    fn unfinished_unsafe_or_over_bound_native_items_are_refused() {
        let mut wire = json!({"type":"web_search_call", "id":"ws-fixture", "status":"searching"});
        assert!(SearchHistoryItem::from_output(&wire).is_err());
        wire["status"] = "completed".into();
        for action in [
            json!({"type":"unknown"}),
            json!({"type":"open_page", "url":"javascript:bad"}),
            json!({"type":"search", "queries":["x".repeat(4097)]}),
            json!({"type":"search", "sources":[{"type":"url","url":"https://u:p@example.com"}]}),
        ] {
            wire["action"] = action;
            assert!(SearchHistoryItem::from_output(&wire).is_err());
        }
        wire["action"] = json!({"type":"search", "queries":vec!["x".repeat(4096);64]});
        assert!(SearchHistoryItem::from_output(&wire).is_err());
    }
}

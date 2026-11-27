//! Basic Anthropic server search: bounded calls and lossless native blocks.
//! Native search is not a local function; missing results are never repaired.
//! commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d, Apache-2.0.
//! Copyright 2025 OpenAI. Modified by Grapt for Anthropic paired blocks.

use crate::fragment::{AdapterState, ToolExecution};
use crate::replay::ReplayBinding;
use crate::transport::sink::DecodeSink;
use grapt_types::ToolCallId;
use serde_json::{json, Value};

const MAX_BYTES: usize = crate::native_search::MAX_SEARCH_ITEM_BYTES;

#[derive(Default)]
pub(super) struct SearchTurn {
    maximum: Option<u8>,
    calls: Vec<ToolCallId>,
    pending: Option<Value>,
    input: String,
    use_open: bool,
}

impl std::fmt::Debug for SearchTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchTurn")
            .field("maximum", &self.maximum)
            .field("calls", &self.calls.len())
            .field("pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}

impl SearchTurn {
    pub(super) fn enabled(&self) -> bool {
        self.maximum.is_some()
    }
    pub(super) fn configure(&mut self, maximum: Option<u8>) {
        self.maximum = maximum;
    }
    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn permits_block(&self, kind: &str) -> bool {
        !self.pending() || (!self.use_open && kind == "web_search_tool_result")
    }
    pub(super) fn open_use(
        &mut self,
        block: &Value,
        key: &str,
        sink: &mut DecodeSink<'_>,
    ) -> Result<(), &'static str> {
        let maximum = self.maximum.ok_or("unadvertised Anthropic server search")?;
        if self.pending() || self.calls.len() >= usize::from(maximum) {
            return Err("Anthropic server search exceeds its admitted use bound");
        }
        if block["name"] != "web_search"
            || block
                .get("caller")
                .is_some_and(|caller| caller["type"] != "direct")
        {
            return Err("unsupported Anthropic server tool or caller");
        }
        let call = block["id"]
            .as_str()
            .and_then(ToolCallId::new)
            .ok_or("Anthropic server search has no valid call id")?;
        if self.calls.contains(&call) {
            return Err("Anthropic server search repeated a call id");
        }
        if block.to_string().len() > MAX_BYTES
            || block.get("input").is_some_and(|input| !input.is_object())
        {
            return Err("Anthropic server search input is malformed or oversized");
        }
        self.pending = Some(block.clone());
        self.input.clear();
        self.use_open = true;
        self.calls.push(call.clone());
        sink.tool_input_opened(key, call, "web_search", ToolExecution::ProviderHosted);
        Ok(())
    }
    pub(super) fn input_delta(&mut self, delta: &str) -> Result<(), &'static str> {
        if !self.use_open || self.input.len().saturating_add(delta.len()) > MAX_BYTES {
            return Err("Anthropic server search input exceeds its bound");
        }
        self.input.push_str(delta);
        Ok(())
    }
    pub(super) fn close_use(
        &mut self,
        key: &str,
        sink: &mut DecodeSink<'_>,
    ) -> Result<(), &'static str> {
        let block = self
            .pending
            .as_mut()
            .ok_or("server search closed without opening")?;
        let initial = block
            .get("input")
            .filter(|input| input.as_object().is_some_and(|v| !v.is_empty()));
        let input = if self.input.is_empty() {
            initial.cloned().unwrap_or_else(|| json!({}))
        } else {
            let parsed: Value = serde_json::from_str(&self.input)
                .map_err(|_| "Anthropic server search arguments are not JSON")?;
            if !parsed.is_object() || initial.is_some_and(|initial| initial != &parsed) {
                return Err("Anthropic server search arguments disagree with its initial input");
            }
            parsed
        };
        let arguments = if self.input.is_empty() {
            input.to_string()
        } else {
            self.input.clone()
        };
        if !sink.tool_input_final(key, Some(&arguments)) || !sink.tool_input_closed(key) {
            return Err("Anthropic server search input could not be closed");
        }
        block["input"] = input;
        self.use_open = false;
        Ok(())
    }
    pub(super) fn result(&self, block: &Value) -> Result<(), &'static str> {
        let pending = self
            .pending
            .as_ref()
            .ok_or("search result has no pending server call")?;
        if self.use_open || block["tool_use_id"] != pending["id"] {
            return Err("Anthropic search result does not match its closed call");
        }
        if block.to_string().len() > MAX_BYTES {
            return Err("Anthropic search result exceeds its byte bound");
        }
        Ok(())
    }
    pub(super) fn close_result(
        &mut self,
        result: Value,
        binding: &ReplayBinding,
        sink: &mut DecodeSink<'_>,
    ) -> Result<(), &'static str> {
        self.result(&result)?;
        let server = self.pending.as_ref().ok_or("search result has no call")?;
        let state = crate::replay::anthropic_search_state(server, &result, binding)?;
        let call = server["id"]
            .as_str()
            .and_then(ToolCallId::new)
            .ok_or("invalid server call")?;
        let content = &result["content"];
        let display = if let Some(results) = content.as_array() {
            // Encrypted content stays in replay only, never in rendered tool output.
            let sources = results
                .iter()
                .map(|item| {
                    json!({
                        "url":item["url"], "title":item["title"], "page_age":item["page_age"]
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({"sources":sources}).to_string())
        } else {
            let code = content["error_code"]
                .as_str()
                .ok_or("invalid server search error")?;
            let reason = match code {
                "too_many_requests" | "invalid_tool_input" | "max_uses_exceeded"
                | "query_too_long" | "request_too_large" | "unavailable" => code,
                _ => "unrecognized provider search error",
            };
            Err(format!("Anthropic web search failed: {reason}"))
        };
        sink.hosted_tool_terminal(call, "web_search", display, state);
        self.pending = None;
        Ok(())
    }
}

pub(super) struct SearchText {
    block: Value,
    rendered: String,
    citations: crate::citations::CitationTracker,
}
impl std::fmt::Debug for SearchText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SearchText([REDACTED])")
    }
}
impl SearchText {
    pub(super) fn new(block: &Value, sink: &mut DecodeSink<'_>) -> Result<Self, &'static str> {
        let text = block["text"]
            .as_str()
            .ok_or("Anthropic text is malformed")?;
        if block.to_string().len() > MAX_BYTES {
            return Err("Anthropic text replay exceeds its byte bound");
        }
        let mut held = Self {
            block: block.clone(),
            rendered: text.to_owned(),
            citations: crate::citations::CitationTracker::default(),
        };
        if !text.is_empty() {
            sink.text_delta(text);
        }
        if let Some(citations) = block.get("citations").filter(|value| !value.is_null()) {
            let citations = citations
                .as_array()
                .filter(|items| items.len() <= 128)
                .ok_or("Anthropic text citations are malformed or oversized")?;
            for citation in citations {
                held.render_citation(citation, sink)?;
            }
        }
        Ok(held)
    }
    pub(super) fn delta(
        &mut self,
        delta: &Value,
        sink: &mut DecodeSink<'_>,
    ) -> Result<(), &'static str> {
        match delta["type"].as_str() {
            Some("text_delta") => {
                let text = delta["text"]
                    .as_str()
                    .ok_or("Anthropic text delta is malformed")?;
                let Some(Value::String(raw)) = self.block.get_mut("text") else {
                    return Err("Anthropic text is malformed");
                };
                if raw.len().saturating_add(text.len()) > MAX_BYTES
                    || self.rendered.len().saturating_add(text.len()) > MAX_BYTES
                {
                    return Err("Anthropic text replay exceeds its byte bound");
                }
                raw.push_str(text);
                self.rendered.push_str(text);
                sink.text_delta(text);
            }
            Some("citations_delta") => {
                let citation = delta
                    .get("citation")
                    .ok_or("Anthropic citation delta is missing")?;
                if self.block.get("citations").is_none_or(Value::is_null) {
                    self.block["citations"] = json!([]);
                }
                let citations = self.block["citations"]
                    .as_array_mut()
                    .ok_or("invalid citations")?;
                if citations.len() >= 128 {
                    return Err("Anthropic citations exceed their bound");
                }
                citations.push(citation.clone());
                if self.block.to_string().len() > MAX_BYTES {
                    return Err("Anthropic citation replay exceeds its byte bound");
                }
                self.render_citation(citation, sink)?;
            }
            _ => return Err("unsupported Anthropic text delta"),
        }
        Ok(())
    }
    fn render_citation(
        &mut self,
        citation: &Value,
        sink: &mut DecodeSink<'_>,
    ) -> Result<(), &'static str> {
        if citation["type"] != "web_search_result_location" {
            return Err("unsupported Anthropic citation type");
        }
        // These coordinates key this rendered occurrence, not a vendor
        // citation offset. Repeated sources at later claims still need links.
        let annotation = json!({"type":"url_citation", "url":citation["url"],
            "title":citation["title"], "start_index":self.rendered.len(), "end_index":self.rendered.len()});
        let link = self
            .citations
            .append("anthropic", 0, &annotation)
            .map_err(|_| "Anthropic citation is malformed or oversized")?;
        if let Some(link) = link {
            if self.rendered.len().saturating_add(link.len()) > MAX_BYTES {
                return Err("Anthropic cited text exceeds its render bound");
            }
            self.rendered.push_str(&link);
            sink.text_delta(&link);
        }
        Ok(())
    }
    pub(super) fn finish(self, binding: &ReplayBinding) -> Result<AdapterState, &'static str> {
        if self
            .block
            .get("citations")
            .and_then(Value::as_array)
            .is_none_or(|items| items.is_empty())
        {
            return Ok(AdapterState::none());
        }
        crate::replay::anthropic_text_state(&self.block, &self.rendered, binding)
    }
}

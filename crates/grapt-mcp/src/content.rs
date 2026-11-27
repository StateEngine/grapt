// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// . Modified by Grapt: pure bounded
// prompt/resource pages, complete publication and duplicate identity refusal.
//! User-selected MCP content. Transport and user-turn admission stay outside.
use crate::{encode, McpBounds, McpProtocolError, OutboundRequest, RequestSequence};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogKind {
    Prompts,
    Resources,
    Templates,
}
impl CatalogKind {
    pub fn method(self) -> &'static str {
        match self {
            Self::Prompts => "prompts/list",
            Self::Resources => "resources/list",
            Self::Templates => "resources/templates/list",
        }
    }
    fn field(self) -> &'static str {
        match self {
            Self::Prompts => "prompts",
            Self::Resources => "resources",
            Self::Templates => "resourceTemplates",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Prompts => "name",
            Self::Resources => "uri",
            Self::Templates => "uriTemplate",
        }
    }
}

/// Only validated descriptor fields survive this protocol projection.
#[derive(Debug)]
pub struct ContentCatalog {
    kind: CatalogKind,
    bounds: McpBounds,
    rows: Vec<Value>,
    identities: BTreeSet<String>,
    cursors: BTreeSet<String>,
    cursor: Option<String>,
    pages: usize,
    bytes: usize,
    complete: bool,
}
impl ContentCatalog {
    pub fn new(kind: CatalogKind, bounds: McpBounds) -> Self {
        Self {
            kind,
            bounds,
            rows: Vec::new(),
            identities: BTreeSet::new(),
            cursors: BTreeSet::new(),
            cursor: None,
            pages: 0,
            bytes: 0,
            complete: false,
        }
    }
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
    pub fn accept_page(&mut self, result: &Value) -> Result<bool, McpProtocolError> {
        if self.complete || self.pages >= 100 {
            return Err(invalid("MCP content pagination exceeded 100 pages"));
        }
        let bytes = self.bytes.saturating_add(result.to_string().len());
        if bytes > self.bounds.max_frame_bytes() {
            return Err(invalid("MCP content catalog exceeds its byte bound"));
        }
        let page = result[self.kind.field()]
            .as_array()
            .ok_or_else(|| invalid("MCP content page has no descriptor array"))?;
        if self.rows.len().saturating_add(page.len()) > self.bounds.max_tools().min(256) {
            return Err(invalid("MCP content catalog exceeds its item bound"));
        }
        let cursor = match result.get("nextCursor") {
            None => None,
            Some(value) => Some(text(value, 1024, "pagination cursor")?.to_owned()),
        };
        if let Some(cursor) = &cursor {
            if self.pages == 99 || !self.cursors.insert(cursor.clone()) {
                return Err(invalid(
                    "MCP content cursor repeats or exceeds the page bound",
                ));
            }
        }
        for row in page {
            let row = descriptor(self.kind, row)?;
            let key = text(&row[self.kind.key()], 4096, "descriptor identity")?;
            if !self.identities.insert(key.to_owned()) {
                return Err(invalid("MCP content catalog repeats a descriptor identity"));
            }
            self.rows.push(row);
        }
        self.pages += 1;
        self.bytes = bytes;
        self.complete = cursor.is_none();
        self.cursor = cursor;
        Ok(self.complete)
    }
    pub fn finish(self) -> Result<Vec<Value>, McpProtocolError> {
        if !self.complete {
            return Err(invalid("MCP content catalog is incomplete"));
        }
        Ok(self.rows)
    }
}

pub fn list_request(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    kind: CatalogKind,
    cursor: Option<&str>,
) -> Result<OutboundRequest, McpProtocolError> {
    let params = match cursor {
        Some(cursor) => json!({"cursor": text(&json!(cursor), 1024, "pagination cursor")?}),
        None => json!({}),
    };
    encode(sequence, bounds, kind.method(), params)
}

pub fn prompt_request(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    name: &str,
    arguments: &BTreeMap<String, String>,
) -> Result<OutboundRequest, McpProtocolError> {
    text(&json!(name), 128, "prompt name")?;
    if arguments.len() > 32
        || arguments
            .iter()
            .map(|(k, v)| k.len().saturating_add(v.len()))
            .sum::<usize>()
            > 65_536
    {
        return Err(invalid("MCP prompt arguments exceed their bound"));
    }
    for key in arguments.keys() {
        text(&json!(key), 128, "prompt argument name")?;
    }
    encode(
        sequence,
        bounds,
        "prompts/get",
        json!({"name":name,"arguments":arguments}),
    )
}

pub fn resource_request(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    uri: &str,
) -> Result<OutboundRequest, McpProtocolError> {
    text(&json!(uri), 4096, "resource URI")?;
    encode(sequence, bounds, "resources/read", json!({"uri":uri}))
}

/// Subscription messages use the same validated resource identity as reads.
pub fn subscription_request(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    uri: &str,
    subscribe: bool,
) -> Result<OutboundRequest, McpProtocolError> {
    text(&json!(uri), 4096, "resource URI")?;
    encode(
        sequence,
        bounds,
        if subscribe {
            "resources/subscribe"
        } else {
            "resources/unsubscribe"
        },
        json!({"uri":uri}),
    )
}

fn descriptor(kind: CatalogKind, row: &Value) -> Result<Value, McpProtocolError> {
    let name = text(&row["name"], 128, "content name")?;
    let mut result = json!({"name":name});
    if let Some(description) = row.get("description") {
        result["description"] = json!(text(description, 1024, "content description")?);
    }
    if kind == CatalogKind::Prompts {
        let mut arguments = Vec::new();
        let mut names = BTreeSet::new();
        if let Some(declared) = row.get("arguments") {
            let declared = declared
                .as_array()
                .filter(|rows| rows.len() <= 32)
                .ok_or_else(|| invalid("MCP prompt argument declarations exceed their bound"))?;
            for argument in declared {
                let name = text(&argument["name"], 128, "prompt argument name")?;
                if !names.insert(name) {
                    return Err(invalid("MCP prompt repeats an argument name"));
                }
                let required = match argument.get("required") {
                    None => false,
                    Some(value) => value
                        .as_bool()
                        .ok_or_else(|| invalid("MCP prompt required flag is not boolean"))?,
                };
                let mut projected = json!({"name":name,"required":required});
                if let Some(description) = argument.get("description") {
                    projected["description"] =
                        json!(text(description, 1024, "argument description")?);
                }
                arguments.push(projected);
            }
        }
        result["arguments"] = json!(arguments);
    } else {
        let field = kind.key();
        result[field] = json!(text(&row[field], 4096, "resource URI")?);
        if let Some(mime) = row.get("mimeType") {
            result["mimeType"] = json!(text(mime, 128, "resource media type")?);
        }
    }
    Ok(result)
}

fn text<'a>(value: &'a Value, limit: usize, what: &str) -> Result<&'a str, McpProtocolError> {
    value
        .as_str()
        .filter(|text| {
            (!text.is_empty() || what.contains("description"))
                && text.len() <= limit
                && !text.chars().any(|c| {
                    c.is_control()
                        && !(what.contains("description") && matches!(c, '\n' | '\r' | '\t'))
                })
        })
        .ok_or_else(|| {
            invalid(&format!(
                "MCP {what} is missing, invalid or exceeds {limit} bytes"
            ))
        })
}
fn invalid(message: &str) -> McpProtocolError {
    McpProtocolError::Malformed(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paged_content_is_complete_and_repeated_cursors_or_arguments_refuse(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bounds = McpBounds::conservative()?;
        let mut catalog = ContentCatalog::new(CatalogKind::Prompts, bounds);
        assert!(!catalog.accept_page(&json!({"prompts":[{"name":"review","arguments":[{"name":"code","required":true}]}],"nextCursor":"second"}))?);
        assert_eq!(catalog.cursor(), Some("second"));
        assert!(catalog
            .accept_page(&json!({"prompts":[],"nextCursor":"second"}))
            .is_err());
        assert!(catalog.finish().is_err());
        let mut catalog = ContentCatalog::new(CatalogKind::Resources, bounds);
        assert!(catalog.accept_page(&json!({"resources":[{"name":"guide","uri":"memo://guide"}]}))?);
        assert_eq!(catalog.finish()?.len(), 1);
        let mut catalog = ContentCatalog::new(CatalogKind::Prompts, bounds);
        assert!(catalog.accept_page(&json!({"prompts":[{"name":"review","arguments":[{"name":"code"},{"name":"code"}]}]})).is_err());
        Ok(())
    }
}

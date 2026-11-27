//! Collect a complete tools catalog without publishing partial pages.
use crate::{accept_tools, DiscoveredTool, McpBounds, McpProtocolError};
use serde_json::Value;
use std::collections::BTreeSet;

/// One discovery uses at most max_tools pages and max_frame_bytes aggregate
/// result bytes. Empty pages therefore consume budget too. Transport owns the
/// single catalog deadline; page arrival never renews it.
#[derive(Debug)]
pub struct ToolCatalog {
    bounds: McpBounds,
    tools: Vec<DiscoveredTool>,
    cursors: BTreeSet<String>,
    cursor: Option<String>,
    pages: usize,
    bytes: usize,
    complete: bool,
}

impl ToolCatalog {
    pub fn new(bounds: McpBounds) -> Self {
        Self {
            bounds,
            tools: Vec::new(),
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
        if self.complete || self.pages >= self.bounds.max_tools() {
            return Err(McpProtocolError::TooManyToolPages {
                limit: self.bounds.max_tools(),
            });
        }
        let bytes = self.bytes.saturating_add(result.to_string().len());
        if bytes > self.bounds.max_frame_bytes() {
            return Err(McpProtocolError::ToolCatalogTooLarge {
                limit: self.bounds.max_frame_bytes(),
            });
        }
        let cursor = match result.get("nextCursor") {
            None => None,
            Some(Value::String(cursor)) if !cursor.is_empty() && cursor.len() <= 1024 => {
                if self.cursors.contains(cursor) {
                    return Err(McpProtocolError::RepeatedToolsCursor);
                }
                Some(cursor.clone())
            }
            Some(_) => return Err(McpProtocolError::InvalidToolsCursor),
        };
        // Refuse before issuing a request that cannot fit the page budget.
        if cursor.is_some() && self.pages + 1 >= self.bounds.max_tools() {
            return Err(McpProtocolError::TooManyToolPages {
                limit: self.bounds.max_tools(),
            });
        }
        let page = accept_tools(self.bounds, result)?;
        let offered = self.tools.len().saturating_add(page.len());
        if offered > self.bounds.max_tools() {
            return Err(McpProtocolError::TooManyTools {
                limit: self.bounds.max_tools(),
                offered,
            });
        }
        for tool in &page {
            if self.tools.iter().any(|old| old.name() == tool.name()) {
                return Err(McpProtocolError::ToolRepeated(
                    tool.name().as_str().to_owned(),
                ));
            }
        }
        self.tools.extend(page);
        self.pages += 1;
        self.bytes = bytes;
        if let Some(cursor) = &cursor {
            self.cursors.insert(cursor.clone());
        }
        self.complete = cursor.is_none();
        self.cursor = cursor;
        Ok(self.complete)
    }

    pub fn finish(self) -> Result<Vec<DiscoveredTool>, McpProtocolError> {
        if !self.complete {
            return Err(McpProtocolError::ToolCatalogIncomplete);
        }
        Ok(self.tools)
    }
}

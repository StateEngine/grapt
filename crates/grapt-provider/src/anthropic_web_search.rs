// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: Anthropic's live-only search with an explicit bounded
// use allowance. Reuses the native-spec/domain pattern adapted in web_search.rs

use crate::{DomainFilter, WebSearchDomainsError};

/// A separately admitted Anthropic server tool. This API offers only live
/// search; cached-only search and dynamic filtering/code execution are not
/// representable by this spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicWebSearchSpec {
    max_uses: u8,
    domains: Option<(DomainFilter, Vec<String>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Anthropic web search max_uses must be in 1..=5")]
pub struct AnthropicWebSearchUsesError;

impl AnthropicWebSearchSpec {
    /// Admit at most `max_uses` live searches in one physical request.
    pub fn live(max_uses: u8) -> Result<Self, AnthropicWebSearchUsesError> {
        if !(1..=5).contains(&max_uses) {
            return Err(AnthropicWebSearchUsesError);
        }
        Ok(Self {
            max_uses,
            domains: None,
        })
    }

    /// One bounded domain filter, never both allow and block lists.
    pub fn with_domains(
        mut self,
        filter: DomainFilter,
        domains: Vec<String>,
    ) -> Result<Self, WebSearchDomainsError> {
        self.domains = Some((filter, crate::web_search::normalize_domains(domains)?));
        Ok(self)
    }

    pub fn max_uses(&self) -> u8 {
        self.max_uses
    }

    /// Serialized server-tool bytes for request budgeting, not token usage.
    pub fn encoded_bytes(&self) -> usize {
        self.wire().to_string().len()
    }

    pub(crate) fn wire(&self) -> serde_json::Value {
        let mut tool = serde_json::json!({
            "type":"web_search_20250305", "name":"web_search", "max_uses":self.max_uses,
        });
        if let Some((kind, domains)) = &self.domains {
            let field = match kind {
                DomainFilter::Allow => "allowed_domains",
                DomainFilter::Block => "blocked_domains",
            };
            tool[field] = serde_json::json!(domains);
        }
        tool
    }
}

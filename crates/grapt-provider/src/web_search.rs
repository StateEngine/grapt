// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded domain controls and explicit cached/live access;
// no SDK, executor, credential owner, or unbounded search configuration.

/// Hosted search is separate from function definitions. The caller enables it
/// only after admitting provider-hosted web access for the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchSpec {
    access: WebSearchAccess,
    context: WebSearchContext,
    domains: Option<(DomainFilter, Vec<String>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchAccess {
    Cached,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchContext {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainFilter {
    Allow,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "web search needs 1..=100 ASCII domain names, each at most 253 bytes, without scheme or path"
)]
pub struct WebSearchDomainsError;

impl WebSearchSpec {
    pub fn new(access: WebSearchAccess) -> Self {
        Self {
            access,
            context: WebSearchContext::Medium,
            domains: None,
        }
    }

    pub fn with_context(mut self, context: WebSearchContext) -> Self {
        self.context = context;
        self
    }

    /// Domain filtering includes subdomains, following the hosted API's
    /// contract. It is not a local-network permission or a URL path filter.
    pub fn with_domains(
        mut self,
        filter: DomainFilter,
        domains: Vec<String>,
    ) -> Result<Self, WebSearchDomainsError> {
        self.domains = Some((filter, normalize_domains(domains)?));
        Ok(self)
    }

    pub fn access(&self) -> WebSearchAccess {
        self.access
    }

    /// Serialized hosted-tool bytes for request budgeting. This does not
    /// expose request serialization or admit provider/network execution.
    pub fn encoded_bytes(&self) -> usize {
        self.wire().to_string().len()
    }

    pub(crate) fn wire(&self) -> serde_json::Value {
        let mut fields = serde_json::Map::new();
        fields.insert("type".to_owned(), serde_json::json!("web_search"));
        fields.insert(
            "external_web_access".to_owned(),
            serde_json::json!(self.access == WebSearchAccess::Live),
        );
        fields.insert(
            "search_context_size".to_owned(),
            serde_json::json!(match self.context {
                WebSearchContext::Low => "low",
                WebSearchContext::Medium => "medium",
                WebSearchContext::High => "high",
            }),
        );
        if let Some((kind, domains)) = &self.domains {
            let field = match kind {
                DomainFilter::Allow => "allowed_domains",
                DomainFilter::Block => "blocked_domains",
            };
            let mut filter = serde_json::Map::new();
            filter.insert(field.to_owned(), serde_json::json!(domains));
            fields.insert("filters".to_owned(), serde_json::Value::Object(filter));
        }
        serde_json::Value::Object(fields)
    }
}

pub(crate) fn normalize_domains(
    domains: Vec<String>,
) -> Result<Vec<String>, WebSearchDomainsError> {
    if domains.is_empty() || domains.len() > 100 {
        return Err(WebSearchDomainsError);
    }
    let mut normalized = Vec::with_capacity(domains.len());
    for domain in domains {
        if domain.len() > 253 || !domain.is_ascii() {
            return Err(WebSearchDomainsError);
        }
        let domain = domain
            .strip_suffix('.')
            .unwrap_or(&domain)
            .to_ascii_lowercase();
        if !domain.contains('.')
            || !domain.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err(WebSearchDomainsError);
        }
        normalized.push(domain);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_mode_is_explicit_and_never_defaults_to_live() {
        assert_eq!(
            WebSearchSpec::new(WebSearchAccess::Cached).wire(),
            serde_json::json!({
                "type":"web_search", "external_web_access":false, "search_context_size":"medium",
            })
        );
        assert_eq!(
            WebSearchSpec::new(WebSearchAccess::Live)
                .with_context(WebSearchContext::Low)
                .wire()["external_web_access"],
            true
        );
    }

    #[test]
    fn filters_are_bounded_canonical_domains_not_urls() -> Result<(), WebSearchDomainsError> {
        let spec = WebSearchSpec::new(WebSearchAccess::Live).with_domains(
            DomainFilter::Allow,
            vec![
                "EXAMPLE.COM.".into(),
                "docs.example.com".into(),
                "example.com".into(),
            ],
        )?;
        assert_eq!(
            spec.wire()["filters"],
            serde_json::json!({"allowed_domains":["docs.example.com","example.com"]})
        );
        for bad in [
            "https://example.com",
            "example.com/path",
            "*.example.com",
            "local",
            "example..com",
            "-example.com",
            "example_.com",
            "éxample.com",
        ] {
            assert!(WebSearchSpec::new(WebSearchAccess::Cached)
                .with_domains(DomainFilter::Block, vec![bad.into()])
                .is_err());
        }
        assert!(WebSearchSpec::new(WebSearchAccess::Live)
            .with_domains(DomainFilter::Allow, vec![])
            .is_err());
        assert!(WebSearchSpec::new(WebSearchAccess::Live)
            .with_domains(DomainFilter::Allow, vec!["example.com".to_owned(); 101])
            .is_err());
        Ok(())
    }
}

// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: pure bounded discovery candidates, exact resource/issuer
// identities, no credentials on metadata requests, no legacy issuer fallback.
// commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.

use serde_json::Value;
use url::Url;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiscoveryError {
    #[error("OAuth discovery URL must be HTTPS without credentials or fragments")]
    Url,
    #[error("OAuth discovery metadata exceeds its bound or is malformed")]
    Format,
    #[error("OAuth protected resource metadata names a different resource")]
    Resource,
}

fn https(raw: &str) -> Result<Url, DiscoveryError> {
    if raw.len() > 8192 || raw.chars().any(char::is_control) || raw.trim() != raw {
        return Err(DiscoveryError::Url);
    }
    let url = Url::parse(raw).map_err(|_| DiscoveryError::Url)?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(DiscoveryError::Url);
    }
    Ok(url)
}

/// A parsed Bearer resource_metadata challenge takes precedence. Without one,
/// try the endpoint path then the origin's root well-known document. Callers
/// perform only unauthenticated, bounded GETs and do not follow redirects.
pub fn protected_resource_candidates(
    resource: &str,
    advertised: Option<&str>,
) -> Result<Vec<Url>, DiscoveryError> {
    let mut url = https(resource)?;
    if let Some(advertised) = advertised {
        return Ok(vec![https(advertised)?]);
    }
    let path = url.path().to_owned();
    url.set_query(None);
    let mut candidates = Vec::with_capacity(2);
    if path != "/" {
        url.set_path(&format!("/.well-known/oauth-protected-resource{path}"));
        candidates.push(url.clone());
    }
    url.set_path("/.well-known/oauth-protected-resource");
    candidates.push(url);
    Ok(candidates)
}

/// RFC 8414 insertion followed by OIDC insertion and appending. The caller
/// retains `issuer` unchanged for the mandatory exact metadata comparison.
pub fn authorization_server_candidates(issuer: &str) -> Result<Vec<Url>, DiscoveryError> {
    let mut url = https(issuer)?;
    if url.query().is_some() {
        return Err(DiscoveryError::Url);
    }
    let path = if url.path() == "/" { "" } else { url.path() }.to_owned();
    let mut candidates = Vec::with_capacity(3);
    for prefix in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/openid-configuration",
    ] {
        url.set_path(&format!("{prefix}{path}"));
        candidates.push(url.clone());
    }
    if !path.is_empty() {
        url.set_path(&format!(
            "{}/.well-known/openid-configuration",
            path.trim_end_matches('/')
        ));
        if !candidates.contains(&url) {
            candidates.push(url);
        }
    }
    Ok(candidates)
}

#[derive(Clone)]
pub struct ProtectedResource {
    resource: String,
    issuers: Vec<String>,
    scopes: Option<Vec<String>>,
}

impl std::fmt::Debug for ProtectedResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectedResource").finish_non_exhaustive()
    }
}

impl ProtectedResource {
    pub fn parse(bytes: &[u8], expected_resource: &str) -> Result<Self, DiscoveryError> {
        if bytes.len() > 65536 {
            return Err(DiscoveryError::Format);
        }
        let value: Value =
            crate::oauth::metadata_value(bytes).map_err(|_| DiscoveryError::Format)?;
        let resource = value["resource"].as_str().ok_or(DiscoveryError::Resource)?;
        https(resource)?;
        if resource != expected_resource {
            return Err(DiscoveryError::Resource);
        }
        let fields = value["authorization_servers"]
            .as_array()
            .filter(|values| !values.is_empty() && values.len() <= 16)
            .ok_or(DiscoveryError::Format)?;
        let mut issuers = Vec::with_capacity(fields.len());
        for field in fields {
            let issuer = field.as_str().ok_or(DiscoveryError::Format)?;
            let url = https(issuer)?;
            if url.query().is_some() || issuers.iter().any(|held| held == issuer) {
                return Err(DiscoveryError::Format);
            }
            issuers.push(issuer.to_owned());
        }
        let scopes = value
            .get("scopes_supported")
            .map(|value| {
                let fields = value
                    .as_array()
                    .filter(|values| values.len() <= 64)
                    .ok_or(DiscoveryError::Format)?;
                let mut scopes = Vec::with_capacity(fields.len());
                let mut bytes = 0usize;
                for field in fields {
                    let scope = field
                        .as_str()
                        .filter(|scope| {
                            !scope.is_empty()
                                && scope.bytes().all(|byte| {
                                    (0x21..=0x7e).contains(&byte) && !matches!(byte, b'"' | b'\\')
                                })
                        })
                        .ok_or(DiscoveryError::Format)?;
                    bytes += scope.len();
                    if bytes > 4096 || scopes.iter().any(|held| held == scope) {
                        return Err(DiscoveryError::Format);
                    }
                    scopes.push(scope.to_owned());
                }
                Ok(scopes)
            })
            .transpose()?;
        Ok(Self {
            resource: resource.to_owned(),
            issuers,
            scopes,
        })
    }
    pub fn resource(&self) -> &str {
        &self.resource
    }
    pub fn issuers(&self) -> &[String] {
        &self.issuers
    }
    pub fn scopes(&self) -> Option<&[String]> {
        self.scopes.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn candidates_follow_mcp_priority_and_strip_resource_query_only_from_discovery(
    ) -> Result<(), DiscoveryError> {
        let candidates =
            protected_resource_candidates("https://mcp.example/public/mcp?tenant=one", None)?;
        assert_eq!(
            candidates.iter().map(Url::as_str).collect::<Vec<_>>(),
            vec![
                "https://mcp.example/.well-known/oauth-protected-resource/public/mcp",
                "https://mcp.example/.well-known/oauth-protected-resource"
            ]
        );
        assert_eq!(
            protected_resource_candidates(
                "https://mcp.example/public/mcp",
                Some("https://metadata.example/resource")
            )?
            .iter()
            .map(Url::as_str)
            .collect::<Vec<_>>(),
            vec!["https://metadata.example/resource"]
        );
        assert_eq!(
            authorization_server_candidates("https://auth.example/tenant")?
                .iter()
                .map(Url::as_str)
                .collect::<Vec<_>>(),
            vec![
                "https://auth.example/.well-known/oauth-authorization-server/tenant",
                "https://auth.example/.well-known/openid-configuration/tenant",
                "https://auth.example/tenant/.well-known/openid-configuration"
            ]
        );
        assert_eq!(
            authorization_server_candidates("https://auth.example")?.len(),
            2
        );
        assert!(authorization_server_candidates("https://auth.example?tenant=one").is_err());
        Ok(())
    }
    #[test]
    fn protected_resource_preserves_exact_issuer_identity_and_rejects_resource_substitution(
    ) -> Result<(), DiscoveryError> {
        let bytes = br#"{"resource":"https://mcp.example/mcp","authorization_servers":["https://auth.example"],"scopes_supported":["read","write"]}"#;
        let resource = ProtectedResource::parse(bytes, "https://mcp.example/mcp")?;
        assert_eq!(resource.issuers(), &["https://auth.example"]);
        assert_eq!(
            resource.scopes(),
            Some(["read".to_owned(), "write".to_owned()].as_slice())
        );
        assert_eq!(
            ProtectedResource::parse(bytes, "https://mcp.example/other").err(),
            Some(DiscoveryError::Resource)
        );
        assert!(!format!("{resource:?}").contains("auth.example"));
        Ok(())
    }
}

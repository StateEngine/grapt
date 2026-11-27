// Copyright 2025 OpenAI; Copyright 2023-2026 SpaceXAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded HTTP(S) validation and global-form consent,
// without upstream session routing or browser actions. See notices/90-sections/mcp-url-elicitation.md.
//! November 2025 URL requests. These values are private user-facing material,
//! never tool results, browser instructions, or evidence of external completion.
use serde_json::{json, Value};

/// Server identity, deliberately distinct from a locally minted form/elicitation ID.
#[derive(Clone, PartialEq, Eq)]
pub struct ServerElicitationId(String);

impl ServerElicitationId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ServerElicitationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerElicitationId([private])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct UrlElicitation {
    id: ServerElicitationId,
    message: String,
    url: String,
    host: String,
}

impl std::fmt::Debug for UrlElicitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UrlElicitation([private])")
    }
}

impl UrlElicitation {
    pub fn parse(params: &Value) -> Result<Self, String> {
        if params["mode"] != "url" || params.get("requestedSchema").is_some() {
            return Err("expected URL elicitation without a form schema".into());
        }
        if params.get("task").is_some() {
            return Err("task-augmented elicitation is unavailable".into());
        }
        let text = |key: &str, limit: usize| {
            params[key]
                .as_str()
                .filter(|text| !text.is_empty() && text.len() <= limit)
                .ok_or_else(|| format!("elicitation {key} absent or beyond its bound"))
        };
        let id = text("elicitationId", 512)?;
        let message = text("message", 4096)?;
        let raw = text("url", 8192)?;
        if id.chars().any(display_control)
            || raw
                .chars()
                .any(|ch| display_control(ch) || ch.is_whitespace() || ch == '\\')
        {
            return Err("elicitation URL or ID contains unsafe display characters".into());
        }
        let url = url::Url::parse(raw).map_err(|_| "elicitation URL is invalid")?;
        let authority = raw
            .split_once("://")
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case(url.scheme()))
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default());
        if !matches!(url.scheme(), "http" | "https")
            || authority.is_none_or(|authority| authority.is_empty() || authority.contains('@'))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("elicitation URL must be HTTP(S), with a host and without userinfo".into());
        }
        let host = url
            .host_str()
            .ok_or("elicitation URL host absent")?
            .to_owned();
        Ok(Self {
            id: ServerElicitationId(id.to_owned()),
            message: message.to_owned(),
            url: raw.to_owned(),
            host,
        })
    }

    pub fn id(&self) -> &ServerElicitationId {
        &self.id
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn host(&self) -> &str {
        &self.host
    }

    /// A closed choice, with no default: neither consent nor refusal is implicit.
    /// Cancellation uses the existing cancellation operation, not this answer.
    pub fn fields(&self) -> Value {
        json!([{"key":"decision","type":"string","title":"External interaction consent",
            "required":true,"custom":false,"options":[
                {"value":"accept","label":"Accept (continue manually)"},
                {"value":"decline","label":"Decline"}]}])
    }

    pub fn response(&self, answer: &Value) -> Result<Value, String> {
        match answer["decision"].as_str() {
            Some("accept") => Ok(json!({"action":"accept"})),
            Some("decline") => Ok(json!({"action":"decline"})),
            _ => Err("URL elicitation answer is not explicit consent or decline".into()),
        }
    }
}

fn display_control(ch: char) -> bool {
    ch.is_control()
        || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// An error's requirements are not callbacks and never authorize automatic replay.
#[derive(Clone, PartialEq, Eq)]
pub struct UrlRequirements(Vec<UrlElicitation>);

impl std::fmt::Debug for UrlRequirements {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UrlRequirements")
            .field("count", &self.0.len())
            .finish()
    }
}

impl UrlRequirements {
    pub fn parse(data: &Value) -> Result<Self, String> {
        let rows = data["elicitations"]
            .as_array()
            .filter(|rows| !rows.is_empty() && rows.len() <= 8)
            .ok_or("URL-required error must carry one to eight elicitations")?;
        let requests = rows
            .iter()
            .map(UrlElicitation::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if requests
            .iter()
            .enumerate()
            .any(|(index, request)| requests[..index].iter().any(|prior| prior.id == request.id))
        {
            return Err("URL-required error repeats an elicitation ID".into());
        }
        Ok(Self(requests))
    }

    pub fn requests(&self) -> &[UrlElicitation] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_input_requires_a_bounded_web_url_and_explicit_content_free_decision(
    ) -> Result<(), String> {
        let params = json!({"mode":"url","message":"Connect manually","url":"https://example.test/connect?private=secret","elicitationId":"external-9"});
        let request = UrlElicitation::parse(&params)?;
        assert_eq!(request.id().as_str(), "external-9");
        for decision in ["accept", "decline"] {
            assert_eq!(
                request.response(&json!({"decision":decision}))?,
                json!({"action":decision})
            );
        }
        assert!(request.response(&json!({})).is_err());
        assert!(!format!("{request:?}").contains("secret"));
        for invalid in [
            "file:///secret",
            "javascript:alert(1)",
            "https://user:pass@example.test",
            "https://@example.test",
            "https://example.test/\u{001b}action",
            "https://example.test/\u{202e}action",
            "https:example.test",
            "https://example.test\\evil",
            "",
        ] {
            let mut params = params.clone();
            params["url"] = json!(invalid);
            assert!(UrlElicitation::parse(&params).is_err());
        }
        let mut params = params;
        params["elicitationId"] = json!("x".repeat(513));
        assert!(UrlElicitation::parse(&params).is_err());
        Ok(())
    }
}

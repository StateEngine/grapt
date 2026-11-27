// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: fail closed on malformed or ambiguous headers, cap all
// configured material, reserve transport/session fields, and mark values secret.
// (build_default_headers), commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.

use super::{HttpError, MAX_HEADER_BYTES};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::collections::BTreeMap;

pub(super) fn configured(fields: &BTreeMap<String, String>) -> Result<HeaderMap, HttpError> {
    if fields.len() > 64 {
        return Err(HttpError::Headers);
    }
    let mut headers = HeaderMap::new();
    let mut bytes = 0usize;
    for (name, value) in fields {
        bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
        if name.len() > 128 || bytes > MAX_HEADER_BYTES {
            return Err(HttpError::Headers);
        }
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| HttpError::Headers)?;
        if matches!(
            name.as_str(),
            "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "upgrade"
                | "proxy-authorization"
                | "proxy-connection"
                | "te"
                | "trailer"
                | "accept"
                | "content-type"
                | "mcp-session-id"
                | "mcp-protocol-version"
                | "last-event-id"
        ) || headers.contains_key(&name)
        {
            return Err(HttpError::Headers);
        }
        let mut value = HeaderValue::from_str(value).map_err(|_| HttpError::Headers)?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(headers)
}

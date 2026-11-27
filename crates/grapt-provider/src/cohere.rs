//! Native Cohere V2 Chat on the shared bounded transport.
// Copyright 2023 Vercel, Inc. SPDX-License-Identifier: Apache-2.0
// Adapted from vercel/ai packages/cohere/src at
// 7772d0ca0f7abc2cb2a0ad61b0272a72655d780d; see licenses/vercel-ai/.
mod decoder;
mod input;
use crate::{CodexInputItem, MaxOutputTokens, ToolChoice, ToolDefinition};
pub(crate) use decoder::Decoder;
use grapt_types::ModelRef;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohereChatRequest {
    pub model: ModelRef,
    pub messages: Vec<CodexInputItem>,
    pub max_tokens: Option<MaxOutputTokens>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
}

pub(crate) fn body(request: &CohereChatRequest) -> Result<Value, &'static str> {
    if request.model.variant().is_some() {
        return Err("Cohere generation variants are not configured");
    }
    let mut body = json!({
        "model":request.model.model().as_str(),
        "messages":input::project(&request.messages)?,
        "stream":true,
    });
    if let Some(bound) = request.max_tokens {
        body["max_tokens"] = json!(bound.value());
    }
    let mut names = std::collections::BTreeSet::new();
    if request
        .tools
        .iter()
        .any(|tool| !names.insert(wire_name(&tool.name)))
    {
        return Err("Cohere function aliases collide");
    }
    let tools = request.tools.iter().filter(|tool| {
        !matches!(&request.tool_choice, Some(ToolChoice::Named {tool: selected}) if selected != &tool.name)
    }).map(|tool| {
        if tool.input_format != crate::ToolInputFormat::Json {
            return Err("Cohere tools require JSON input");
        }
        let schema: Value = serde_json::from_str(&tool.input_schema)
            .map_err(|_| "Cohere tool schema is invalid")?;
        let mut function = json!({"name":wire_name(&tool.name),"parameters":schema});
        if let Some(description) = &tool.description {
            function["description"] = json!(description);
        }
        Ok(json!({"type":"function","function":function}))
    }).collect::<Result<Vec<_>, &'static str>>()?;
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        match request.tool_choice {
            Some(ToolChoice::None) => body["tool_choice"] = json!("NONE"),
            Some(ToolChoice::Required | ToolChoice::Named { .. }) => {
                body["tool_choice"] = json!("REQUIRED")
            }
            _ => (),
        }
    }
    Ok(body)
}

pub(crate) fn wire_name(name: &str) -> String {
    if name.len() <= 64
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return name.to_owned();
    }
    use sha2::Digest;
    let digest = sha2::Sha256::digest(name.as_bytes());
    let mut alias = String::from("tool_");
    for byte in &digest[..24] {
        use std::fmt::Write;
        let _ = write!(&mut alias, "{byte:02x}");
    }
    alias
}

//! Native Gemini GenerateContent protocol; shared bounded transport and vault authority.
// Copyright 2023 Vercel, Inc. SPDX-License-Identifier: Apache-2.0
// Adapted from vercel/ai packages/google/src at
// 7772d0ca0f7abc2cb2a0ad61b0272a72655d780d; see licenses/vercel-ai/.
mod decoder;
mod input;
pub(crate) mod replay;
use crate::{CodexInputItem, MaxOutputTokens, ToolChoice, ToolDefinition};
pub(crate) use decoder::Decoder;
use grapt_types::ModelRef;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleGeminiRequest {
    pub model: ModelRef,
    pub messages: Vec<CodexInputItem>,
    pub max_tokens: Option<MaxOutputTokens>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
}

pub(crate) fn stream_url(base: &str, model: &str) -> Result<String, &'static str> {
    if model.is_empty()
        || model.len() > 256
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err("Gemini model must be one bounded resource segment");
    }
    Ok(format!(
        "{}/models/{model}:streamGenerateContent?alt=sse",
        base.trim_end_matches('/')
    ))
}

pub(crate) fn body(request: &GoogleGeminiRequest) -> Result<Value, &'static str> {
    if request.model.variant().is_some() {
        return Err("Gemini generation variants are not configured");
    }
    let (mut contents, system) = input::project(&request.messages, request.model.model().as_str())?;
    let mut body = json!({"contents":contents});
    if !system.is_empty() {
        if request.model.model().as_str().starts_with("gemma-") {
            contents.insert(0, json!({"role":"user", "parts":system}));
            body["contents"] = json!(contents);
        } else {
            body["systemInstruction"] = json!({"parts":system});
        }
    }
    if let Some(bound) = request.max_tokens {
        body["generationConfig"] = json!({"maxOutputTokens":bound.value()});
    }
    if !request.tools.is_empty() {
        let mut names = std::collections::BTreeSet::new();
        if request
            .tools
            .iter()
            .any(|tool| !names.insert(wire_name(&tool.name)))
        {
            return Err("Gemini function aliases collide");
        }
        let declarations = request
            .tools
            .iter()
            .map(|tool| {
                if tool.input_format != crate::ToolInputFormat::Json {
                    return Err("Gemini tools require JSON input");
                }
                let schema: Value = serde_json::from_str(&tool.input_schema)
                    .map_err(|_| "Gemini tool schema is invalid")?;
                let mut declaration =
                    json!({"name":wire_name(&tool.name),"parametersJsonSchema":schema});
                if let Some(description) = &tool.description {
                    declaration["description"] = json!(description);
                }
                Ok(declaration)
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        body["tools"] = json!([{"functionDeclarations":declarations}]);
        let mut config = json!({"mode": match request.tool_choice { Some(ToolChoice::None) => "NONE", Some(ToolChoice::Required | ToolChoice::Named {..}) => "ANY", _ => "AUTO" }});
        if let Some(ToolChoice::Named { tool }) = &request.tool_choice {
            config["allowedFunctionNames"] = json!([wire_name(tool)]);
        }
        body["toolConfig"] = json!({"functionCallingConfig":config});
    }
    Ok(body)
}

/// Native function names are bounded; durable tool names remain unchanged.
pub(crate) fn wire_name(name: &str) -> String {
    if name.len() <= 64
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
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

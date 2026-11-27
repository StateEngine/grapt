//! Existing v1 output protocol; unknown fields remain annotations-only.
//! Extracted from this crate's original hooks/command.rs without changing decoding.
//! Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
//! Licensed under the Apache License, Version 2.0; see LICENSE-APACHE.
//! Provided AS IS, without warranties or conditions of any kind.
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Output {
    decision: Option<String>,
    reason: Option<String>,
    r#continue: Option<bool>,
    stop_reason: Option<String>,
    hook_specific_output: Option<SpecificOutput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpecificOutput {
    permission_decision: Option<String>,
    permission_decision_reason: Option<String>,
    additional_context: Option<String>,
}

pub(super) fn notes(stdout: &str) -> Result<Vec<String>, String> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if !text.starts_with(['{', '[']) {
        return Ok(vec![text.to_owned()]);
    }
    if text.starts_with('[') {
        return Err("hook returned invalid structured output".into());
    }
    // Malformed structured output is a visible refusal, not plain text which
    // could accidentally swallow a misspelled or truncated block decision.
    let output: Output =
        serde_json::from_str(text).map_err(|_| "hook returned invalid structured output")?;
    if output.r#continue == Some(false) {
        return Err(format!(
            "hook stopped processing: {}",
            output
                .stop_reason
                .as_deref()
                .and_then(nonempty)
                .unwrap_or("no reason supplied")
        ));
    }
    match output.decision.as_deref() {
        Some("block") => {
            return Err(format!(
                "hook denied: {}",
                output
                    .reason
                    .as_deref()
                    .and_then(nonempty)
                    .unwrap_or("no reason supplied")
            ));
        }
        None | Some("allow" | "approve") => {}
        Some(_) => return Err("hook returned an unsupported decision".into()),
    }
    let Some(specific) = output.hook_specific_output else {
        return Ok(Vec::new());
    };
    match specific.permission_decision.as_deref() {
        Some("deny") => Err(format!(
            "hook denied: {}",
            specific
                .permission_decision_reason
                .as_deref()
                .and_then(nonempty)
                .unwrap_or("no reason supplied")
        )),
        // An allow decision has NO authorization effect; permission checking
        // still belongs to the permission layer. updatedInput is never applied.
        None | Some("allow") => Ok(specific
            .additional_context
            .as_deref()
            .and_then(nonempty)
            .map(str::to_owned)
            .into_iter()
            .collect()),
        Some(_) => Err("hook returned an unsupported permission decision".into()),
    }
}

fn nonempty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

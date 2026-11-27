//! Per-call enforcement retained outside mutable hook results until capture.
use super::*;

#[derive(Clone)]
pub(in crate::dogfood::tools) struct Receipt {
    pub enforcement: RealizedEnforcement,
    pub ran: Option<bool>,
}
impl Receipt {
    pub(in crate::dogfood::tools) fn pending(enabled: bool) -> Self {
        Self {
            enforcement: if enabled {
                degraded("command did not reach the sandbox backend")
            } else {
                RealizedEnforcement::HarnessOnly
            },
            ran: Some(false),
        }
    }
    pub(in crate::dogfood::tools) fn started(enforcement: RealizedEnforcement) -> Self {
        Self {
            enforcement,
            ran: Some(true),
        }
    }
    pub(in crate::dogfood::tools) fn parse(raw: &str) -> Option<Self> {
        let receipt = super::receipt(raw)?;
        let enforcement = &receipt["enforcement"];
        let enforcement = match enforcement["kind"].as_str()? {
            "harness_only" => RealizedEnforcement::HarnessOnly,
            "platform_sandbox" => RealizedEnforcement::PlatformSandbox {
                backend: enforcement["backend"].as_str()?.into(),
            },
            "degraded" => RealizedEnforcement::Degraded {
                backend: enforcement["backend"].as_str()?.into(),
                reason: enforcement["reason"].as_str()?.into(),
            },
            _ => return None,
        };
        let ran = match &receipt["ran"] {
            serde_json::Value::Null => None,
            serde_json::Value::Bool(ran) => Some(*ran),
            _ => return None,
        };
        Some(Self { enforcement, ran })
    }
    /// Shell callbacks keep their command-output contract. Structured terminal
    /// reports keep their other fields; callbacks cannot alter this receipt.
    pub(in crate::dogfood::tools) fn output(
        &self,
        output: Vec<u8>,
        shell: bool,
        machine: bool,
    ) -> Result<Vec<u8>, String> {
        let text = match std::str::from_utf8(&output) {
            Ok(text) => std::borrow::Cow::Borrowed(text),
            Err(error) if machine => return Err(format!("command output is not UTF-8: {error}")),
            Err(_) => std::borrow::Cow::Owned(format!(
                "{}\n[tool output projection: invalid UTF-8 bytes were replaced explicitly]",
                String::from_utf8_lossy(&output)
            )),
        };
        let mut result = if shell {
            serde_json::json!({"output":text})
        } else {
            serde_json::from_slice::<serde_json::Value>(&output)
                .ok()
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(|| serde_json::json!({"output":text}))
        };
        result["enforcement"] = value(&self.enforcement);
        result["ran"] = serde_json::json!(self.ran);
        let bound = usize::try_from(grapt_runtime::TOOL_ARTIFACT_MAX_BYTES)
            .map_err(|_| "execution receipt bound exceeds usize")?;
        bounded(result, bound, machine)
    }
    pub(in crate::dogfood::tools) fn error(&self, detail: &str) -> String {
        let structured = super::receipt(detail);
        let detail = structured
            .as_ref()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| {
                        value["enforcement"]
                            .get("reason")
                            .and_then(serde_json::Value::as_str)
                    })
            })
            .unwrap_or(detail);
        failure(&self.enforcement, detail, self.ran)
    }
}

// Account for JSON escaping while retaining both output ends, including the
// command's exit paragraph. Every pass is linear and bounded by captured output.
fn retained(text: &str, budget: usize) -> (&str, &str) {
    let mut head = 0;
    let mut used = 0;
    for (offset, character) in text.char_indices() {
        let cost = escaped(character);
        if used + cost > budget / 2 {
            break;
        }
        used += cost;
        head = offset + character.len_utf8();
    }
    let mut tail = text.len();
    for (offset, character) in text.char_indices().rev() {
        if offset < head || used + escaped(character) > budget {
            break;
        }
        used += escaped(character);
        tail = offset;
    }
    (&text[..head], &text[tail..])
}
fn escaped(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
        c if c <= '\u{001f}' => 6,
        c => c.len_utf8(),
    }
}

/// Bound the complete envelope, including JSON escaping and execution metadata.
fn bounded(mut result: serde_json::Value, bound: usize, machine: bool) -> Result<Vec<u8>, String> {
    let encoded = serde_json::to_vec(&result).map_err(|error| error.to_string())?;
    if encoded.len() <= bound {
        return Ok(encoded);
    }
    if machine {
        return Err("command output exceeds its complete execution receipt bound".into());
    }
    let output = result
        .get_mut("output")
        .map(serde_json::Value::take)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or("execution result exceeds its receipt bound")?;
    result["output"] = serde_json::json!("");
    let overhead = serde_json::to_vec(&result)
        .map_err(|error| error.to_string())?
        .len();
    let available = bound
        .checked_sub(overhead + 256)
        .ok_or("execution metadata exceeds receipt bound")?;
    let (head, tail) = retained(&output, available);
    let omitted = output.len() - head.len() - tail.len();
    result["output"] = serde_json::json!(format!(
        "{head}\n[shell: omitted {omitted} output bytes to retain execution receipt]\n{tail}"
    ));
    result["omitted_output_bytes"] = serde_json::json!(omitted);
    let encoded = serde_json::to_vec(&result).map_err(|error| error.to_string())?;
    if encoded.len() > bound {
        return Err("execution receipt exceeds its bound".into());
    }
    Ok(encoded)
}

pub(in crate::dogfood) fn project(raw: &str, bound: usize) -> Option<Result<String, String>> {
    let result = super::receipt(raw)?;
    Some(
        bounded(result, bound, false)
            .and_then(|bytes| String::from_utf8(bytes).map_err(|error| error.to_string())),
    )
}

//! Approval display is captured from the same prepared effect and input that
//! the permission runner consumes. It never grants or changes authority.
//! Copyright 2025 OpenAI. Modified by Grapt: existing bounded ask metadata.
use super::*;
use grapt_api::u3a::{OpenObject, PermissionPreviewDto, MAX_PERMISSION_PREVIEW_BYTES};
use serde_json::{json, Value};

const UNAVAILABLE_PREVIEW_REASON: &str =
    "Approval preview exceeds its byte bound; inspect the full requested resources.";

/// The preview is display-only, so it degrades instead of failing the effect:
/// the payload text is truncated with a marker that names the omitted bytes,
/// and when nothing fits the ask proceeds without a preview. The full effect
/// still applies after approval.
pub(super) fn metadata(
    root: &Path,
    prepared: &PreparedEffect,
    input: &BoundedToolInput,
    consequences: Option<&[grapt_permission::PermissionConsequence]>,
) -> Option<Value> {
    let consequences = consequences.map(|items| {
        items
            .iter()
            .map(|item| json!({"action":item.action,"resource":item.resource}))
            .collect::<Vec<_>>()
    });
    let preview = match build(root, prepared, input) {
        Ok(Some(preview)) => fit_preview(preview, &prepared.action, consequences.as_deref()),
        _ => return None,
    };
    if !envelope_fits(&preview, &prepared.action, consequences.as_deref()) {
        return None;
    }
    // The wire shape and per-field bounds are checked before a permission is
    // persisted, so a preview cannot make a durable ask unreadable later.
    let _: PermissionPreviewDto = serde_json::from_value(preview.clone()).ok()?;
    Some(json!({"approvalPreview":preview}))
}

fn unavailable_preview() -> Value {
    json!({"type":"unavailable","reason":UNAVAILABLE_PREVIEW_REASON})
}

/// The whole envelope shares one metadata budget with the policy and
/// consequence fields, so the preview payload is the part that yields: find
/// the largest text that keeps the envelope decodable.
fn fit_preview(mut preview: Value, tool: &str, consequences: Option<&[Value]>) -> Value {
    if fits(&preview, tool, consequences) {
        return preview;
    }
    let field = if preview.get("command").and_then(Value::as_str).is_some() {
        "command"
    } else if preview.get("patch").and_then(Value::as_str).is_some() {
        "patch"
    } else {
        return unavailable_preview();
    };
    let source = match preview[field].as_str() {
        Some(text) => text.to_owned(),
        None => return unavailable_preview(),
    };
    let mut lo = 0usize;
    let mut hi = source.len();
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let mut candidate = preview.clone();
        candidate[field] = json!(truncate_text(&source, mid));
        if fits(&candidate, tool, consequences) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    preview[field] = json!(truncate_text(&source, lo));
    preview["truncated"] = json!(true);
    if fits(&preview, tool, consequences) {
        return preview;
    }
    unavailable_preview()
}

fn envelope_fits(preview: &Value, tool: &str, consequences: Option<&[Value]>) -> bool {
    let envelope = json!({"tool":tool,
        "policy":{"ownerFloor":false,"removesFiles":false},
        "consequences":consequences,
        "request":{"approvalPreview":preview}});
    serde_json::from_value::<OpenObject>(envelope).is_ok()
}

/// The preview must fit both the shared metadata envelope and its own
/// canonical byte bound; the payload yields to the stricter of the two.
fn fits(preview: &Value, tool: &str, consequences: Option<&[Value]>) -> bool {
    envelope_fits(preview, tool, consequences)
        && serde_json::to_vec(preview)
            .is_ok_and(|bytes| bytes.len() <= MAX_PERMISSION_PREVIEW_BYTES)
}

/// Cut `source` to at most `keep_bytes` on a char boundary; when bytes are
/// dropped, a trailing marker names the omitted count exactly.
fn truncate_text(source: &str, keep_bytes: usize) -> String {
    let mut end = keep_bytes.min(source.len());
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    if end == source.len() {
        return source.to_owned();
    }
    let mut shown = source[..end].to_owned();
    if !shown.is_empty() && !shown.ends_with('\n') {
        shown.push('\n');
    }
    shown.push_str(&format!("\u{2026} {} more bytes", source.len() - end));
    shown
}

pub(super) fn build(
    root: &Path,
    prepared: &PreparedEffect,
    input: &BoundedToolInput,
) -> Result<Option<Value>, String> {
    let shell = |command: &str, cwd: &Path, program: &Path| -> Result<Value, String> {
        let cwd = cwd.to_str().ok_or("approval cwd is not UTF-8")?;
        let shell = program.to_str().ok_or("approval shell is not UTF-8")?;
        Ok(json!({"type":"shell", "command":command, "cwd":cwd, "shell":shell, "truncated":false}))
    };
    let value = match &prepared.target {
        EffectTarget::Shell {
            command, program, ..
        } => shell(command, root, program)?,
        EffectTarget::TerminalExec(spec) => {
            let mut preview = shell(&spec.command, &spec.cwd, &spec.program)?;
            if !spec.environment.write_targets().is_empty() {
                preview["classifierVerdict"] = json!("Requested write target; earlier effects may repeat. Containment remains enabled.");
            }
            preview
        }
        EffectTarget::Edit(_) => {
            let args: EditArguments = decode_input(input)?;
            let mut patch = format!(
                "--- {}\n+++ {}\n@@ replacement{} @@\n",
                args.path,
                args.path,
                if args.replace_all.unwrap_or(false) {
                    " (all exact matches)"
                } else {
                    ""
                }
            );
            marked(&mut patch, '-', args.old_text.as_str());
            marked(&mut patch, '+', &args.new_text);
            patch_value("replacement", &patch)
        }
        EffectTarget::Coding(coding::CodingTarget::Patch(_)) => {
            let value: Value =
                serde_json::from_slice(input.as_bytes()).map_err(|e| e.to_string())?;
            if let Some(text) = value.get("input").and_then(Value::as_str) {
                patch_value("apply-patch", text)
            } else {
                let operations = value.get("operations").ok_or("patch operations missing")?;
                patch_value(
                    "operations",
                    &serde_json::to_string_pretty(operations).map_err(|e| e.to_string())?,
                )
            }
        }
        EffectTarget::Coding(
            coding::CodingTarget::Write(_) | coding::CodingTarget::Overwrite { .. },
        ) => {
            let value: Value =
                serde_json::from_slice(input.as_bytes()).map_err(|e| e.to_string())?;
            let path = value
                .get("path")
                .and_then(Value::as_str)
                .ok_or("write path missing")?;
            let content = value
                .get("content")
                .and_then(Value::as_str)
                .ok_or("write content missing")?;
            let mut patch = format!("+++ {path}\n@@ complete replacement content @@\n");
            marked(&mut patch, '+', content);
            patch_value("write", &patch)
        }
        _ => return Ok(None),
    };
    Ok(Some(value))
}

fn marked(output: &mut String, sign: char, text: &str) {
    for line in text.split_inclusive('\n') {
        output.push(sign);
        output.push_str(line);
        if !line.ends_with('\n') {
            output.push_str("\n\\ No newline at end of input\n");
        }
    }
}

fn patch_value(format: &str, text: &str) -> Value {
    json!({"type":"patch", "format":format, "patch":text, "truncated":false})
}

#[cfg(test)]
#[path = "tests/approval_preview.rs"]
mod tests;

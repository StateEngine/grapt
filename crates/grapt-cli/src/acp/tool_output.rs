//! ACP v1 replaces tool content. A final result is one bounded snapshot.
use super::{transport::FRAME_BYTES, RpcFault};
use serde_json::{json, Value};

fn frame_bytes(session: &str, update: &Value) -> Result<usize, RpcFault> {
    serde_json::to_vec(&json!({"jsonrpc":"2.0","method":"session/update",
        "params":{"sessionId":session,"update":update}}))
    .map(|bytes| bytes.len().saturating_add(1))
    .map_err(|_| RpcFault::internal("tool update encoding failed"))
}
fn marker(kept: usize, total: usize) -> String {
    format!(
        "\n[output truncated: kept {kept} of {total} bytes; full output is in the session history]"
    )
}
fn escaped(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        control if u32::from(control) < 0x20 => 6,
        other => other.len_utf8(),
    }
}
fn width(text: &str) -> usize {
    text.chars().map(escaped).sum()
}

/// Preserve a fitting snapshot. Otherwise budget its actual escaped envelope,
/// retain ordered text prefixes and mark every shortened item explicitly.
/// Input, attachments and metadata are never silently discarded.
pub(super) fn bounded(session: &str, mut update: Value, maximum: usize) -> Result<Value, RpcFault> {
    let maximum = maximum.min(FRAME_BYTES);
    if update["sessionUpdate"] != "tool_call_update" || frame_bytes(session, &update)? <= maximum {
        return Ok(update);
    }
    let mut texts = Vec::new();
    if let Some(content) = update.get_mut("content").and_then(Value::as_array_mut) {
        for (index, item) in content.iter_mut().enumerate() {
            if let Some(text) = item.pointer_mut("/content/text") {
                if let Some(original) = text.as_str().map(str::to_owned) {
                    // Reserving total's digit width also bounds kept's width.
                    *text = json!(marker(original.len(), original.len()));
                    texts.push((index, original));
                }
            }
        }
    }
    let mut remaining = maximum
        .checked_sub(frame_bytes(session, &update)?)
        .ok_or_else(|| {
            RpcFault::internal("tool metadata or attachments exceed the ACP frame bound")
        })?;
    for (index, original) in texts {
        let reserved = width(&marker(original.len(), original.len()));
        let full = width(&original);
        let replacement = if full <= remaining.saturating_add(reserved) {
            remaining = remaining.saturating_add(reserved) - full;
            original
        } else {
            let mut kept = 0;
            let mut used = 0;
            for character in original.chars() {
                let next = escaped(character);
                if next > remaining.saturating_sub(used) {
                    break;
                }
                used += next;
                kept += character.len_utf8();
            }
            let suffix = marker(kept, original.len());
            remaining = remaining - used + reserved - width(&suffix);
            format!("{}{suffix}", &original[..kept])
        };
        update["content"][index]["content"]["text"] = json!(replacement);
    }
    if frame_bytes(session, &update)? > maximum {
        return Err(RpcFault::internal(
            "tool snapshot cannot fit the ACP frame bound",
        ));
    }
    Ok(update)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update(text: &str) -> Value {
        json!({"sessionUpdate":"tool_call_update","toolCallId":"call_a","status":"completed",
            "content":[{"type":"content","content":{"type":"text","text":text}}],
            "rawInput":{},"rawOutput":{"metadata":{}}})
    }
    #[test]
    fn a_fitting_snapshot_is_unchanged() {
        let original = update("result");
        let result = bounded("ses_a", original.clone(), 262_144)
            .unwrap_or_else(|fault| panic!("{}", fault.message));
        assert_eq!(result, original);
    }
    #[test]
    fn escaped_utf8_output_fits_one_final_snapshot_with_an_honest_marker() {
        let original = format!("{}{}", "é".repeat(80_000), "\u{1}".repeat(60_000));
        let result = bounded("ses_a", update(&original), 262_144)
            .unwrap_or_else(|fault| panic!("{}", fault.message));
        assert_eq!(result["status"], "completed");
        assert!(frame_bytes("ses_a", &result).is_ok_and(|bytes| bytes <= 262_144));
        let text = result["content"][0]["content"]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("bounded tool text missing"));
        let (prefix, notice) = text
            .rsplit_once("\n[output truncated:")
            .unwrap_or_else(|| panic!("truncation marker missing"));
        assert!(original.starts_with(prefix));
        assert_eq!(
            notice,
            format!(
                " kept {} of {} bytes; full output is in the session history]",
                prefix.len(),
                original.len()
            )
        );
    }
    #[test]
    fn oversized_metadata_is_a_fault() {
        let mut original = update("result");
        original["rawInput"] = json!({"oversized":"x".repeat(262_144)});
        assert!(bounded("ses_a", original, 262_144).is_err());
    }
    #[test]
    fn multiple_text_items_share_the_actual_envelope_budget() {
        let mut original = update(&"\u{1}".repeat(100_000));
        original["content"]
            .as_array_mut()
            .unwrap_or_else(|| panic!("fixture content"))
            .push(json!({"type":"content","content":{"type":"text","text":"😀".repeat(80_000)}}));
        let result =
            bounded("ses_a", original, 262_144).unwrap_or_else(|fault| panic!("{}", fault.message));
        assert!(frame_bytes("ses_a", &result).is_ok_and(|bytes| bytes <= 262_144));
        for item in result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("fixture content"))
        {
            assert!(item["content"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("output truncated:")));
        }
    }
}

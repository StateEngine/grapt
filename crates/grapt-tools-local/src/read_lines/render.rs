//! Numbered source presentation.
//! Copyright 2025 OpenAI.
//! Apache-2.0; Modified by Grapt: retain lossless Unicode pagination,
//! one file identity header, scoped instructions, and structured opt-in.
use crate::search::OUTPUT_BYTES;

/// Encode a bounded compact or structured source page, including attached scoped metadata.
pub fn encode_page(
    page: &serde_json::Value,
    structured: bool,
    start_line: u64,
    start_column: u64,
) -> Result<Vec<u8>, String> {
    if structured {
        let bytes = serde_json::to_vec(page).map_err(|error| error.to_string())?;
        if bytes.len() > OUTPUT_BYTES {
            return Err("coding output byte bound exceeded".into());
        }
        return Ok(bytes);
    }
    let mut text = format!(
        "File: {} | sha256: {} | total_lines: {} | start_line: {start_line} start_column: {start_column}\n",
        page["path"],
        page["sha256"].as_str().ok_or("read_lines hash absent")?,
        page["totalLines"],
    );
    for row in page["lines"].as_array().ok_or("read_lines rows absent")? {
        text.push_str(&format!(
            "{}→{}\n",
            row["line"],
            row["text"].as_str().ok_or("read_lines text absent")?,
        ));
    }
    if let Some(line) = page["nextLine"].as_u64() {
        let column = page["nextColumn"]
            .as_u64()
            .ok_or("read_lines column absent")?;
        text.push_str(&format!(
            "[continue: start_line={line} start_column={column}]\n"
        ));
    } else {
        text.push_str("[end of file]\n");
    }
    // Preserve the existing instruction text, precedence and incomplete notices,
    // including the fallback for instructions that do not fit in the page.
    if page.get("scopedInstructions").is_some() {
        let mut instructions = serde_json::Map::new();
        for key in ["scopedInstructions", "scopedPrecedence", "scopedIncomplete"] {
            if let Some(value) = page.get(key) {
                instructions.insert(key.into(), value.clone());
            }
        }
        text.push_str("[scoped instructions]\n");
        text.push_str(&serde_json::Value::Object(instructions).to_string());
        text.push('\n');
    }
    if text.len() > OUTPUT_BYTES {
        return Err("read_lines compact output bound exceeded".into());
    }
    Ok(text.into_bytes())
}

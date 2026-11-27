//! Bounded diagnostics from Git porcelain-v1 -z records; no filesystem traversal.

const MAX_PATHS: usize = 8;
const MAX_ESCAPED_PATH_BYTES: usize = 160;

pub(super) fn retained_path(path: &std::path::Path, ignored: bool) -> String {
    let mut normalized = Vec::new();
    for component in path.iter() {
        if !normalized.is_empty() {
            normalized.push(b'/');
        }
        normalized.extend_from_slice(component.as_encoded_bytes());
    }
    let mut paths = Vec::new();
    preview(&mut paths, &normalized);
    let category = if ignored {
        "retained ignored files"
    } else {
        "uncommitted changes"
    };
    format!(
        "{category}: {}; move or remove retained content before retrying",
        paths.join(", ")
    )
}

pub(super) fn ensure_clean(output: &[u8]) -> Result<(), String> {
    if output.is_empty() {
        return Ok(());
    }
    let records = output
        .strip_suffix(b"\0")
        .ok_or("Git returned an incomplete NUL-delimited status; refusing operation")?;
    let mut records = records.split(|byte| *byte == 0);
    let mut ignored = Vec::new();
    let mut uncommitted = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 || record[2] != b' ' {
            return Err("Git returned a malformed status record; refusing operation".into());
        }
        let state = &record[..2];
        if state == b"!!" {
            preview(&mut ignored, &record[3..]);
        } else if state == b"??" {
            preview(&mut uncommitted, &record[3..]);
        } else {
            if state == b"  "
                || !state.iter().all(|byte| {
                    matches!(*byte, b' ' | b'M' | b'T' | b'A' | b'D' | b'R' | b'C' | b'U')
                })
            {
                return Err("Git returned an unknown status state; refusing operation".into());
            }
            preview(&mut uncommitted, &record[3..]);
            if state.iter().any(|byte| matches!(*byte, b'R' | b'C')) {
                // Porcelain -z gives destination then an additional NUL-delimited
                // origin. Consume the pair even after the diagnostic path bound.
                let origin = records.next().filter(|path| !path.is_empty()).ok_or(
                    "Git returned an incomplete rename/copy status pair; refusing operation",
                )?;
                preview(&mut uncommitted, origin);
            }
        }
    }
    let mut categories = Vec::new();
    if !uncommitted.is_empty() {
        categories.push(format!("uncommitted changes: {}", uncommitted.join(", ")));
    }
    if !ignored.is_empty() {
        categories.push(format!("retained ignored files: {}", ignored.join(", ")));
    }
    if categories.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{}; commit or move retained files before retrying (up to {MAX_PATHS} paths per category)",
            categories.join("; ")
        ))
    }
}

fn preview(paths: &mut Vec<String>, path: &[u8]) {
    if paths.len() == MAX_PATHS {
        return;
    }
    // Byte escaping handles non-UTF-8 names and prevents control characters
    // from changing terminal output. Never include file contents.
    let mut path: String = path
        .escape_ascii()
        .take(MAX_ESCAPED_PATH_BYTES + 1)
        .map(char::from)
        .collect();
    if path.len() > MAX_ESCAPED_PATH_BYTES {
        path.truncate(MAX_ESCAPED_PATH_BYTES);
        path.push_str("...");
    }
    paths.push(format!("\"{path}\""));
}

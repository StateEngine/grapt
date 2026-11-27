//! Additive source views; the original `read` byte-window contract is unchanged.

use super::*;

pub(super) fn read_target(root: &Path, requested: &str) -> Result<PathBuf, String> {
    let relative = relative_path(root, requested)?;
    reject_links(root, relative)?;
    let path = resolve_existing(root, requested, false)?;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("read_lines metadata unavailable: {error}"))?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err("read_lines requires a regular file without linked components".into());
    }
    Ok(path)
}

pub(super) fn read_lines(
    root: &Path,
    path: &Path,
    start_line: u64,
    start_column: u64,
    line_count: u64,
    structured: bool,
) -> Result<Vec<u8>, String> {
    let mut output = source_pages::page(root, path, start_line, start_column, line_count)?;
    // Scoped instructions ride on the read that opens a file, not on every page.
    if start_line == 1 && start_column == 1 {
        let scoped = crate::dogfood::instructions::scoped_for_read(root, path);
        if !scoped.is_empty() {
            output["scopedPrecedence"] = crate::dogfood::instructions::SCOPED_PRECEDENCE.into();
            if let Some(incomplete) = scoped.incomplete() {
                output["scopedIncomplete"] = incomplete.into();
            }
            let paths: Vec<String> = scoped
                .instructions
                .iter()
                .map(|scoped| scoped.path.clone())
                .collect();
            output["scopedInstructions"] = scoped
                .instructions
                .into_iter()
                .map(|scoped| match scoped.content {
                    Ok(content) => serde_json::json!({"path":scoped.path, "content":content}),
                    Err(reason) => serde_json::json!({"path":scoped.path, "unavailable":reason}),
                })
                .collect();
            if let Ok(encoded) =
                source_pages::encode_page(&output, structured, start_line, start_column)
            {
                return Ok(encoded);
            }
            // The page itself fits; report the instructions it could not carry.
            output["scopedInstructions"] = paths
                .into_iter()
                .map(|path| serde_json::json!({"path":path, "unavailable":"read output bound leaves no room; read this AGENTS.md directly"}))
                .collect();
        }
    }
    source_pages::encode_page(&output, structured, start_line, start_column)
}

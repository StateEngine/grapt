//! Shared presentation helpers for protocol-backed terminal clients.

pub mod assistant_footer;
mod queued_input;
pub use queued_input::lines as queued_input_lines;

mod approval;
pub mod permission;
pub mod status;
mod workspace_diff;
pub use workspace_diff::lines as workspace_diff_lines;
mod mcp_details;
pub use mcp_details::lines as mcp_detail_lines;
mod mcp_summary;
pub use mcp_summary::lines as mcp_summary_lines;

#[cfg(test)]
mod tests;

/// Flatten line breaks before bounding a single presentation row in cells.
fn truncate(text: &str, width: usize) -> String {
    let flattened: String = text
        .chars()
        .map(|ch| if ch == '\n' || ch == '\r' { ' ' } else { ch })
        .collect();
    crate::cells::cut(&flattened, width)
}

/// Expand tabs and make controls visible before shared row wrapping.
fn visible(line: &str) -> String {
    let mut out = String::new();
    for ch in line.chars() {
        match ch {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

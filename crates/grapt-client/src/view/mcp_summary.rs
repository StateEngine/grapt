//! Read-only MCP server summary for native command panes.
use super::{truncate, visible};
use crate::client::{Client, Connection};

pub fn lines<C: Connection>(client: &Client<C>, width: usize) -> Vec<String> {
    let mut lines = server_lines(client, width);
    lines.extend(client.mcp_resource_lines());
    lines
}

fn server_lines<C: Connection>(client: &Client<C>, width: usize) -> Vec<String> {
    let mut lines = vec![truncate("MCP SERVERS", width)];
    if client.mcp_catalog_pending() {
        lines.push(truncate(
            "  Loading MCP servers; startup questions remain answerable.",
            width,
        ));
        return lines;
    }
    if let Some(message) = client.mcp_refusal() {
        lines.push(refusal_line(message, width));
        return lines;
    }
    let rows = client.mcp_servers();
    if rows.is_empty() {
        lines.push(truncate("  (none)", width));
        return lines;
    }
    for (index, row) in rows.iter().enumerate() {
        let counts = row
            .tools
            .as_ref()
            .map(|tools| {
                let offered = tools.iter().filter(|tool| tool.omission.is_none()).count();
                let omitted = tools.len().saturating_sub(offered);
                let unit = if offered == 1 { "tool" } else { "tools" };
                if omitted == 0 {
                    format!(" ({offered} {unit})")
                } else {
                    format!(" ({offered} {unit}, {omitted} withheld)")
                }
            })
            .unwrap_or_default();
        lines.push(truncate(
            &visible(&format!(
                "{} {}  {}{counts}",
                if index == 0 { ">" } else { " " },
                row.name,
                row.status.word()
            )),
            width,
        ));
    }
    lines
}

/// Keep the faulting filename at the end of a refusal visible.
fn refusal_line(message: &str, width: usize) -> String {
    const MARKER: &str = "  ...";
    let line = format!("  {message}");
    if crate::cells::cells(&line) <= width {
        return line;
    }
    let allowance = width.saturating_sub(crate::cells::cells(MARKER));
    let anchored = format!("{MARKER}{}", crate::cells::tail(message, allowance));
    crate::cells::cut(&anchored, width)
}

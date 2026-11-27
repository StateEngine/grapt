//! Copyright 2025 OpenAI.
//! Apache-2.0. Modified by Grapt: bounded native rows, held publication
//! snapshot and explicit omissions; credentials and resource reads are separate.
use super::visible;
use crate::client::{Client, Connection};
pub fn lines<C: Connection>(client: &Client<C>, width: usize) -> Vec<String> {
    let mut lines = vec!["MCP DETAILS".to_owned()];
    let mut push = |text: &str| lines.extend(crate::cells::wrap(&visible(text), width));
    if client.mcp_catalog_pending() {
        push("Loading MCP servers; startup questions remain answerable.");
        return lines;
    }
    if let Some(error) = client.mcp_refusal() {
        push(error);
        return lines;
    }
    if client.mcp_servers().is_empty() {
        push("No MCP servers configured.");
        return lines;
    }
    for row in client.mcp_servers() {
        push(&format!("{}: {}", row.name, row.status.word()));
        if let Some(error) = row.status.error() {
            push(error);
        }
        match &row.tools {
            None => push("  Tools: not reported"),
            Some(tools) if tools.is_empty() => push("  Tools: none offered"),
            Some(tools) => {
                for tool in tools {
                    push(&match &tool.omission {
                        None => format!("  {}", tool.name),
                        Some(reason) => format!("  {} — withheld: {reason}", tool.name),
                    });
                }
            }
        }
    }
    push("Session permissions and policy also apply.");
    lines.extend(client.mcp_resource_lines());
    lines
}

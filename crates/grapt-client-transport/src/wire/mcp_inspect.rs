//! Native inspection supplements the retained closed MCP status catalog.
use super::*;
pub fn decode_mcp_inspection(bytes: &[u8]) -> Result<Vec<McpServerRow>, WireError> {
    let dto: grapt_api::u3a::McpInspectionResponseDto = decode_page(bytes, limits()?)?;
    Ok(dto
        .data()
        .get()
        .as_slice()
        .iter()
        .map(|entry| {
            let mut row = super::catalogs::mcp_server_row(entry.server().get());
            row.tools = Some(
                entry
                    .tools()
                    .get()
                    .as_slice()
                    .iter()
                    .map(|tool| McpToolRow {
                        name: tool.name().get().as_str().to_owned(),
                        omission: tool
                            .omission()
                            .as_present()
                            .map(|reason| reason.as_str().to_owned()),
                    })
                    .collect(),
            );
            row
        })
        .collect())
}
pub fn encode_mcp_inspection(
    project: &ProjectRow,
    rows: &[McpServerRow],
) -> Result<Vec<u8>, WireError> {
    let status = encode_mcp_servers(project, rows)?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&status).map_err(|error| WireError::Codec {
            reason: error.to_string(),
        })?;
    let servers = value["data"].as_array().ok_or_else(|| WireError::Codec {
        reason: "MCP status rows unreadable".to_owned(),
    })?;
    if servers.len() != rows.len() {
        return Err(WireError::Codec {
            reason: "MCP status row count changed".to_owned(),
        });
    }
    let mut data = Vec::with_capacity(rows.len());
    for (server, row) in servers.iter().zip(rows) {
        let tools = row.tools.as_deref().ok_or_else(|| WireError::Codec {
            reason: "MCP tool catalog has not been read".to_owned(),
        })?;
        let tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|tool| {
                let mut value = serde_json::json!({"name":tool.name});
                if let Some(reason) = &tool.omission {
                    value["omission"] = serde_json::json!(reason);
                }
                value
            })
            .collect();
        data.push(serde_json::json!({"server":server,"tools":tools}));
    }
    value["data"] = serde_json::Value::Array(data);
    let bytes = serde_json::to_vec(&value).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    let _validated: grapt_api::u3a::McpInspectionResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

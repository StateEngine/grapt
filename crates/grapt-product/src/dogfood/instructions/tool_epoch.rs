//! SC-CACHE-02: tool definitions share the existing durable context epoch.
//! Only model-visible descriptions are retained here, never launch identities,
//! connections or permission grants. Dispatch always uses the live snapshot.

use grapt_instruction::{InstructionValue, SourceObservation};
use grapt_provider::{ToolDefinition, ToolInputFormat};
use serde::{Deserialize, Serialize};

pub(super) const FROZEN: &str = "core/mcp-tool-epoch";
pub(super) const CURRENT: &str = "core/mcp-tools";
const MAX_BYTES: usize = 1024 * 1024;
// Matches the provider request ceiling; plugin generations remain live.
const MAX_TOOLS: usize = 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTool {
    name: String,
    description: Option<String>,
    input_schema: String,
    grammar: Option<String>,
}

pub(super) fn metadata(key: &str) -> bool {
    matches!(key, FROZEN | CURRENT)
}

pub(super) fn observations(
    tools: &[ToolDefinition],
    failed: bool,
) -> Result<Vec<SourceObservation>, String> {
    if tools.len() > MAX_TOOLS {
        return Err("MCP tool epoch exceeds its tool bound".to_owned());
    }
    let stored: Vec<_> = tools
        .iter()
        .map(|tool| StoredTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
            grammar: match &tool.input_format {
                ToolInputFormat::Json => None,
                ToolInputFormat::Text { grammar } => Some(grammar.clone()),
            },
        })
        .collect();
    let value = serde_json::to_value(stored).map_err(|error| error.to_string())?;
    if value.to_string().len() > MAX_BYTES {
        return Err("MCP tool epoch exceeds 1 MiB".to_owned());
    }
    Ok(vec![
        if failed {
            SourceObservation::Unavailable {
                key: super::key(FROZEN)?,
            }
        } else {
            SourceObservation::AvailableOnce {
                key: super::key(FROZEN)?,
                value: InstructionValue::Structured(value.clone()),
            }
        },
        if failed {
            SourceObservation::Unavailable {
                key: super::key(CURRENT)?,
            }
        } else {
            SourceObservation::Available {
                key: super::key(CURRENT)?,
                value: InstructionValue::Structured(value),
            }
        },
    ])
}

pub(super) fn decode(value: &str) -> Result<Vec<ToolDefinition>, String> {
    if value.len() > MAX_BYTES {
        return Err("stored MCP tool epoch exceeds 1 MiB".to_owned());
    }
    let tools: Vec<StoredTool> =
        serde_json::from_str(value).map_err(|_| "stored MCP tool epoch is malformed".to_owned())?;
    if tools.len() > MAX_TOOLS {
        return Err("stored MCP tool epoch exceeds its tool bound".to_owned());
    }
    Ok(tools
        .into_iter()
        .map(|tool| ToolDefinition {
            name: tool.name,
            description: tool.description,
            input_schema: tool.input_schema,
            input_format: match tool.grammar {
                Some(grammar) => ToolInputFormat::Text { grammar },
                None => ToolInputFormat::Json,
            },
        })
        .collect())
}

pub(super) fn update(value: &str) -> String {
    format!(
        "MCP catalog update. The Session's original tool definitions remain in effect. \
         Tools absent from the current catalog are unavailable. Use current parameter \
         schemas below; new tools can be reached through execute when exposed there, \
         otherwise start a new Session to offer them directly.\n{value}"
    )
}

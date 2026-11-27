//! Command templates come from the existing location-scoped catalog DTO.
use super::*;
pub use grapt_api::u3a::CommandOriginDto as CommandOrigin;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTemplate {
    pub name: String,
    pub description: String,
    pub template: String,
    /// R supports prompt templates; specialized execution stays explicit.
    pub unsupported: bool,
    pub origin: grapt_api::u3a::CommandOriginDto,
    pub source: String,
    pub unavailable: Option<String>,
    pub mcp: Option<McpPrompt>,
    pub claude: Option<ClaudePluginPaths>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudePluginPaths {
    pub root: String,
    pub data: String,
    pub project: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPrompt {
    pub server: String,
    pub name: String,
    pub arguments: Vec<McpArgument>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpArgument {
    pub name: String,
    pub required: bool,
}
#[derive(Debug)]
pub struct CommandCatalog {
    pub owner: FormLocation,
    pub rows: Vec<CommandTemplate>,
    pub warnings: Vec<String>,
    pub scope: Option<String>,
}

/// Only the public capture is rendered: keys and ambient core/instructions.
pub fn decode_instruction_context(bytes: &[u8]) -> Result<Vec<String>, WireError> {
    let dto: grapt_api::u3a::InstructionInspectionDto = decode_page(bytes, limits()?)?;
    let mut lines = vec![
        "INSTRUCTION CONTEXT".into(),
        "Last-captured instruction state; not a disk refresh.".into(),
    ];
    if !*dto.captured().get() {
        lines.push("Instructions have not been captured yet.".into());
        return Ok(lines);
    }
    lines.push(String::new());
    lines.push("Keys (private entry values excluded):".into());
    let keys = dto.keys().get().as_slice();
    if keys.is_empty() {
        lines.push("(none)".into());
    } else {
        lines.extend(keys.iter().map(|key| format!("- {}", key.as_str())));
    }
    lines.push(String::new());
    lines.push("Ambient core/instructions (file headings and content):".into());
    let ambient = dto.ambient().get().as_str();
    if ambient.is_empty() {
        lines.push("(none)".into());
    } else {
        lines.extend(ambient.lines().map(str::to_owned));
    }
    Ok(lines)
}

pub fn decode_commands(bytes: &[u8]) -> Result<CommandCatalog, WireError> {
    let dto: grapt_api::u3a::CommandInspectionResponseDto = decode_page(bytes, limits()?)?;
    let location = dto.location().get();
    let owner = FormLocation {
        project: location.project().get().id().get().clone(),
        directory: location.directory().get().as_str().to_owned(),
    };
    let rows = dto
        .data()
        .get()
        .as_slice()
        .iter()
        .map(|inspection| {
            let row = inspection.command().get();
            let source = inspection.source().get().as_str();
            let unavailable = inspection
                .unavailable()
                .as_present()
                .map(|v| v.as_str().to_owned());
            let name = row.name().get().as_str();
            let template = row.template().get().as_str();
            let description = row.description().as_present().map_or("", |v| v.as_str());
            if name.is_empty()
                || name.len() > 512
                || name.chars().any(|c| c.is_whitespace() || c.is_control())
                || template.len() > 65_536
                || description.len() > 8192
                || source.is_empty()
                || source.len() > 4096
                || source.chars().any(char::is_control)
                || unavailable.as_ref().is_some_and(|v| v.len() > 4096)
            {
                return Err(WireError::Invalid {
                    what: "command catalog",
                    value: "name, description or template exceeds its bound".into(),
                });
            }
            let mcp = inspection.mcp().as_present().map(|binding| McpPrompt {
                server: binding.server().get().as_str().into(),
                name: binding.name().get().as_str().into(),
                arguments: binding
                    .arguments()
                    .get()
                    .as_slice()
                    .iter()
                    .map(|argument| McpArgument {
                        name: argument.name().get().as_str().into(),
                        required: *argument.required().get(),
                    })
                    .collect(),
            });
            if (inspection.origin().get() == &CommandOrigin::Mcp) != mcp.is_some() {
                return Err(WireError::Invalid {
                    what: "MCP command provenance",
                    value: "binding and origin differ".into(),
                });
            }
            let claude = inspection
                .claude()
                .as_present()
                .map(|paths| ClaudePluginPaths {
                    root: paths.root().get().as_str().to_owned(),
                    data: paths.data().get().as_str().to_owned(),
                    project: paths.project().get().as_str().to_owned(),
                });
            Ok(CommandTemplate {
                claude,
                mcp,
                name: name.into(),
                origin: *inspection.origin().get(),
                source: source.into(),
                unavailable,
                template: template.into(),
                description: description.into(),
                unsupported: row.agent().as_present().is_some()
                    || row.model().as_present().is_some()
                    || row.subtask().as_present() == Some(&true)
                    || template.contains("!`"),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let scope = dto.mcp_scope().as_present().map(|v| v.as_str().to_owned());
    if scope
        .as_ref()
        .is_some_and(|scope| scope.len() != 64 || !scope.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(WireError::Invalid {
            what: "MCP scope",
            value: "invalid digest".into(),
        });
    }
    Ok(CommandCatalog {
        owner,
        rows,
        scope,
        warnings: dto
            .warnings()
            .as_present()
            .map(|v| v.as_slice().iter().map(|s| s.as_str().to_owned()).collect())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_command_dto_keeps_workspace_and_rejects_unbounded_templates(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut value = serde_json::json!({"location":{"directory":"/repo","project":{"id":"prj_commands","directory":"/repo"}},"data":[{"name":"review","template":"Review $ARGUMENTS"},{"name":"agent-task","template":"Task","subtask":true}]});
        for row in value["data"].as_array_mut().ok_or("rows")? {
            *row = serde_json::json!({"command":row.clone(),"origin":"builtin","source":"Grapt built-in"});
        }
        let catalog = decode_commands(&serde_json::to_vec(&value)?)?;
        let (owner, rows) = (catalog.owner, catalog.rows);
        assert_eq!(owner.directory, "/repo");
        assert!(!rows[0].unsupported);
        assert!(rows[1].unsupported);
        value["data"][0]["command"]["template"] = "x".repeat(65_537).into();
        assert!(decode_commands(&serde_json::to_vec(&value)?).is_err());
        Ok(())
    }
    #[test]
    fn old_catalog_cannot_silently_grant_builtin_authority(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let value = serde_json::json!({"location":{"directory":"/repo","project":{"id":"prj_commands","directory":"/repo"}},"data":[{"name":"review","template":"untrusted"}]});
        assert!(decode_commands(&serde_json::to_vec(&value)?).is_err());
        Ok(())
    }
}

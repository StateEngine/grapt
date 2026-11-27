//! Application selection and publication metadata for hosted MCP tools.
use super::{LaunchIdentity, ServerEntry};
use grapt_mcp::DiscoveredTool;
/// How many MCP tools one execution may publish, across ALL its servers, and
/// how many bytes of schema they may carry between them.
///
/// The per-server bounds in `McpBounds` cap what ONE server can inflate.
/// Neither caps the aggregate, and the aggregate is what reaches a prompt:
/// eight servers each just inside their own tool bound would publish a catalog
/// no single server was ever allowed to. These bounds are on the sum, and they
/// are deliberately smaller than the per-server product.
pub(crate) const MAX_PUBLISHED_TOOLS: usize = 256;
pub(crate) const MAX_PUBLISHED_SCHEMA_BYTES: usize = 512 * 1024;
#[cfg(test)]
pub(crate) use grapt_mcp_host::publication::{qualified_tool_name, shadows_builtin};
pub(crate) use grapt_mcp_host::publication::{sanitize_name, WithheldTool};
use std::collections::BTreeMap;
pub(crate) type PublishedTools = grapt_mcp_host::publication::PublishedTools<LaunchIdentity>;
pub(crate) type McpRoute = grapt_mcp_host::publication::McpRoute<LaunchIdentity>;
pub(crate) type ConnectedCatalog<'a> =
    grapt_mcp_host::publication::ConnectedCatalog<'a, LaunchIdentity>;

/// Project every connected server's catalog into the definitions a model is
/// shown, qualified by server.
///
/// Only CONNECTED servers contribute. A disabled or unusable server offers no
/// tools, and offering a model a tool that cannot be called would produce a
/// call this product must then refuse — a failure invented by the prompt
/// rather than reported by it.
pub(crate) fn tool_definitions(
    entries: &BTreeMap<String, ServerEntry>,
    builtins: &[&str],
) -> PublishedTools {
    tool_definitions_for_session(entries, builtins, false)
}

pub(crate) fn tool_definitions_for_session(
    entries: &BTreeMap<String, ServerEntry>,
    builtins: &[&str],
    bound_resume: bool,
) -> PublishedTools {
    let connected: Vec<ConnectedCatalog<'_>> = entries
        .iter()
        .filter_map(|(server, entry)| match entry {
            // A connection a caller has OBSERVED dead publishes nothing. Its
            // catalog describes a pipe that is gone.
            ServerEntry::Live(live) if live.observed_unusable().is_some() => None,
            ServerEntry::Live(live) => Some(ConnectedCatalog {
                server: server.as_str(),
                launch: live.launch().clone(),
                catalog: live.catalog(),
            }),
            ServerEntry::Disabled | ServerEntry::Unusable(..) => None,
        })
        .collect();
    let mut published = tool_definitions_from_bound(&connected, builtins, bound_resume);
    for (server, entry) in entries {
        if let ServerEntry::Live(live) = entry {
            if live.observed_unusable().is_none() {
                if let Some(text) = live
                    .identity()
                    .instructions()
                    .filter(|text| !text.trim().is_empty())
                {
                    published
                        .instructions
                        .insert(server.clone(), text.to_owned());
                }
            }
        }
    }
    published
}

#[cfg(test)]
pub(crate) fn tool_definitions_from(
    connected: &[ConnectedCatalog<'_>],
    builtins: &[&str],
) -> PublishedTools {
    tool_definitions_from_bound(connected, builtins, false)
}

pub(crate) fn tool_definitions_from_bound(
    connected: &[ConnectedCatalog<'_>],
    builtins: &[&str],
    bound_resume: bool,
) -> PublishedTools {
    grapt_mcp_host::publication::tool_definitions_from(
        connected,
        builtins,
        &DescriptionPolicy { bound_resume },
    )
}

struct DescriptionPolicy {
    bound_resume: bool,
}

impl grapt_mcp_host::publication::PublicationPolicy for DescriptionPolicy {
    fn max_tools(&self) -> usize {
        MAX_PUBLISHED_TOOLS
    }

    fn max_schema_bytes(&self) -> usize {
        MAX_PUBLISHED_SCHEMA_BYTES
    }

    fn max_description_bytes(&self) -> usize {
        grapt_provider::tool_eligibility::MAX_TOOL_DESCRIPTION_BYTES
    }

    fn description(&self, server: &str, tool: &DiscoveredTool) -> Option<String> {
        if self.bound_resume
            && server == "stateengine"
            && tool.name().as_str() == "stateengine_resume"
        {
            Some(super::super::stateengine_resume::BOUND_RESUME_GUIDANCE.to_owned())
        } else {
            tool.description().map(str::to_owned)
        }
    }
}

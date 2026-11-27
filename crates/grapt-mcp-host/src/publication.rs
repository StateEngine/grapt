//! What a set of connected servers publishes to a model.
//!
//! Naming, the refusals that keep a published name meaning exactly one tool,
//! and the bounds on what a whole catalog may put in front of a model. Split
//! out of `mcp.rs` under the workspace god-file ceiling; this is the
//! publication concern and it has no transport in it.

use std::collections::BTreeMap;

use grapt_mcp::{DiscoveredTool, McpToolName};
use grapt_provider::ToolDefinition;

/// Application-owned metadata policy applied before aggregate publication bounds.
pub trait PublicationPolicy {
    fn max_tools(&self) -> usize;
    fn max_schema_bytes(&self) -> usize;
    fn max_description_bytes(&self) -> usize;
    fn description(&self, server: &str, tool: &DiscoveredTool) -> Option<String>;
}

/// The name one discovered tool is known by outside its server.
///
/// This is NOT a name this product invents. The frozen oracle mints it in
/// The oracle's `mcp.ts:20-21` as `${namespace(server)}_${tool}`,
/// with every byte outside `[a-zA-Z0-9_-]` replaced by `_` in BOTH halves, and
/// `mcp.ts:114` uses that same qualified name as the permission ACTION.
///
/// So one identity serves both purposes, which is the property worth keeping:
/// the name a model is shown, the name a user approves, and the name a receipt
/// records are the same string. A separate display name and permission key
/// would let a user approve one thing and a server run another.
pub fn qualified_tool_name(server: &str, tool: &str) -> String {
    format!("{}_{}", sanitize_name(server), sanitize_name(tool))
}

pub fn sanitize_name(raw: &str) -> String {
    raw.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Whether a qualified MCP name could be mistaken for a built-in tool.
///
/// Compare the complete built-in catalog: `read_lines` can also be produced by
/// MCP server `read` and tool `lines`. Publication withholds a colliding
/// definition and route so it cannot answer for an approved built-in tool.
pub fn shadows_builtin(qualified: &str, builtins: &[&str]) -> bool {
    builtins.contains(&qualified)
}

/// What a set of connected servers publishes to a model, and what it could
/// not publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedTools<L> {
    /// Guidance from the same connected server generation as the live routes.
    pub instructions: BTreeMap<String, String>,
    /// The refresh failed or was incomplete. Live routes may be unavailable,
    /// while the model retains its previous advertised catalog.
    pub refresh_failed: bool,
    /// Declared executable servers withheld because their source is a repository.
    pub blocked_servers: Vec<String>,
    pub codemode_servers: std::collections::BTreeSet<String>,
    pub definitions: Vec<ToolDefinition>,
    /// Tools dropped because a built-in already answers to their name.
    pub shadowed: Vec<WithheldTool>,
    /// Tools dropped because MORE THAN ONE (server, tool) pair produces their
    /// name. See [`tool_definitions_from`] for why these are refused rather
    /// than resolved. Every claimant is listed, because either of them could
    /// be the one an operator renames.
    pub ambiguous: Vec<WithheldTool>,
    /// Tools dropped because the AGGREGATE bound was reached. They are real
    /// and unambiguous; there was simply no room.
    pub unpublished: Vec<WithheldTool>,
    /// The exact origin of every PUBLISHED name: which server, and the tool
    /// name that server itself published.
    ///
    /// Dispatch and approval both resolve through this rather than by taking
    /// the qualified string apart again. The join character is also the
    /// sanitiser, so splitting `a_b_c` cannot recover which half was the
    /// server — the only way to know is to remember what it was built from.
    /// A name with no route here is a name this product will not run.
    pub routes: BTreeMap<String, McpRoute<L>>,
}

/// A tool that was discovered but NOT offered to a model.
///
/// The originating server is carried rather than recovered from the qualified
/// name. Recovering it by prefix is wrong: `a_b_tool` starts with `a_` as well
/// as `a_b_`, so a server named `a` would be told about a tool that was never
/// its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithheldTool {
    pub server: String,
    pub name: String,
}

/// Where a published tool name actually goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRoute<L> {
    pub description: Option<String>,
    pub output_schema: Option<String>,
    pub server: String,
    pub tool: McpToolName,
    /// What was actually launched for this route, and under which generation.
    ///
    /// Ratified law C5 binds approval to the exact program and argument
    /// vector. A configured name is neither, and neither is a declared
    /// command: a bare program is resolved before it runs. This carries the
    /// resolved facts and declaration so consent binds what ran. Reconnecting
    /// the same server preserves that grant; a declaration change invalidates it.
    pub launch: L,
}

/// One connected server's catalog, with the command it was started from.
#[derive(Debug)]
pub struct ConnectedCatalog<'a, L> {
    pub server: &'a str,
    pub launch: L,
    pub catalog: &'a [DiscoveredTool],
}

/// The pure projection over connected catalogs, over catalogs rather than over live
/// connections. Split out so the projection can be exercised without spawning
/// a child process or owning a real channel — the rule being tested is about
/// names and authority, not about who holds a file descriptor.
///
/// # Two ways a qualified name fails to identify one tool
///
/// **A built-in already answers to it.** Dropped, and RETURNED: swallowing it
/// silently leaves an operator wondering why a configured tool never appears.
///
/// **Two different (server, tool) pairs produce it.** The qualified form joins
/// with `_` and sanitises to `_`, so it is NOT injective: server `a_b` with
/// tool `c` and server `a` with tool `b_c` both yield `a_b_c`, with no
/// punctuation involved at all, and servers `a/b` and `a_b` become the same
/// prefix once sanitised.
///
/// Both are REFUSED rather than resolved, because that name is not only a
/// label shown to a model — it is the permission ACTION
/// (the oracle's `mcp.ts:114`). Publishing one and dropping the
/// other would let a user approve `a_b_c` believing one server while a
/// different server's tool ran. Equal public strings do not establish equal
/// authority, so an ambiguous name identifies no tool and publishes none.
///
/// Refusing BOTH rather than the later one is deliberate: which entry is
/// "later" is itself iteration order, and a rule whose outcome depends on map
/// ordering is the same defect wearing a different hat.
pub fn tool_definitions_from<L: Clone>(
    connected: &[ConnectedCatalog<'_, L>],
    builtins: &[&str],
    policy: &impl PublicationPolicy,
) -> PublishedTools<L> {
    let mut claims: BTreeMap<String, usize> = BTreeMap::new();
    for entry in connected {
        for tool in entry.catalog {
            *claims
                .entry(qualified_tool_name(entry.server, tool.name().as_str()))
                .or_default() += 1;
        }
    }

    let mut published = PublishedTools::default();
    let mut schema_bytes = 0usize;
    let mut stopped = false;
    for entry in connected {
        for tool in entry.catalog {
            let name = qualified_tool_name(entry.server, tool.name().as_str());
            let withheld = WithheldTool {
                server: entry.server.to_owned(),
                name: name.clone(),
            };
            if shadows_builtin(&name, builtins) {
                published.shadowed.push(withheld);
                continue;
            }
            if claims.get(&name).copied().unwrap_or_default() > 1 {
                // EVERY claimant is recorded, not just the first: an operator
                // has to rename one of them and cannot be told which without
                // being told about both.
                published.ambiguous.push(withheld);
                continue;
            }
            // Carried as the server published it, already bounded by
            // `accept_tools` at discovery. Re-deriving a schema here would
            // describe the tool as this product imagines it rather than as the
            // server declared it.
            let input_schema = tool.input_schema().to_string();
            let output_schema = tool.output_schema().map(ToString::to_string);
            let description = policy.description(entry.server, tool);
            let carried_bytes = input_schema
                .len()
                .saturating_add(output_schema.as_ref().map_or(0, String::len))
                .saturating_add(description.as_ref().map_or(0, String::len));
            // POLICY, chosen and not merely described: crossing EITHER bound
            // stops publication. The crossing tool and every tool after it are
            // refused and RECORDED.
            //
            // The alternative — skip the tool that does not fit and keep
            // admitting smaller ones after it — packs more in, and was what
            // this code actually did while the comment claimed otherwise. It
            // is rejected because it makes the published catalog depend on the
            // SIZES of tools rather than their order, so an operator cannot
            // predict or explain which of their tools appear. "Truncated at
            // the first tool that did not fit" is a sentence a user can act on.
            if stopped
                || description.as_ref().is_some_and(|description| {
                    description.is_empty() || description.len() > policy.max_description_bytes()
                })
                || published.definitions.len() >= policy.max_tools()
                || schema_bytes.saturating_add(carried_bytes) > policy.max_schema_bytes()
            {
                stopped = true;
                published.unpublished.push(withheld);
                continue;
            }
            schema_bytes = schema_bytes.saturating_add(carried_bytes);
            published.routes.insert(
                name.clone(),
                McpRoute {
                    description: description.clone(),
                    output_schema,
                    server: entry.server.to_owned(),
                    tool: tool.name().clone(),
                    launch: entry.launch.clone(),
                },
            );
            published.definitions.push(ToolDefinition {
                input_format: grapt_provider::ToolInputFormat::Json,
                name,
                description,
                input_schema,
            });
        }
    }
    published
}

impl<L> Default for PublishedTools<L> {
    fn default() -> Self {
        Self {
            instructions: BTreeMap::new(),
            refresh_failed: false,
            blocked_servers: Vec::new(),
            codemode_servers: std::collections::BTreeSet::new(),
            definitions: Vec::new(),
            shadowed: Vec::new(),
            ambiguous: Vec::new(),
            unpublished: Vec::new(),
            routes: BTreeMap::new(),
        }
    }
}

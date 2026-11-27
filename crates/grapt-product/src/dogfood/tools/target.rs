//! Prepared built-in effects, bound to the values checked before authorization.

use super::*;

#[derive(Debug, Clone)]
pub(super) enum EffectTarget {
    Plan,
    Ask(ask::Arguments),
    AgentFollowup {
        target: SessionId,
        text: String,
    },
    AgentMessage {
        target: SessionId,
        text: String,
    },
    WebFetch(grapt_web::WebUrl),
    AgentWait {
        timeout_ms: u64,
    },
    AgentList {
        after: Option<grapt_types::SubagentOperationId>,
    },
    CodeMode {
        request: grapt_codemode::ExecuteRequest,
        nested: grapt_runtime::NestedTools,
    },
    Read(PathBuf),
    Image(PathBuf),
    Edit(PathBuf),
    Coding(coding::CodingTarget),
    TerminalExec(terminal::ExecSpec),
    TerminalStdin(terminal::StdinSpec),
    Shell {
        command: String,
        program: PathBuf,
        environment: shell_environment::Environment,
    },
    /// A child Session that answers one question and settles.
    Subagent {
        task: String,
    },
    AgentSpawn(agent_spawn::SpawnTask),
    AgentInterrupt {
        child: SessionId,
    },
    /// Discover installed metadata, or load one skill/resource by id.
    Skill(skill::Request),
    ReadHistory(read_history::Request),
    History {
        call: Option<ToolCallId>,
        cursor: Option<u64>,
        offset: Option<u64>,
    },
    /// A call on a configured MCP server, bound to the exact route it was
    /// published from rather than to the qualified name a model produced.
    Mcp {
        route: super::super::mcp::McpRoute,
        arguments: serde_json::Value,
    },
    Plugin {
        route: grapt_plugin::PluginToolRoute,
        arguments: serde_json::Value,
    },
    /// One integration command method's exact argv, run without a shell.
    IntegrationCommand {
        argv: Vec<String>,
    },
}

#[derive(Debug)]
pub(super) struct PreparedEffect {
    pub(super) target: EffectTarget,
    pub(super) capability: CapabilityId,
    /// What the user is asked to approve. A `String` rather than a static
    /// name because an MCP action is the qualified tool name, which is
    /// discovered rather than compiled in.
    pub(super) action: String,
    pub(super) resource: String,
}

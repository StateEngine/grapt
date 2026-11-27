//! The native client's closed HTTP operation vocabulary and path resolution.

use super::ContractError;
use crate::wire::Spelled;

/// A route this client drives, named by its frozen `operationId`.
///
/// The client never types a path literal at a call site. It names an
/// operation and this table resolves the path, so a path that drifts from the
/// frozen record drifts in exactly one place and one test catches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operation {
    /// `v2.health.get`
    HealthGet,
    IntegrationList,
    IntegrationKeyConnect,
    IntegrationOAuthConnect,
    IntegrationOAuthStatus,
    IntegrationOAuthCancel,
    IntegrationOAuthComplete,
    CredentialRename,
    CredentialRemove,

    /// `v2.provider.list` — the catalog a session is created against.
    ProviderList,
    /// `v2.project.list` — the project gateway's source.
    ProjectList,
    /// `v2.project.current` — which project a directory resolves to.
    ProjectCurrent,
    /// `v2.agent.list` — the agent roster for a project location.
    AgentList,
    /// `v2.model.list` — the model picker's source for a project location.
    ModelList,
    /// `v2.model.default` — the model a location starts on.
    ModelDefault,
    /// `v2.command.list` — location-scoped prompt templates.
    CommandList,
    /// Native provenance and trust inspection.
    CommandInspect,
    /// Native read-only last-captured instruction inspection.
    InstructionInspect,
    /// `v2.session.switchAgent` — bind one session to an agent.
    SessionAgentSelect,
    /// `v2.session.switchModel` — bind one session to a model.
    SessionModelSelect,
    /// `v2.form.request.list` — pending forms across sessions.
    FormRequestList,
    /// Approved native authenticated location-scoped elicitation routes.
    GlobalFormGet,
    GlobalFormState,
    GlobalFormReply,
    GlobalFormCancel,
    /// `v2.session.form.list` — pending forms in one session.
    SessionFormList,
    /// `v2.session.form.get` — one form, by its own identity.
    SessionFormGet,
    /// `v2.session.form.state` — a form's settlement state, separately from
    /// its content.
    ///
    /// This is the route that answers "is this still open?" without re-reading
    /// the form body, and it is the one that keeps a SUSPENDED form legible: a
    /// form the client can no longer see in a list is not thereby cancelled.
    SessionFormState,
    /// `v2.session.form.reply` — settle a form by answering it.
    SessionFormReply,
    /// `v2.session.form.cancel` — settle a form by declining it.
    ///
    /// Cancelling is the client's ONLY other settlement. "Not answered" is not
    /// a third one: an unanswered form stays open, and a suspended form is
    /// still replyable later.
    SessionFormCancel,
    /// `v2.session.list` — the session picker's source.
    SessionList,
    /// `v2.session.create`
    SessionCreate,
    /// Fork a selected conversation prefix.
    SessionFork,
    SideQuestion,
    /// `v2.session.get` — selecting one session.
    SessionGet,
    /// `v2.session.prompt` — prompt submission.
    SessionPrompt,
    /// `v2.session.pending.list` — admitted inputs awaiting promotion.
    SessionPendingList,
    /// `v2.session.interrupt` — the cancellation affordance.
    SessionInterrupt,
    /// `v2.session.compact` — the composer's `/compact` command.
    SessionCompact,
    /// Native conversation cutoff preview and commit.
    SessionRewindPreview,
    SessionRewind,
    SessionUndoPreview,
    SessionUndo,
    /// Native read-only compatibility preview and explicit user-consent creation.
    SessionSandboxWorktreeConfigGet,
    SessionSandboxWorktreeConfigCreate,
    /// Native: record nested tool runs lost with their process as unknown.
    SessionReconcile,
    /// `v2.session.rename` — the composer's `/rename` command.
    SessionRename,
    /// Delete through the existing authenticated session lifecycle contract.
    SessionDelete,
    /// `v2.message.list` — the durable re-list that reconciles a stream gap.
    MessageList,
    /// One coherent snapshot at a committed position: rows, permissions,
    /// forms, execution and open live text. Not in the frozen record; an
    /// older service answers 404 and the client keeps its legacy reads.
    SessionSnapshot,
    /// `v2.event.subscribe` — volatile SSE, no replay cursor.
    EventSubscribe,
    /// `v2.permission.request.list` — pending asks across sessions.
    PermissionRequestList,
    SandboxGrantList,
    SandboxGrantRevoke,
    /// Which model this session's SUBAGENTS run as.
    ///
    /// Read-only here. The client presents the selection; changing it is a
    /// separate operation this client does not perform.
    SessionSubagentModelGet,
    /// `v2.session.permission.list` — pending asks in one session.
    SessionPermissionList,
    /// `v2.session.permission.create` — create one durable ask.
    SessionPermissionCreate,
    /// `v2.session.permission.get`
    SessionPermissionGet,
    /// `v2.session.permission.reply` — all four reply scopes share one route.
    SessionPermissionReply,
    /// `v2.mcp.list` — the configured MCP servers and their connection status.
    ///
    /// Read-only: this client shows the connection truth it is given. The
    /// connect and disconnect routes are separate operations it does not name.
    McpList,
    /// Native read-only backend workspace diff.
    WorkspaceDiff,
    /// Native authenticated Git checkout management.
    WorkspaceWorktree,
    ClaudePlugin,

    /// Native read-only MCP tool inspection.
    McpInspect,
    /// Native authenticated MCP sign-in and sign-out.
    McpAuth,
    McpContent,
    /// Native persistent user MCP enablement.
    McpConfiguration,
    /// Native location-scoped connection retirement and refresh.
    McpReconnect,
    /// Native preview of one exact project-server approval.
    McpApproval,
    /// Approved resolved selected/effective read (PRESET-AT-LAUNCH).
    SessionPresetGet,
    SessionPresetSelect,
}

/// The HTTP method a frozen operation is reached by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// The `{sessionID}` placeholder exactly as the frozen record spells it.
const SESSION_PLACEHOLDER: &str = "{sessionID}";
/// The `{requestID}` placeholder exactly as the frozen record spells it.
const REQUEST_PLACEHOLDER: &str = "{requestID}";
/// The `{formID}` placeholder exactly as the frozen record spells it.
const FORM_PLACEHOLDER: &str = "{formID}";

impl Operation {
    /// Every operation this client is allowed to reach, in journey order.
    ///
    /// The route-closure witness derives its expectation from THIS slice. An
    /// operation absent from here is one the client may not drive, and an
    /// operation here that the frozen record does not declare fails the
    /// closure test.
    pub const ALL: &'static [Operation] = &[
        Operation::HealthGet,
        Operation::IntegrationList,
        Operation::IntegrationKeyConnect,
        Operation::IntegrationOAuthConnect,
        Operation::IntegrationOAuthStatus,
        Operation::IntegrationOAuthCancel,
        Operation::IntegrationOAuthComplete,
        Operation::CredentialRename,
        Operation::CredentialRemove,
        Operation::ProviderList,
        Operation::ProjectList,
        Operation::ProjectCurrent,
        Operation::AgentList,
        Operation::ModelList,
        Operation::ModelDefault,
        Operation::CommandList,
        Operation::CommandInspect,
        Operation::InstructionInspect,
        Operation::SessionList,
        Operation::SessionCreate,
        Operation::SessionFork,
        Operation::SideQuestion,
        Operation::SessionGet,
        Operation::SessionAgentSelect,
        Operation::SessionModelSelect,
        Operation::FormRequestList,
        Operation::GlobalFormGet,
        Operation::GlobalFormState,
        Operation::GlobalFormReply,
        Operation::GlobalFormCancel,
        Operation::SessionFormList,
        Operation::SessionFormGet,
        Operation::SessionFormState,
        Operation::SessionFormReply,
        Operation::SessionFormCancel,
        Operation::SessionPrompt,
        Operation::SessionPendingList,
        Operation::SessionInterrupt,
        Operation::SessionCompact,
        Operation::SessionRewindPreview,
        Operation::SessionRewind,
        Operation::SessionUndoPreview,
        Operation::SessionUndo,
        Operation::SessionSandboxWorktreeConfigGet,
        Operation::SessionSandboxWorktreeConfigCreate,
        Operation::SessionReconcile,
        Operation::SessionRename,
        Operation::SessionDelete,
        Operation::MessageList,
        Operation::SessionSnapshot,
        Operation::EventSubscribe,
        Operation::PermissionRequestList,
        Operation::SandboxGrantList,
        Operation::SandboxGrantRevoke,
        Operation::SessionSubagentModelGet,
        Operation::SessionPermissionList,
        Operation::SessionPermissionCreate,
        Operation::SessionPermissionGet,
        Operation::SessionPermissionReply,
        Operation::McpList,
        Operation::WorkspaceDiff,
        Operation::WorkspaceWorktree,
        Operation::ClaudePlugin,
        Operation::McpInspect,
        Operation::McpAuth,
        Operation::McpContent,
        Operation::McpConfiguration,
        Operation::McpReconnect,
        Operation::McpApproval,
        Operation::SessionPresetGet,
        Operation::SessionPresetSelect,
    ];

    /// The frozen `operationId`.
    pub fn operation_id(self) -> &'static str {
        match self {
            Operation::HealthGet => "v2.health.get",
            Operation::IntegrationList => "v2.integration.list",
            Operation::IntegrationKeyConnect => "v2.integration.connect.key",
            Operation::IntegrationOAuthConnect => "v2.integration.oauth.connect",
            Operation::IntegrationOAuthStatus => "v2.integration.oauth.status",
            Operation::IntegrationOAuthCancel => "v2.integration.oauth.cancel",
            Operation::IntegrationOAuthComplete => "v2.integration.oauth.complete",
            Operation::CredentialRename => "v2.credential.update",
            Operation::CredentialRemove => "v2.credential.remove",

            Operation::ProviderList => "v2.provider.list",
            Operation::ProjectList => "v2.project.list",
            Operation::ProjectCurrent => "v2.project.current",
            Operation::AgentList => "v2.agent.list",
            Operation::ModelList => "v2.model.list",
            Operation::ModelDefault => "v2.model.default",
            Operation::CommandList => "v2.command.list",
            Operation::CommandInspect => "grapt.command.inspect",
            Operation::InstructionInspect => "grapt.instructions.inspect",
            Operation::SessionAgentSelect => "v2.session.switchAgent",
            Operation::SessionModelSelect => "v2.session.switchModel",
            Operation::FormRequestList => "v2.form.request.list",
            Operation::GlobalFormGet => "grapt.form.get",
            Operation::GlobalFormState => "grapt.form.state",
            Operation::GlobalFormReply => "grapt.form.reply",
            Operation::GlobalFormCancel => "grapt.form.cancel",
            Operation::SessionFormList => "v2.session.form.list",
            Operation::SessionFormGet => "v2.session.form.get",
            Operation::SessionFormState => "v2.session.form.state",
            Operation::SessionFormReply => "v2.session.form.reply",
            Operation::SessionFormCancel => "v2.session.form.cancel",
            Operation::SessionList => "v2.session.list",
            Operation::SessionCreate => "v2.session.create",
            Operation::SessionFork => "v2.session.fork",
            Operation::SideQuestion => "v2.session.side",
            Operation::SessionGet => "v2.session.get",
            Operation::SessionPrompt => "v2.session.prompt",
            Operation::SessionPendingList => "v2.session.pending.list",
            Operation::SessionInterrupt => "v2.session.interrupt",
            Operation::SessionCompact => "v2.session.compact",
            Operation::SessionRewindPreview => "grapt.session.rewind.preview",
            Operation::SessionRewind => "grapt.session.rewind",
            Operation::SessionUndoPreview => "grapt.session.undo.preview",
            Operation::SessionUndo => "grapt.session.undo",
            Operation::SessionSandboxWorktreeConfigGet => {
                "grapt.session.sandbox.worktreeConfig.get"
            }
            Operation::SessionSandboxWorktreeConfigCreate => {
                "grapt.session.sandbox.worktreeConfig.create"
            }
            Operation::SessionReconcile => "grapt.session.reconcile",
            Operation::SessionRename => "v2.session.rename",
            Operation::SessionDelete => "v2.session.remove",
            Operation::MessageList => "v2.message.list",
            Operation::SessionSnapshot => "grapt.session.snapshot",
            Operation::EventSubscribe => "v2.event.subscribe",
            Operation::PermissionRequestList => "v2.permission.request.list",
            Operation::SandboxGrantList => "grapt.sandbox.grants.list",
            Operation::SandboxGrantRevoke => "grapt.sandbox.grants.revoke",
            Operation::SessionSubagentModelGet => "v2.session.subagentModel.get",
            Operation::SessionPermissionList => "v2.session.permission.list",
            Operation::SessionPermissionCreate => "v2.session.permission.create",
            Operation::SessionPermissionGet => "v2.session.permission.get",
            Operation::SessionPermissionReply => "v2.session.permission.reply",
            Operation::McpList => "v2.mcp.list",
            Operation::WorkspaceDiff => "grapt.workspace.diff",
            Operation::WorkspaceWorktree => "grapt.workspace.worktree",
            Operation::ClaudePlugin => "grapt.plugin.claude",

            Operation::McpInspect => "grapt.mcp.inspect",
            Operation::McpAuth => "grapt.mcp.auth",
            Operation::McpContent => "grapt.mcp.content",
            Operation::McpConfiguration => "grapt.mcp.configuration",
            Operation::McpReconnect => "grapt.mcp.reconnect",
            Operation::McpApproval => "grapt.mcp.approval",
            Operation::SessionPresetGet => "v2.session.preset.get",
            Operation::SessionPresetSelect => "v2.session.preset",
        }
    }

    /// The frozen templated path.
    pub fn template(self) -> &'static str {
        match self {
            Operation::HealthGet => "/api/health",
            Operation::IntegrationList => "/api/integration",
            Operation::IntegrationKeyConnect => "/api/integration/{integrationID}/connect/key",
            Operation::IntegrationOAuthConnect => "/api/integration/{integrationID}/connect/oauth",
            Operation::IntegrationOAuthStatus | Operation::IntegrationOAuthCancel => {
                "/api/integration/{integrationID}/connect/oauth/{attemptID}"
            }
            Operation::IntegrationOAuthComplete => {
                "/api/integration/{integrationID}/connect/oauth/{attemptID}/complete"
            }
            Operation::CredentialRename | Operation::CredentialRemove => {
                "/api/credential/{credentialID}"
            }
            Operation::ProviderList => "/api/provider",
            Operation::ProjectList => "/api/project",
            Operation::ProjectCurrent => "/api/project/current",
            Operation::AgentList => "/api/agent",
            Operation::ModelList => "/api/model",
            Operation::ModelDefault => "/api/model/default",
            Operation::CommandList => "/api/command",
            Operation::CommandInspect => "/api/command/inspect",
            Operation::InstructionInspect => "/api/session/{sessionID}/instructions/inspect",
            Operation::SessionAgentSelect => "/api/session/{sessionID}/agent",
            Operation::SessionModelSelect => "/api/session/{sessionID}/model",
            Operation::FormRequestList => "/api/form/request",
            Operation::GlobalFormGet => "/api/session/global/form/{formID}",
            Operation::GlobalFormState => "/api/session/global/form/{formID}/state",
            Operation::GlobalFormReply => "/api/session/global/form/{formID}/reply",
            Operation::GlobalFormCancel => "/api/session/global/form/{formID}/cancel",
            Operation::SessionFormList => "/api/session/{sessionID}/form",
            Operation::SessionFormGet => "/api/session/{sessionID}/form/{formID}",
            Operation::SessionFormState => "/api/session/{sessionID}/form/{formID}/state",
            Operation::SessionFormReply => "/api/session/{sessionID}/form/{formID}/reply",
            Operation::SessionFormCancel => "/api/session/{sessionID}/form/{formID}/cancel",
            Operation::SessionList | Operation::SessionCreate => "/api/session",
            Operation::SessionGet | Operation::SessionDelete => "/api/session/{sessionID}",
            Operation::SessionPrompt => "/api/session/{sessionID}/prompt",
            Operation::SessionPendingList => "/api/session/{sessionID}/pending",
            Operation::SessionInterrupt => "/api/session/{sessionID}/interrupt",
            Operation::SessionCompact => "/api/session/{sessionID}/compact",
            Operation::SessionFork => "/api/session/{sessionID}/fork",
            Operation::SideQuestion => "/api/session/{sessionID}/side",
            Operation::SessionRewindPreview => "/api/session/{sessionID}/rewind/preview",
            Operation::SessionRewind => "/api/session/{sessionID}/rewind",
            Operation::SessionUndoPreview => "/api/session/{sessionID}/undo/preview",
            Operation::SessionUndo => "/api/session/{sessionID}/undo",
            Operation::SessionSandboxWorktreeConfigGet
            | Operation::SessionSandboxWorktreeConfigCreate => {
                "/api/session/{sessionID}/sandbox/worktree-config"
            }
            Operation::SessionReconcile => "/api/session/{sessionID}/reconcile",
            Operation::SessionRename => "/api/session/{sessionID}/rename",
            Operation::MessageList => "/api/session/{sessionID}/message",
            Operation::SessionSnapshot => "/api/session/{sessionID}/snapshot",
            Operation::EventSubscribe => "/api/event",
            Operation::PermissionRequestList => "/api/permission/request",
            Operation::SandboxGrantList => "/api/session/{sessionID}/sandbox-grants",
            Operation::SandboxGrantRevoke => "/api/session/{sessionID}/sandbox-grants/revoke",
            Operation::SessionSubagentModelGet => "/api/session/{sessionID}/subagent-model",
            Operation::SessionPermissionList | Operation::SessionPermissionCreate => {
                "/api/session/{sessionID}/permission"
            }
            Operation::SessionPermissionGet => "/api/session/{sessionID}/permission/{requestID}",
            Operation::SessionPermissionReply => {
                "/api/session/{sessionID}/permission/{requestID}/reply"
            }
            Operation::McpList => "/api/mcp",
            Operation::WorkspaceDiff => "/api/workspace/diff",
            Operation::WorkspaceWorktree => "/api/workspace/worktree",
            Operation::ClaudePlugin => "/api/plugin/claude",

            Operation::McpInspect => "/api/mcp/inspect",
            Operation::McpAuth => "/api/mcp/{server}/auth",
            Operation::McpContent => "/api/mcp/{server}/content",
            Operation::McpConfiguration => "/api/mcp/{server}/configuration",
            Operation::McpReconnect => "/api/mcp/{server}/reconnect",
            Operation::McpApproval => "/api/mcp/{server}/approval",
            Operation::SessionPresetGet | Operation::SessionPresetSelect => {
                "/api/session/{sessionID}/preset"
            }
        }
    }

    pub fn method(self) -> Method {
        match self {
            Operation::IntegrationList
            | Operation::IntegrationOAuthStatus
            | Operation::HealthGet
            | Operation::ProviderList
            | Operation::ProjectList
            | Operation::ProjectCurrent
            | Operation::AgentList
            | Operation::ModelList
            | Operation::ModelDefault
            | Operation::CommandInspect
            | Operation::InstructionInspect
            | Operation::CommandList
            | Operation::FormRequestList
            | Operation::GlobalFormGet
            | Operation::GlobalFormState
            | Operation::SessionSubagentModelGet
            | Operation::SessionFormList
            | Operation::SessionFormGet
            | Operation::SessionFormState
            | Operation::SessionList
            | Operation::SessionGet
            | Operation::SessionSandboxWorktreeConfigGet
            | Operation::SessionPendingList
            | Operation::MessageList
            | Operation::SessionSnapshot
            | Operation::EventSubscribe
            | Operation::PermissionRequestList
            | Operation::SandboxGrantList
            | Operation::SessionPermissionList
            | Operation::SessionPermissionGet
            | Operation::McpList
            | Operation::WorkspaceDiff
            | Operation::McpInspect
            | Operation::McpApproval
            | Operation::SessionPresetGet => Method::Get,
            Operation::IntegrationKeyConnect
            | Operation::IntegrationOAuthConnect
            | Operation::IntegrationOAuthComplete
            | Operation::McpContent
            | Operation::McpAuth
            | Operation::WorkspaceWorktree
            | Operation::ClaudePlugin
            | Operation::McpConfiguration
            | Operation::McpReconnect
            | Operation::SessionCreate
            | Operation::SessionFork
            | Operation::SideQuestion
            | Operation::SessionAgentSelect
            | Operation::SessionModelSelect
            | Operation::SessionPrompt
            | Operation::SessionInterrupt
            | Operation::SessionRename
            | Operation::SessionCompact
            | Operation::SessionRewindPreview
            | Operation::SessionRewind
            | Operation::SessionUndoPreview
            | Operation::SessionUndo
            | Operation::SessionSandboxWorktreeConfigCreate
            | Operation::SessionReconcile
            | Operation::SessionPermissionCreate
            | Operation::GlobalFormReply
            | Operation::GlobalFormCancel
            | Operation::SessionFormReply
            | Operation::SessionFormCancel
            | Operation::SessionPermissionReply
            | Operation::SandboxGrantRevoke
            | Operation::SessionPresetSelect => Method::Post,
            Operation::CredentialRename => Method::Patch,
            Operation::CredentialRemove
            | Operation::IntegrationOAuthCancel
            | Operation::SessionDelete => Method::Delete,
        }
    }

    /// True when the template carries `{sessionID}`.
    pub fn needs_session(self) -> bool {
        self.template().contains(SESSION_PLACEHOLDER)
    }

    /// True when the template carries `{requestID}`.
    pub fn needs_request(self) -> bool {
        self.template().contains(REQUEST_PLACEHOLDER)
    }

    /// True when the template carries `{formID}`.
    ///
    /// No frozen template carries both this and `{requestID}`, which is why
    /// one sub-resource coordinate addresses either. The closure witness
    /// asserts that exclusivity rather than leaving it as an assumption: if
    /// the contract ever grows a route scoped by both, this collapses two
    /// distinct identities into one argument and the witness reddens.
    pub fn needs_form(self) -> bool {
        self.template().contains(FORM_PLACEHOLDER)
    }

    /// Resolve the concrete path.
    ///
    /// A scoped operation given no scope is a CALLER defect, not a server one,
    /// so it is refused here rather than producing a path containing a literal
    /// brace that a server would answer with a 404 — which the client would
    /// then have to interpret, and could mistake for an unavailable route and
    /// report as a product gap.
    ///
    /// The session scope is a `SessionWireId`, not a `&str`. That closes the
    /// last route by which an unparsed identity could reach the wire: there is
    /// no way to address a session-scoped operation without first having
    /// parsed the identity, so a path can never be built from a string the
    /// domain would refuse.
    pub fn path(
        self,
        session: Option<&crate::wire::SessionWireId>,
        request: Option<&str>,
    ) -> Result<String, ContractError> {
        let mut path = self.template().to_owned();
        if path.contains("{integrationID}") || path.contains("{credentialID}") {
            // These routes require their own typed coordinates, including both
            // integration and attempt for OAuth. Use WireRequest::integration.
            return Err(ContractError::RequestRequired {
                operation: self.operation_id(),
            });
        }
        if matches!(
            self,
            Operation::McpContent
                | Operation::McpAuth
                | Operation::McpConfiguration
                | Operation::McpReconnect
                | Operation::McpApproval
        ) {
            let name = request
                .filter(|name| {
                    !name.is_empty()
                        && name.len() <= 256
                        && !name.chars().any(char::is_control)
                        && !matches!(*name, "." | "..")
                })
                .ok_or(ContractError::RequestRequired {
                    operation: self.operation_id(),
                })?;
            let encoded: String = name.bytes().map(|byte| format!("%{byte:02X}")).collect();
            path = path.replace("{server}", &encoded);
        }
        if self.needs_session() {
            match session {
                Some(id) => {
                    path = path.replace(SESSION_PLACEHOLDER, id.spelled());
                }
                _ => {
                    return Err(ContractError::SessionRequired {
                        operation: self.operation_id(),
                    })
                }
            }
        }
        if self.needs_request() {
            match request {
                Some(id) if !id.is_empty() => path = path.replace(REQUEST_PLACEHOLDER, id),
                _ => {
                    return Err(ContractError::RequestRequired {
                        operation: self.operation_id(),
                    })
                }
            }
        }
        if self.needs_form() {
            match request {
                Some(id) if !id.is_empty() => path = path.replace(FORM_PLACEHOLDER, id),
                _ => {
                    return Err(ContractError::FormRequired {
                        operation: self.operation_id(),
                    })
                }
            }
        }
        Ok(path)
    }
}

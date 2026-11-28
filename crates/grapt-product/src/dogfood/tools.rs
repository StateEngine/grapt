//! Rust-authoritative arguments and provider projections for the dogfood tools.

mod approval_preview;
pub(super) mod ask;
mod call_input;
mod catalog;
pub(in crate::dogfood) mod codemode_failure;
mod configured_program;
mod decision_wait;
mod diagnostics;
mod failure_detail;
mod formatting;
mod history;
mod hook_decision;
mod mcp_call;
mod mcp_failure;
mod mcp_recovery;
pub(in crate::dogfood) mod native_failure;
mod paths;
mod permission_hooks;
mod plan;
pub(in crate::dogfood) mod sandbox;
mod schema;
mod selection;
pub(in crate::dogfood) mod shadow;
pub(in crate::dogfood) mod shell_environment;
pub(super) use call_input::context_call;
pub(super) use catalog::BUILTIN_TOOL_NAMES;
#[cfg(test)]
use catalog::{
    tool_definition, EDIT_DESCRIPTION, HISTORY_DESCRIPTION, READ_DESCRIPTION, SHELL_DESCRIPTION,
    SUBAGENT_DESCRIPTION,
};
pub(super) use effects::allow_shell_environment;
pub(in crate::dogfood) use effects::ShellControl;
#[cfg(test)]
use failure_detail::EARLY_FAILURE_DETAIL_BYTES;
use failure_detail::{captured_failure, failed_with_detail};
pub(super) use history::HistoryArguments;
use mcp_call::run_mcp;
use paths::{filesystem_capability, protected_paths, resolve_existing};
pub(super) use selection::{ephemeral_definitions, offered_definitions};
pub(super) use skill::public_missing_detail as public_missing_skill_detail;
pub(super) use terminal::exec_description;
#[cfg(target_os = "linux")]
pub(in crate::dogfood) use terminal::linux_cleanup::group_running;
#[cfg(target_os = "macos")]
pub(in crate::dogfood) use terminal::macos_cleanup;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use grapt_api::{DogfoodEventHub, SealedPrincipal};
use grapt_artifact::FilesystemArtifactStore;
#[cfg(test)]
use grapt_artifact::{ArtifactStore, ReadBudget};
use grapt_inbox::Inbox;
use grapt_permission::{
    PermissionHttpSource, PermissionHttpState, PermissionOwner, ToolAuthorizationError,
    ToolPermissionAuthority,
};
use grapt_provider::ToolDefinition;
use grapt_runtime::{
    BoundedArtifactWriter, BoundedProgressSink, BoundedToolInput, BoxFuture, GovernedToolExecutor,
    GovernedToolRunner, ToolCancellation, ToolExecutionTerminal, ToolRunner,
};
use grapt_store::{OsRandomIds, SystemClock};
use grapt_tools_local::FILE_EFFECT_MAX_BYTES;
use grapt_types::{
    AgentId, CapabilityId, ClockAuthority, EffectClass, EgressAuthority, EventId,
    FilesystemAuthority, IdAuthority, MessageId, PathIdentity, PermissionId, ProgramIdentity,
    ProjectId, RealizedEnforcement, ResolvedCapability, SessionId, ToolCallId, ToolFailureReason,
    ToolTerminalOutcome,
};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

pub(super) const READ_TOOL: &str = "read";
pub(super) const EDIT_TOOL: &str = "edit";
pub(super) const WRITE_TOOL: &str = "write";
pub(super) const GLOB_TOOL: &str = "glob";
pub(super) const GREP_TOOL: &str = "grep";
pub(super) const READ_LINES_TOOL: &str = "read_lines";
pub(super) const LIST_TOOL: &str = "list";
pub(super) const PATCH_TOOL: &str = "patch";
pub(super) const APPLY_PATCH_TOOL: &str = "apply_patch";
pub(super) const SHELL_TOOL: &str = "shell";
/// Product-internal effect a plugin integration command method runs through.
/// It is never published to a model and a plugin tool cannot claim the name.
pub(super) const INTEGRATION_COMMAND_TOOL: &str = "plugin_integration_command";
pub(super) const SUBAGENT_TOOL: &str = "subagent";
pub(super) const HISTORY_TOOL: &str = "history";
pub(super) const PLAN_TOOL: &str = "update_plan";
pub(super) const EXECUTE_TOOL: &str = "execute";

pub(super) fn composition_definition(
    published: &super::mcp::PublishedTools,
) -> Result<Option<ToolDefinition>, String> {
    codemode::definition(published)
}

/// A child that exceeds this finite budget fails and is cancelled.
const SUBAGENT_BUDGET: Duration = Duration::from_secs(300);

/// The budget this composition will actually enforce.
///
/// Ordinary builds always answer [`SUBAGENT_BUDGET`]. The override exists under
/// the same named testkit feature the loopback provider endpoint uses, and for
/// the same reason: a five-minute budget cannot be reached inside a gate, so
/// without it the timeout path would be the one branch here with no witness —
/// asserted by reading the code rather than by running it. It narrows the
/// budget only; it cannot widen it, so no test can grant a child more time than
/// production allows.
fn subagent_budget() -> Duration {
    #[cfg(feature = "loopback-provider-testkit")]
    if let Some(raw) = std::env::var_os("GRAPT_SUBAGENT_BUDGET_MS") {
        if let Some(ms) = raw.to_str().and_then(|value| value.parse::<u64>().ok()) {
            return Duration::from_millis(ms).min(SUBAGENT_BUDGET);
        }
    }
    SUBAGENT_BUDGET
}

/// The furthest line a read may start at.
const READ_MAX_LINE: u64 = 1_000_000_000;
/// Lines in one read page by default, and the most a read may ask for.
const READ_PAGE_LINES: u64 = read_window::READ_LINES;
const SHELL_STREAM_MAX_BYTES: usize = 512 * 1_024;
const SHELL_DEFAULT_TIMEOUT_MS: u64 = 30_000;
const SHELL_MAX_TIMEOUT_MS: u64 = 120_000;

/// The widest integer bound an advertised schema may carry: 2^53 - 1, the
/// largest integer an IEEE 754 double represents exactly. Serving backends
/// reject requests advertising wider bounds, so every published bound is
/// capped here while the runtime decoders keep accepting the full u64 range.
pub(super) const JSON_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

trait ProviderFieldSchema {
    fn provider_schema() -> serde_json::Value;
}

impl ProviderFieldSchema for String {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type": "string"})
    }
}

impl ProviderFieldSchema for u64 {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type": "integer", "minimum": 0,
            "maximum": JSON_SAFE_INTEGER_MAX})
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BoundedU64<const MIN: u64, const MAX: u64>(u64);

impl<const MIN: u64, const MAX: u64> BoundedU64<MIN, MAX> {
    const fn get(self) -> u64 {
        self.0
    }
}

impl<'de, const MIN: u64, const MAX: u64> Deserialize<'de> for BoundedU64<MIN, MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        if value < MIN || value > MAX {
            return Err(D::Error::custom(format_args!(
                "integer must be between {MIN} and {MAX} inclusive"
            )));
        }
        Ok(Self(value))
    }
}

impl<const MIN: u64, const MAX: u64> ProviderFieldSchema for BoundedU64<MIN, MAX> {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type": "integer", "minimum": MIN.min(JSON_SAFE_INTEGER_MAX),
            "maximum": MAX.min(JSON_SAFE_INTEGER_MAX)})
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct NonEmptyString(String);

impl NonEmptyString {
    fn as_str(&self) -> &str {
        &self.0
    }

    fn into_inner(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for NonEmptyString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() {
            return Err(D::Error::custom("string must not be empty"));
        }
        Ok(Self(value))
    }
}

impl ProviderFieldSchema for NonEmptyString {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type": "string", "minLength": 1})
    }
}

trait ToolArguments {
    const TOOL_NAME: &'static str;

    fn provider_schema() -> String;
}

fn deserialize_present_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

macro_rules! tool_arguments {
    (
        $visibility:vis struct $name:ident => $tool_name:ident {
            $(required $required_field:ident: $required_type:ty,)*
            $(optional $optional_field:ident: $optional_type:ty,)*
            $(nullable $nullable_field:ident: $nullable_type:ty,)*
        }
    ) => {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        $visibility struct $name {
            $($visibility $required_field: $required_type,)*
            $(
                #[serde(default, deserialize_with = "deserialize_present_non_null")]
                $visibility $optional_field: Option<$optional_type>,
            )*
            $(
                #[serde(default)]
                $visibility $nullable_field: Option<$nullable_type>,
            )*
        }

        impl ToolArguments for $name {
            const TOOL_NAME: &'static str = $tool_name;

            fn provider_schema() -> String {
                let mut properties = serde_json::Map::new();
                $(
                    properties.insert(
                        stringify!($required_field).to_owned(),
                        <$required_type as ProviderFieldSchema>::provider_schema(),
                    );
                )*
                $(
                    properties.insert(
                        stringify!($optional_field).to_owned(),
                        <$optional_type as ProviderFieldSchema>::provider_schema(),
                    );
                )*
                $(
                    properties.insert(
                        stringify!($nullable_field).to_owned(),
                        serde_json::json!({"anyOf": [
                            <$nullable_type as ProviderFieldSchema>::provider_schema(),
                            {"type": "null"}
                        ]}),
                    );
                )*
                serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": properties,
                    "required": [$(stringify!($required_field)),*],
                })
                .to_string()
            }
        }
    };
}

mod agents;
mod codemode;
mod description;
use description::ToolDescription;
pub(in crate::dogfood) mod coding;
pub(super) mod image;
mod terminal;
pub(super) use codemode::WorkerRegistry;
pub(super) use terminal::recovery as terminal_recovery;
#[cfg(windows)]
pub(crate) use terminal::shim_main;
pub(super) use terminal::TerminalRegistry;

tool_arguments! {
    pub(super) struct ReadArguments => READ_TOOL {
        required path: String,
        optional offset: BoundedU64<1, READ_MAX_LINE>,
        optional limit: BoundedU64<1, READ_PAGE_LINES>,
    }
}

tool_arguments! {
    pub(super) struct EditArguments => EDIT_TOOL {
        required path: String,
        required old_text: NonEmptyString,
        required new_text: String,
        optional replace_all: bool,
    }
}

tool_arguments! {
    pub(super) struct ShellArguments => SHELL_TOOL {
        required command: NonEmptyString,
        optional description: ToolDescription,
        optional timeout_ms: BoundedU64<1, SHELL_MAX_TIMEOUT_MS>,
        optional msys_unconfined: bool,
    }
}

pub(super) fn definitions() -> Vec<ToolDefinition> {
    catalog::definitions()
}

mod sandbox_access;
mod target;
use target::{EffectTarget, PreparedEffect};

struct ToolRunOptions {
    machine_output: bool,
    plugin_hooks: bool,
    hook: super::hooks::HookResult,
    observation: Option<Arc<shadow::Dispatch>>,
}

#[derive(Debug)]
struct ProjectToolExecutor {
    observation: Option<Arc<shadow::Dispatch>>,
    pre_annotations: Vec<String>,
    credential_environment: Vec<String>,
    machine_output: bool,
    ephemeral: bool,
    hooks: Option<Arc<super::hooks::HookRuntime>>,
    checkpoint_root: Option<PathBuf>,
    workers: Arc<codemode::Workers>,
    terminals: Arc<terminal::TerminalRegistry>,
    ids: Arc<OsRandomIds>,
    artifact_store: Arc<FilesystemArtifactStore>,
    inbox: Arc<Mutex<Inbox>>,
    root: PathBuf,
    target: Mutex<Option<EffectTarget>>,
    mcp: Arc<super::mcp::McpRegistry>,
    plugins: Arc<super::plugins::PluginRuntime>,
    plugin_generation: Option<Arc<grapt_plugin::PluginGeneration>>,
    session_capabilities: Arc<Mutex<super::context::SessionCapabilities>>,
    location: String,
    forms: super::elicitation::FormProducer,
    approvals: grapt_api::ApprovalWatch,
    decision_waits: Arc<super::human_input::Waits>,
    state_feedback: Option<super::session_status::RetryFeedback>,
    /// Everything a subagent call needs: the route back to the one execution
    /// owner, and the parent coordinates the child inherits its lineage and
    /// authority from.
    child_seam: super::subagent::ChildSeam,
    parent: SessionId,
    message: MessageId,
    agent: AgentId,
    tool: String,
    principal: SealedPrincipal,
    /// The attempt that admitted the call this executor is running, when one is
    /// known. `None` where no attempt frames the call — a direct test drive —
    /// never a minted substitute.
    attempt: Option<grapt_types::PhysicalAttemptId>,
}

#[derive(Clone, Default)]
struct UnexecutedApprovals(Arc<Mutex<std::collections::HashSet<ToolCallId>>>);

impl UnexecutedApprovals {
    fn mark_pending(&self, call: &ToolCallId) {
        lock(&self.0).insert(call.clone());
    }

    fn mark_dispatched(&self, call: &ToolCallId) {
        lock(&self.0).remove(call);
    }

    fn is_known_unexecuted(&self, call: &ToolCallId) -> bool {
        lock(&self.0).contains(call)
    }
}

#[allow(
    missing_debug_implementations,
    reason = "the runner owns an opaque authenticated principal"
)]
#[derive(Clone)]
pub(super) struct PermissionToolRunner {
    shadow: Arc<shadow::Ledger>,
    approval_location: grapt_types::LocationRef,
    ephemeral: bool,
    hooks: Option<Arc<super::hooks::HookRuntime>>,
    checkpoint_root: Option<PathBuf>,
    workers: Arc<codemode::Workers>,
    terminals: Arc<terminal::TerminalRegistry>,
    root: PathBuf,
    credential_environment: Vec<String>,
    session: SessionId,
    message: MessageId,
    agent: AgentId,
    principal: SealedPrincipal,
    project: ProjectId,
    permissions: Arc<Mutex<PermissionHttpState>>,
    authority: Arc<ToolPermissionAuthority>,
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<OsRandomIds>,
    clock: Arc<SystemClock>,
    artifacts: Arc<FilesystemArtifactStore>,
    hub: Arc<DogfoodEventHub>,
    decision_waits: Arc<super::human_input::Waits>,
    state_feedback: Option<super::session_status::RetryFeedback>,
    /// The catalog this execution planned against, and the registry that holds
    /// the connections it names. Both are needed at execution because a call
    /// must resolve to the SAME route the model was shown.
    published: Arc<super::mcp::PublishedTools>,
    mcp: Arc<super::mcp::McpRegistry>,
    plugins: Arc<super::plugins::PluginRuntime>,
    plugin_generation: Option<Arc<grapt_plugin::PluginGeneration>>,
    session_capabilities: Arc<Mutex<super::context::SessionCapabilities>>,
    location: String,
    /// The route a subagent tool call uses to run its child through the one
    /// execution owner.
    child_seam: super::subagent::ChildSeam,
    /// The trust level this Session's calls are decided under.
    policy: super::permission_preset::ResolvedPolicy,
    /// Calls currently awaiting human approval that have NOT yet been dispatched
    /// to an effect handler. Cleared immediately inside `consume_and_run` before
    /// `governed.run_effect` begins, so a provider-aborted pending approval is
    /// classified as `EffectFailed` while any dispatched call remains `EffectUnknown`.
    unexecuted_approvals: UnexecutedApprovals,
}

/// A command prefix does not authorize startup profiles or another executable.
fn terminal_prefix_grants(spec: &terminal::ExecSpec) -> bool {
    let native_arguments = if cfg!(windows) {
        spec.arguments
            .iter()
            .map(String::as_str)
            .eq(["/D", "/S", "/C", spec.command.as_str()])
    } else {
        spec.arguments
            .iter()
            .map(String::as_str)
            .eq(["-c", spec.command.as_str()])
    };
    let native_invocation = native_arguments && spec.cmd_shell == cfg!(windows);
    native_invocation && shell_program().is_ok_and(|program| program == spec.program)
}

struct PermissionStartFailure {
    reason: ToolFailureReason,
    detail: String,
}
impl From<&str> for PermissionStartFailure {
    fn from(detail: &str) -> Self {
        detail.to_owned().into()
    }
}
impl From<String> for PermissionStartFailure {
    fn from(detail: String) -> Self {
        Self {
            reason: ToolFailureReason::AuthorityUnavailable,
            detail,
        }
    }
}

impl PermissionToolRunner {
    /// Ask the permission authority about one prepared call. A call this
    /// Session's preset denies is refused in its own words, never asked.
    fn begin(
        &self,
        call: &ToolCallId,
        prepared: &PreparedEffect,
        permission: PermissionId,
        plan: &permission_hooks::PermissionPlan,
    ) -> Result<(grapt_permission::PendingToolEffect, PendingDecision), PermissionStartFailure>
    {
        let event = format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| "tool permission event identity was invalid".to_owned())?;
        let cancellation_event: EventId = format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| "tool cancellation event identity was invalid".to_owned())?;
        let owner = PermissionOwner {
            subject: self.principal.principal().receipt().subject().as_str(),
            client: self.principal.principal().client(),
        };
        let source = PermissionHttpSource::Tool {
            message_id: self.message.as_str().to_owned(),
            call_id: call.as_str().to_owned(),
        };
        let mut permissions = lock(&self.permissions);
        let mut inbox = lock(&self.inbox);
        let pending = self
            .hub
            .with_permission_approval(&self.principal, &self.session, &permission, |attached| {
                permissions.begin_tool(
                    inbox.store_mut(),
                    &self.authority,
                    prepared.capability.clone(),
                    &owner,
                    &event,
                    &permission,
                    &self.session,
                    &self.agent,
                    &plan.layers,
                    &self.project,
                    &prepared.action,
                    &plan.resources,
                    Some(&plan.save),
                    plan.metadata.as_ref(),
                    Some(&source),
                    self.clock.now().as_millis(),
                    plan.consequences.as_deref(),
                    attached == Some(true),
                )
            })
            .map_err(|error| error.to_string())?
            .map_err(|error| {
                let reason = if matches!(
                    error,
                    grapt_permission::PermissionHttpError::ApprovalUnavailable
                ) {
                    ToolFailureReason::ApprovalUnavailable
                } else {
                    ToolFailureReason::AuthorityUnavailable
                };
                PermissionStartFailure {
                    reason,
                    detail: super::permission_preset::refusal(error, self.policy.preset),
                }
            })?;
        super::input_events::publish_id(&inbox, &self.hub, &self.session, &event);
        let mut guard = PendingDecision {
            permission,
            session: self.session.clone(),
            principal: self.principal.clone(),
            event: cancellation_event,
            permissions: Arc::clone(&self.permissions),
            authority: Arc::clone(&self.authority),
            inbox: Arc::clone(&self.inbox),
            clock: Arc::clone(&self.clock),
            hub: Arc::clone(&self.hub),
            wait: None,
            armed: pending.awaits_decision(),
        };
        // Drop the borrowed store locks before any cancellation cleanup.
        drop(inbox);
        drop(permissions);
        if guard.armed {
            self.unexecuted_approvals.mark_pending(call);
            guard.wait = Some(
                decision_wait::DecisionWait::begin(
                    &self.decision_waits,
                    super::human_input::Kind::Permission,
                )?
                .with_feedback(self.state_feedback.clone()),
            );
        }
        Ok((pending, guard))
    }
}

#[allow(
    missing_debug_implementations,
    reason = "owns an opaque authenticated principal"
)]
struct PendingDecision {
    permission: PermissionId,
    session: SessionId,
    principal: SealedPrincipal,
    event: EventId,
    permissions: Arc<Mutex<PermissionHttpState>>,
    authority: Arc<ToolPermissionAuthority>,
    inbox: Arc<Mutex<Inbox>>,
    clock: Arc<SystemClock>,
    hub: Arc<DogfoodEventHub>,
    wait: Option<decision_wait::DecisionWait>,
    armed: bool,
}

impl PendingDecision {
    fn finish(&mut self) {
        self.armed = false;
        self.release_wait();
    }
    fn release_wait(&mut self) {
        self.wait = None;
    }
}

impl PendingDecision {
    fn cancel(&mut self, reason: grapt_permission::CancelReason) -> Result<bool, String> {
        let owner = PermissionOwner {
            subject: self.principal.principal().receipt().subject().as_str(),
            client: self.principal.principal().client(),
        };
        let mut permissions = lock(&self.permissions);
        let mut inbox = lock(&self.inbox);
        let committed = permissions
            .cancel_tool_because(
                inbox.store_mut(),
                &owner,
                &self.session,
                &self.permission,
                &self.event,
                self.clock.now().as_millis(),
                reason,
            )
            .map_err(|error| error.to_string())?;
        let cancelled = committed.is_some();
        if let Some(committed) = committed {
            self.authority.cancel(committed);
            super::input_events::publish_id(&inbox, &self.hub, &self.session, &self.event);
        }
        // Publish the committed cancellation before releasing the store locks;
        // observers receive its exact reason without losing their subscription.
        drop(inbox);
        drop(permissions);
        self.finish();
        Ok(cancelled)
    }
}

impl Drop for PendingDecision {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self.cancel(grapt_permission::CancelReason::Interrupted) {
                crate::diagnostics::report(format!(
                    "permission cancellation failed for {} in {}: {error}",
                    self.permission, self.session
                ));
                self.hub.invalidate_session_because(
                    &self.session,
                    "permission cancellation could not be projected",
                );
            }
        }
        self.release_wait();
    }
}

impl ToolRunner for PermissionToolRunner {
    fn is_composition(&self, tool: &str) -> bool {
        tool == EXECUTE_TOOL
    }
    fn settle_attempt(&self) -> BoxFuture<'static, bool> {
        let workers = Arc::clone(&self.workers);
        Box::pin(async move { workers.settle().await })
    }
    fn known_unexecuted(&self, call: &ToolCallId) -> bool {
        self.unexecuted_approvals.is_known_unexecuted(call)
    }
    fn run_with_nested(
        &self,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
        nested: Option<grapt_runtime::NestedTools>,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        self.run_for_output(attempt, call, tool, input, shadow::Origin::Direct, nested)
    }
    fn decision_waits(&self) -> Option<tokio::sync::watch::Receiver<u32>> {
        Some(self.decision_waits.subscribe())
    }
    fn run(
        &self,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        self.run_with_attempt(None, call, tool, input)
    }

    /// THE ATTEMPT ENTERS THE DISPATCH PATH HERE, and nowhere earlier.
    ///
    /// It is owned by the recording side — the durable owner holds the attempt
    /// currently streaming and looks it up when a terminal arrives — so it has
    /// never been available to a tool while the tool ran. `attempt_loop` knows
    /// it at spawn time; this carries it the rest of the way so a nested
    /// elicitation can record WHICH dispatch raised it.
    fn run_with_attempt(
        &self,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        self.run_for_output(attempt, call, tool, input, shadow::Origin::Direct, None)
    }

    fn run_nested(
        &self,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        self.run_for_output(attempt, call, tool, input, shadow::Origin::Nested, None)
    }
}

mod ephemeral;
mod executor;
#[cfg(test)]
pub(in crate::dogfood) use executor::{annotate_failure_detail, annotate_success_output};
mod runner;
#[cfg(test)]
use runner::settle_authorized_effect;

#[cfg(test)]
fn prepare_effect(
    root: &Path,
    tool: &str,
    input: &BoundedToolInput,
    routes: &std::collections::BTreeMap<String, super::mcp::McpRoute>,
) -> Result<PreparedEffect, String> {
    let published = super::mcp::PublishedTools {
        routes: routes.clone(),
        ..super::mcp::PublishedTools::default()
    };
    prepare_effect_inner(root, tool, input, &published, None, None, &[])
}

fn prepare_effect_with_plugins(
    root: &Path,
    tool: &str,
    input: &BoundedToolInput,
    published: &super::mcp::PublishedTools,
    plugins: &super::plugins::PluginRuntime,
    plugin_generation: Option<&grapt_plugin::PluginGeneration>,
    credential_environment: &[String],
) -> Result<PreparedEffect, String> {
    prepare_effect_inner(
        root,
        tool,
        input,
        published,
        Some(plugins),
        plugin_generation,
        credential_environment,
    )
}

fn prepare_effect_inner(
    root: &Path,
    tool: &str,
    input: &BoundedToolInput,
    published: &super::mcp::PublishedTools,
    plugins: Option<&super::plugins::PluginRuntime>,
    plugin_generation: Option<&grapt_plugin::PluginGeneration>,
    credential_environment: &[String],
) -> Result<PreparedEffect, String> {
    // The internal integration-command effect is resolved before any route so
    // no published plugin or MCP tool can answer for it.
    if tool == INTEGRATION_COMMAND_TOOL {
        return prepare_integration_command(root, input);
    }
    // BUILT-INS FIRST. Publication already refuses a qualified name that
    // shadows a built-in, but that guard lives in a different module and a
    // dispatch that consulted routes first would let a route which reached
    // this map by any other path answer for `read`. The built-in owns its
    // name here too, so both sides have to fail before a server can capture
    // one.
    if !BUILTIN_TOOL_NAMES.contains(&tool) {
        // An MCP name is resolved through the ROUTE recorded when it was
        // published, never by taking the qualified string apart: the join
        // character is also the sanitiser, so the name cannot say which half
        // was the server. A name with no route is a name this execution did
        // not publish, and it is refused rather than guessed at.
        if let (Some(plugins), Some(generation)) = (plugins, plugin_generation) {
            if let Some(route) = super::plugins::PluginRuntime::route(generation, published, tool) {
                return prepare_plugin_effect(root, tool, route, input, plugins);
            }
        }
        if let Some(route) = published.routes.get(tool) {
            return prepare_mcp_effect(root, tool, route, input);
        }
    }
    let service = plugins.map_or(root, super::plugins::PluginRuntime::service_root);
    match tool {
        web::FETCH_TOOL => web::prepare(root, input),
        agents::LIST_AGENTS_TOOL => agents::prepare_list(root, input),
        agent_wait::WAIT_AGENT_TOOL => agent_wait::prepare(root, input),
        agent_message::SEND_MESSAGE_TOOL => agent_message::prepare(root, input),
        agent_followup::FOLLOWUP_TASK_TOOL => agent_followup::prepare(root, input),
        WRITE_TOOL | GLOB_TOOL | GREP_TOOL | READ_LINES_TOOL | LIST_TOOL | PATCH_TOOL
        | APPLY_PATCH_TOOL => coding::prepare(root, tool, input),
        READ_TOOL => {
            let args: ReadArguments = decode_input(input)?;
            let target = match resolve_existing(root, &args.path, false) {
                Ok(target) => target,
                Err(error) => return coding::prepare_missing(root, &args.path, error),
            };
            let resource = path_text(&target);
            Ok(PreparedEffect {
                capability: filesystem_capability(EffectClass::FilesystemRead, &resource, false),
                target: EffectTarget::Read(target),
                action: "read".to_owned(),
                resource,
            })
        }
        image::TOOL => image::prepare(root, input),
        EDIT_TOOL => {
            let args: EditArguments = decode_input(input)?;
            let target = resolve_existing(root, &args.path, true)?;
            let resource = path_text(&target);
            Ok(PreparedEffect {
                capability: filesystem_capability(EffectClass::FilesystemWrite, &resource, true),
                target: EffectTarget::Edit(target),
                action: "fs.modify".to_owned(),
                resource,
            })
        }
        SUBAGENT_TOOL => delegation::prepare_subagent(root, input),
        agent_interrupt::INTERRUPT_AGENT_TOOL => agent_interrupt::prepare(root, input),
        agent_spawn::SPAWN_AGENT_TOOL => agent_spawn::prepare(root, input),
        skill::SKILL_TOOL => skill::prepare(root, input),
        PLAN_TOOL => plan::prepare(root, input),
        ask::TOOL => ask::prepare(root, input),
        HISTORY_TOOL => history::prepare(root, input),
        read_history::TOOL => read_history::prepare(root, input),
        sandbox_access::TOOL => sandbox_access::prepare(
            root,
            input,
            shell_environment::Environment::read(service, root, credential_environment)?,
        ),
        terminal::EXEC_COMMAND => terminal::prepare_exec(
            root,
            input,
            shell_environment::Environment::read(service, root, credential_environment)?,
        ),
        terminal::WRITE_STDIN => {
            Err("write_stdin requires a Session-owned terminal registry".into())
        }
        SHELL_TOOL => {
            shell_environment::prepare_shell(root, service, input, credential_environment)
        }
        _ => Err("provider requested an unknown tool".to_owned()),
    }
}

fn prepare_plugin_effect(
    root: &Path,
    tool: &str,
    route: grapt_plugin::PluginToolRoute,
    input: &BoundedToolInput,
    plugins: &super::plugins::PluginRuntime,
) -> Result<PreparedEffect, String> {
    let arguments: serde_json::Value = serde_json::from_slice(input.as_bytes())
        .map_err(|error| format!("plugin tool arguments are not JSON: {error}"))?;
    let runtime = plugins
        .runtime_identity()
        .ok_or_else(|| "plugin runtime identity is absent".to_owned())?;
    let canonical = grapt_event::canonical::canonical_string(&arguments);
    let program = ProgramIdentity::new(
        PathIdentity::canonical(path_text(&runtime.executable)),
        vec![
            route.plugin_id.clone(),
            route.generation.value().to_string(),
            route.callback.to_string(),
            tool.to_owned(),
            canonical,
        ],
        PathIdentity::canonical(path_text(root)),
        Vec::new(),
    );
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        Some(program),
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        target: EffectTarget::Plugin { route, arguments },
        action: tool.to_owned(),
        resource: tool.to_owned(),
    })
}

/// Bind an MCP call to a capability the user can meaningfully approve.
///
/// Ratified law C5: approval binds the exact PROGRAM and ARGUMENT VECTOR, and
/// a later argv rewrite is a different capability rather than the approved
/// one. An MCP server is a spawned program, so that law applies literally
/// here: the capability carries the declared command, the tool the server
/// published, and the arguments as canonical JSON.
///
/// The consequence is the one that matters. Approving `github_create_issue`
/// with one set of arguments does not approve it with another, and a
/// declaration repointed at a different command is a different capability
/// rather than a reused approval — so an approval cannot be inherited by a
/// server the user never agreed to run.
fn prepare_mcp_effect(
    root: &Path,
    qualified: &str,
    route: &super::mcp::McpRoute,
    input: &BoundedToolInput,
) -> Result<PreparedEffect, String> {
    let arguments: serde_json::Value = serde_json::from_slice(input.as_bytes())
        .map_err(|error| format!("MCP tool arguments are not JSON: {error}"))?;
    if !arguments.is_object() {
        return Err("MCP tool arguments must be a JSON object".to_owned());
    }
    // Canonical rather than as-received, so two spellings of one argument set
    // are one capability and cannot be used to dodge a prior refusal.
    let canonical = grapt_event::canonical::canonical_string(&arguments);

    // A connection with no recorded program cannot be bound to one, and an
    // approval that binds nothing is not an approval.
    if route.launch.program.is_empty() {
        return Err(format!(
            "MCP server {:?} has no resolved program to bind an approval to",
            route.server
        ));
    }
    let mut argv = route.launch.arguments.clone();
    argv.push(route.tool.as_str().to_owned());
    argv.push(canonical);
    // Bind the capability to this published route. The registry separately
    // permits transport recovery under its immutable declaration grant;
    // a newly published route still has its own capability identity.
    argv.push(format!("connection:{}", route.launch.incarnation));

    // The RESOLVED program and the cwd the child ACTUALLY ran in, not the
    // configured string and not the Session root. `spawn_local` resolves a
    // bare program itself, and binding what was declared rather than what ran
    // would be a capability describing something that never executed.
    let identity = ProgramIdentity::new(
        PathIdentity::canonical(route.launch.program.clone()),
        argv,
        PathIdentity::canonical(route.launch.cwd.clone()),
        Vec::new(),
    );
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        Some(identity),
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        target: EffectTarget::Mcp {
            route: route.clone(),
            arguments,
        },
        // The action a user sees is the qualified name, which is also what the
        // model was shown and what a receipt records. One identity, three
        // places, per the oracle's own posture at tool/mcp.ts:114.
        action: qualified.to_owned(),
        resource: format!("{}/{}", route.server, route.tool.as_str()),
    })
}

fn decode_input<T: for<'de> Deserialize<'de>>(input: &BoundedToolInput) -> Result<T, String> {
    serde_json::from_slice(input.as_bytes())
        .map_err(|error| format!("tool input is invalid: {error}"))
}

mod agent_followup;
mod agent_interrupt;
mod agent_message;
mod agent_spawn;
pub(super) mod agent_wait;
mod call_repair;
mod delegation;
mod effects;
pub(in crate::dogfood) mod read_history;
mod skill;
mod web;
use delegation::{run_subagent, SubagentArguments};
use effects::*;
use grapt_tools_local::read_window;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod sustained_tests;

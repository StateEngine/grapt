use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use grapt_api::u3a::{
    AggregateIdDto, DogfoodEvent, DurableEvent, DurableEventRef, FiniteF64, LiveEvent,
    SliceDeltaData, SliceEndedData, SliceStartedData, UsageUpdatedData, WireId,
};
use grapt_api::{DogfoodEventHub, SealedPrincipal};
use grapt_artifact::FilesystemArtifactStore;
use grapt_inbox::Inbox;
use grapt_permission::{PermissionHttpState, ToolPermissionAuthority};
use grapt_platform::{CredentialAuthority, CredentialResolutionRequest, ProviderTargetAuthority};
use grapt_provider::{
    AttemptFragment, AttemptOpener, CodexDeadlines, CodexLimits, EndpointConfig, StreamValidator,
    TokenUsage,
};
use grapt_runtime::{
    AttemptPlan, BoundedToolInput, Clock, CommittedFactSink, DurableAttemptOwner, DurableFact,
    EventIds, HistoryCursor, HistoryGeneration, LocationError, LocationRuntime, LocationServiceMap,
    Promotion, ProviderAttemptSource, RuntimeApplication, SessionCoordinator, SessionPorts,
    StepChain, StepPolicy, WakeOutcome,
};
use grapt_store::{AttemptRecordCeiling, OsRandomIds, SystemClock};
use grapt_types::{
    AgentId, ClockAuthority, ContentKind, DeterministicAuthorities, EventId, ExecutionId,
    IdAuthority, LocationId, MessageId, ModelId, PhysicalAttemptId, ProviderId,
    ResolvedModelTarget, SessionId, StepNumber, ToolCallId, ToolTerminalOutcome,
};

use super::continuation::DurableContinuation;
use super::production::{PlatformTargets, ProductPlan, ProductionProviderLimits};
use super::tools::PermissionToolRunner;
use super::usage::costed_usage_dto;
use durable_history::DurableHistory;

mod active;
pub(super) mod artifacts;
use artifacts::open_artifacts;
mod authorities;
mod block_events;
mod calibration;
mod compose;
pub(super) mod context_trim;
mod credentials;
mod durable_history;
mod ephemeral;
pub(super) mod failure_notice;
mod hook_events;
mod interrupt_truth;
mod live_sink;
mod location;
mod observations;
mod plugin;
#[cfg(test)]
#[path = "session_status_revision_tests.rs"]
mod revision_order_tests;
mod side_context;
mod state;
mod transient;
pub(in crate::dogfood) use transient::{check_generation_patch, TransientContext};
mod principals;
mod task_application;
mod timing;
mod tool_events;
mod wake_custody;
#[cfg(test)]
mod workspace_custody_tests;
pub(super) use wake_custody::{
    StopAdmission, StopRelease, WakeCustody, WakeGuard, WakeReservation, WakeSlots,
    WorkspaceIdleReservation,
};
pub(super) fn open_checkpoint_artifacts(service: &Path) -> Result<FilesystemArtifactStore, String> {
    open_artifacts(service).map_err(ExecutionRefusal::reason)
}

pub(in crate::dogfood) use self::active::LiveActiveExecutions;
pub(super) use self::authorities::{RuntimeClock, RuntimeEventIds};
use self::location::registry_location;
use self::principals::{PromptPrincipal, PromptPrincipals};

const MAX_PUBLICATION_REFUSALS: usize = 64;
const MAX_TRANSIENT_GENERATION_BYTES: usize = 4 * 1024 * 1024;

/// The coding chain has no turn budget: a turn runs until the model finishes
/// or the user interrupts. Only opt-in caps stop it: `--max-steps` on an input
/// or `steps` in the selected agent's definition.
fn coding_step_allowance() -> Option<grapt_runtime::StepAllowance> {
    None
}

mod provider_update;
pub(super) mod providers;
mod publication;
pub(in crate::dogfood) mod run;
use interrupt_truth::InterruptTruth;
pub(super) use publication::PublicationLedger;
use publication::{PublishedSnapshotGuard, PublishedSnapshots};

#[derive(Debug, thiserror::Error)]
pub enum ExecutionRefusal {
    #[error("live provider composition refused: {0}")]
    Compose(String),
    #[error("live provider execution refused: {0}")]
    Run(String),
    #[error("live provider execution refused: {0}")]
    PublishedFailure(String),
}

pub enum LiveEndpoint {
    Production(EndpointConfig),
    #[cfg(feature = "loopback-provider-testkit")]
    Loopback(grapt_provider::LoopbackBase),
}

#[derive(Debug)]
struct LiveBlock {
    block: u32,
    opening: grapt_event::session_message::ContentBlockOpenedFact,
    position: grapt_session::CommittedPosition,
    kind: ContentKind,
    text: String,
    // Set on first nonempty delta: empty blocks never consume a transcript row.
    public: Option<(u64, u64)>,
}

#[derive(Debug)]
struct LiveSink {
    artifacts: Arc<FilesystemArtifactStore>,
    tools: BTreeMap<grapt_types::ToolCallId, bool>,
    inbox: Arc<Mutex<Inbox>>,
    hub: Arc<DogfoodEventHub>,
    ids: Arc<OsRandomIds>,
    clock: Arc<SystemClock>,
    session: SessionId,
    execution: ExecutionId,
    publication: Arc<PublicationLedger>,
    blocks: Vec<LiveBlock>,
    /// Finished reasoning the owner holds uncommitted until visible output
    /// follows (SC-REL-02c), in close order; `None` for one whose projection
    /// was refused. The owner commits them in this order, so each reasoning
    /// capture ends the first; a discarded attempt drops them all.
    pending: std::collections::VecDeque<Option<LiveBlock>>,
    /// Blocks whose public projection was refused, at most one per content
    /// kind. Each refusal invalidates subscribers once; the block's later
    /// frames are dropped, never refused again, so one bad block cannot end
    /// the stream per delta.
    refused: Vec<(u32, ContentKind)>,
    attempt: Option<PhysicalAttemptId>,
    text_ordinal: u64,
    reasoning_ordinal: u64,
    /// The public ordinals when the current attempt started. A retrying
    /// attempt has no closed block, so every ordinal it took was a block the
    /// owner discarded; the retry takes them again.
    attempt_ordinals: (u64, u64),
    step: StepNumber,
    agent: WireId<AgentId>,
    model: grapt_api::u3a::ModelRefDto,
    message: Option<MessageId>,
}

type LiveOwner = DurableAttemptOwner<'static, FilesystemArtifactStore, LiveSink>;
type LiveAttempts = ProviderAttemptSource<ProductPlan>;

#[allow(
    missing_debug_implementations,
    reason = "the Location owns the non-formatting provider credential opener"
)]
struct LiveLocation {
    ephemeral: bool,
    active: Arc<LiveActiveExecutions>,
    session_ends: Arc<super::hooks::session_end::SessionEnds>,
    service_root: PathBuf,
    providers: super::provider_generation::Publication,
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<OsRandomIds>,
    clock: Arc<SystemClock>,
    runtime_clock: Arc<RuntimeClock>,
    timing: Arc<dyn grapt_types::timing::TimingObserver>,
    ceiling: AttemptRecordCeiling,
    hub: Arc<DogfoodEventHub>,
    publication: Arc<PublicationLedger>,
    permissions: Arc<Mutex<PermissionHttpState>>,
    tool_permissions: Arc<ToolPermissionAuthority>,
    principals: Arc<PromptPrincipals>,
    tool_artifacts: Arc<FilesystemArtifactStore>,
    codemode_workers: Arc<super::tools::WorkerRegistry>,
    shadow: Arc<super::tools::shadow::Ledger>,
    terminals: Arc<super::tools::TerminalRegistry>,
    /// Shared with the executor that refreshes it, so the snapshot a Session
    /// took in the async run before planning is the one this synchronous seam
    /// reads for THAT Session.
    published: PublishedSnapshots,
    /// Each Session's latest confirmed-usage calibration, outliving the
    /// execution that observed it.
    calibrations: super::continuation::admission::SessionCalibrations,
    compaction_recovery: super::continuation::compaction_recovery::Registry,
    locations: super::locations::SessionLocations,
    /// The same registry the executor snapshotted from, so a call reaches the
    /// connection the catalog was read from rather than a second copy of it.
    mcp: Arc<super::mcp::McpRegistry>,
    plugins: Arc<super::plugins::PluginRuntime>,
    session_capabilities: Arc<Mutex<super::context::SessionCapabilities>>,
    native_mail: Arc<super::native_mail::custody::NativeMailOwners>,
    /// The route the subagent tool uses to run its child through the one
    /// execution owner.
    child_seam: super::subagent::ChildSeam,
}

pub(super) struct LiveProviders {
    configured_provider: ProviderId,
    configured_model: ModelId,
    targets: Arc<PlatformTargets>,
    first_target: Mutex<Option<ResolvedModelTarget>>,
    openers: BTreeMap<ProviderId, Arc<AttemptOpener>>,
}

impl std::fmt::Debug for LiveProviders {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LiveProviders(<bound generation>)")
    }
}

impl LocationRuntime for LiveLocation {
    type Attempts = LiveAttempts;
    type History = DurableHistory;
    type Tools = PermissionToolRunner;
    type Owner = LiveOwner;

    fn input_step_limit(&self, session: &SessionId) -> Result<Option<NonZeroU32>, LocationError> {
        let read = || -> Result<Option<NonZeroU32>, String> {
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            let record = SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                .get(session)
                .map_err(|error| error.to_string())?;
            let agents = super::agent_definitions::Catalog::read(
                &self.service_root,
                Path::new(record.placement.directory()),
            )?;
            Ok(agents
                .selected(record.agent.as_deref().unwrap_or("default"))?
                .and_then(|agent| agent.steps))
        };
        read().map_err(|reason| LocationError::Refused {
            location: self.service_root.to_string_lossy().into_owned(),
            session: session.as_str().to_owned(),
            reason,
        })
    }

    fn notify_pending_inputs_changed(&self, session: &SessionId) {
        self.hub.invalidate_session_because(
            session,
            &format!("session {session}: retired stale automatic resume input"),
        );
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn open_chain(
        &self,
        session: &SessionId,
        promotion: &Promotion,
        cursor: HistoryCursor,
    ) -> Result<StepChain<Self>, LocationError> {
        let refusal = |reason: String| LocationError::Refused {
            location: self.service_root.to_string_lossy().into_owned(),
            session: session.as_str().to_owned(),
            reason,
        };
        let Preflight {
            providers,
            record,
            compaction,
            targets,
            issued,
            tool_root,
            policy,
        } = self.preflight(session).map_err(&refusal)?;
        let bootstrap = match providers.first_target.lock() {
            Ok(mut target) => target.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let first_target = match bootstrap {
            Some(target) if target.is_same_issuance(&issued) => target,
            _ => issued,
        };
        let prompt_principal =
            self.principals
                .get(session)
                .ok_or_else(|| LocationError::Refused {
                    location: self.service_root.to_string_lossy().into_owned(),
                    session: session.as_str().to_owned(),
                    reason: "authenticated prompt principal is unavailable".to_owned(),
                })?;
        if wake_custody::workspace_key(&tool_root).map_err(&refusal)? != prompt_principal.workspace
        {
            return Err(refusal(
                "Session workspace changed after its wake was admitted; retry after it settles"
                    .into(),
            ));
        }
        let hooks = Arc::new(self.load_hooks(&tool_root).map_err(&refusal)?.with_notices(
            super::hooks::NoticeContext {
                inbox: Arc::clone(&self.inbox),
                ids: Arc::clone(&self.ids),
                clock: Arc::clone(&self.clock),
                scope: grapt_session::notice::LifecycleHookScope::Execution(
                    prompt_principal.execution.clone(),
                ),
            },
        ));
        let child = {
            let inbox = self.inbox.lock().unwrap_or_else(|error| error.into_inner());
            grapt_subagent::SubagentService::child_context_on(inbox.store(), session)
                .map_err(|error| refusal(error.to_string()))?
                .is_some()
        };
        if !child {
            self.session_ends
                .remember(session, &hooks)
                .map_err(&refusal)?;
        }
        let prompt = promoted_text(&self.inbox, session, promotion).map_err(|reason| {
            LocationError::Refused {
                location: self.service_root.to_string_lossy().into_owned(),
                session: session.as_str().to_owned(),
                reason,
            }
        })?;
        // Committed before the promotion is published: a client that reads
        // history when it sees the promotion (ACP) then finds the notice.
        // Its sequence still follows the promotion's, so it reads after the
        // message it answers.
        if let Some(unavailable) = self.principals.take_mcp_notice(session) {
            observations::record_mcp_notice(
                self,
                session,
                &prompt_principal.execution,
                unavailable,
            );
        }
        observations::record_config_notices(
            self,
            session,
            &prompt_principal.execution,
            &compaction,
        );
        {
            let inbox = match self.inbox.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            super::input_events::publish_promoted(
                &inbox,
                &self.hub,
                session,
                promotion.input.message(),
            );
        }
        let artifacts = if self.ephemeral {
            Ok(self.tool_artifacts.shared_clone())
        } else {
            open_artifacts(&self.service_root)
        }
        .map_err(|error| LocationError::Refused {
            location: self.service_root.to_string_lossy().into_owned(),
            session: session.as_str().to_owned(),
            reason: error.to_string(),
        })?
        .with_timing(timing::session_context(
            &self.timing,
            session,
            grapt_types::timing::TimingClass::Content,
        ));
        let step = next_step(&self.inbox, session).map_err(&refusal)?;
        let prompt_cache_key = super::cache_scope::session_cache_key(
            &self.service_root,
            &self.inbox,
            session,
            &record.project,
            &first_target,
        )
        .map_err(&refusal)?;
        let selection = grapt_event::session_message::AssistantSelection::new(
            prompt_principal.agent.clone(),
            first_target.provider().clone(),
            first_target.model().clone(),
            first_target.model_ref().variant().cloned(),
        );
        // The live Step opening is checked against the durable one, so it
        // names the ISSUED target, never the stored selection: a Session with
        // no stored effort runs at the account default, and naming the stored
        // one refused every live update of the chain (benchmark ae5ce3a8).
        let mut live_model = serde_json::json!({
            "providerID": first_target.provider().as_str(),
            "id": first_target.model().as_str(),
        });
        if let Some(variant) = first_target.model_ref().variant() {
            live_model["variant"] = serde_json::json!(variant.as_str());
        }
        let owner = DurableAttemptOwner::elect_inbox_owned(
            Arc::clone(&self.inbox),
            artifacts,
            Arc::clone(&self.clock) as Arc<dyn ClockAuthority + Send + Sync>,
            Arc::clone(&self.ids) as Arc<dyn IdAuthority + Send + Sync>,
            LiveSink {
                artifacts: Arc::clone(&self.tool_artifacts),
                tools: BTreeMap::new(),
                inbox: Arc::clone(&self.inbox),
                hub: Arc::clone(&self.hub),
                ids: Arc::clone(&self.ids),
                clock: Arc::clone(&self.clock),
                session: session.clone(),
                execution: prompt_principal.execution.clone(),
                publication: Arc::clone(&self.publication),
                blocks: Vec::new(),
                pending: std::collections::VecDeque::new(),
                refused: Vec::new(),
                attempt: None,
                text_ordinal: 0,
                reasoning_ordinal: 0,
                attempt_ordinals: (0, 0),
                step,
                agent: WireId::parse(prompt_principal.agent.as_str().to_owned())
                    .ok_or_else(|| refusal("executor agent identity is invalid".into()))?,
                model: super::decode(live_model).map_err(refusal)?,
                message: None,
            },
            session.clone(),
            step,
            None,
        )
        .map_err(|error| LocationError::Refused {
            location: self.service_root.to_string_lossy().into_owned(),
            session: session.as_str().to_owned(),
            reason: error.to_string(),
        })?;
        let owner = owner
            .with_timing(Arc::clone(&self.timing))
            .with_selection(selection)
            .map_err(|error| refusal(error.to_string()))?;
        let opener = providers
            .openers
            .get(first_target.provider())
            .ok_or_else(|| refusal("selected provider opener absent".into()))?;
        let admission_diagnosis = super::continuation::admission::DiagnosisSlot::default();
        let continuation = DurableContinuation::new(
            Arc::clone(&self.inbox),
            Arc::clone(&self.tool_artifacts),
            session.clone(),
            promotion.input.message().clone(),
        )
        .with_background_recovery(
            super::continuation::background::notice(
                &self.inbox,
                &self.tool_artifacts,
                session,
                &self.terminals,
            )
            .map_err(refusal)?,
        )
        .with_coverage(admission_diagnosis.clone())
        .with_compaction_recovery(self.compaction_recovery.clone());
        let retry_feedback = super::session_status::RetryFeedback::new(
            Arc::clone(&self.active),
            session.clone(),
            prompt_principal.execution.clone(),
            first_target.provider().clone(),
        );
        self.restore_calibration(session, &first_target);
        let mut plan = ProductPlan::new(
            targets.protocol_family(),
            prompt.clone(),
            prompt_cache_key.clone(),
            Some(first_target),
            targets.clone(),
            // This Session's own snapshot. Every attempt in the execution
            // plans against the same catalog.
            self.published.get(session),
            continuation.share(),
            step,
        )
        .with_ephemeral(self.ephemeral)
        .with_retry_feedback(retry_feedback.clone())
        .with_trim_feedback(context_trim::TrimFeedback::new(
            self,
            session.clone(),
            prompt_principal.execution.clone(),
        ))
        .with_codex_session(session.clone())
        .with_instructions(super::instructions::EffectiveInstructions {
            roots: prompt_principal.instruction_roots.clone(),
            inbox: Arc::clone(&self.inbox),
            session: session.clone(),
            ids: Arc::clone(&self.ids) as Arc<dyn IdAuthority + Send + Sync>,
            clock: Arc::clone(&self.clock) as Arc<dyn ClockAuthority + Send + Sync>,
        })
        .with_admission_diagnosis(admission_diagnosis.clone())
        .with_compaction(compaction)
        .with_policy(policy.clone())
        .with_shell_description(self.shell_description(&tool_root).map_err(refusal)?)
        .with_web_search(
            super::web_search::read(&self.service_root, Path::new(record.placement.directory()))
                .map_err(refusal)?,
        )
        .with_session_calibration(self.calibrations.clone(), session.clone());
        if !self.ephemeral {
            plan = plan
                .with_continuity_resume(super::stateengine_resume::ResumeContext::new(
                    (
                        session.clone(),
                        prompt_principal.agent.clone(),
                        super::session_config::registry_key(
                            &self.inbox,
                            session,
                            record.placement.directory(),
                        )
                        .map_err(refusal)?,
                    ),
                    (
                        Arc::clone(&self.inbox),
                        Arc::clone(&self.session_capabilities),
                        Arc::clone(&self.mcp),
                    ),
                    self.published.get(session),
                    Arc::clone(&self.ids) as Arc<dyn IdAuthority + Send + Sync>,
                ))
                .with_mail_owner(self.native_mail.plan(session).map_err(refusal)?)
                .with_lifecycle(Arc::clone(&hooks), session.clone());
            if let Some(generation) = self.plugins.generation() {
                plan = plan.with_context_hook(
                    Arc::clone(&self.plugins),
                    generation,
                    session.clone(),
                    prompt_principal.agent.clone(),
                    prompt_principal.principal.clone(),
                );
            }
        }
        let attempts = self
            .provider_attempts(
                opener,
                session,
                &record,
                plan,
                retry_feedback.clone(),
                &prompt_principal.execution,
            )
            .map_err(&refusal)?;
        let server_side = compaction.server_side_on(targets.protocol_family());
        let compactor = super::continuation::ProductCompactor::new(
            continuation,
            prompt,
            attempts.compaction_call(),
            targets,
            super::continuation::CompactionAuthorities {
                ids: Arc::clone(&self.ids) as Arc<dyn IdAuthority + Send + Sync>,
                clock: Arc::clone(&self.clock) as Arc<dyn ClockAuthority + Send + Sync>,
                hub: Arc::clone(&self.hub),
                feedback: retry_feedback.clone(),
            },
            admission_diagnosis,
        )
        .with_server_side(server_side)
        .with_ceiling(compaction.auto, compaction.model_auto_compact_token_limit)
        .with_hooks(Arc::clone(&hooks))
        .with_routing_session(prompt_cache_key);
        // REGISTRY IDENTITY, WHICH IS NOT THE FILESYSTEM ROOT.
        //
        // `tool_root` is canonicalized because built-in tools need a real,
        // readable directory. The MCP registry needs the same STRING
        // publication keyed its connections under, which is
        // `placement.directory()` unmodified. On Windows the canonical form
        // is a verbatim \\?\ path, so it never matched and every
        // approved call reported `NoSuchServer` for a connected server.
        let tool_location =
            super::session_config::registry_key(&self.inbox, session, record.placement.directory())
                .map_err(refusal)?;
        let chain = StepChain {
            initial_step: step,
            attempts,
            history: DurableHistory {
                inbox: Arc::clone(&self.inbox),
                session: session.clone(),
                generation: HistoryGeneration::new(cursor.value()),
                compactor,
            },
            tools: Arc::new(
                self.configure_tools(
                    PermissionToolRunner::new(
                        tool_root,
                        session.clone(),
                        promotion.input.message().clone(),
                        prompt_principal.agent.clone(),
                        prompt_principal.principal.clone(),
                        record.project,
                        Arc::clone(&self.permissions),
                        Arc::clone(&self.tool_permissions),
                        Arc::clone(&self.inbox),
                        Arc::clone(&self.ids),
                        Arc::clone(&self.clock),
                        Arc::clone(&self.tool_artifacts),
                        Arc::clone(&self.hub),
                        // The catalog this Session planned against, so a call resolves
                        // to the SAME route the model was shown, and the registry that
                        // holds the connection it names.
                        self.published.get(session),
                        Arc::clone(&self.mcp),
                        Arc::clone(&self.plugins),
                        Arc::clone(&self.session_capabilities),
                        tool_location,
                        record.placement.clone(),
                        Arc::clone(&self.child_seam),
                        Arc::clone(&self.codemode_workers),
                        Arc::clone(&self.terminals),
                        policy,
                        providers
                            .targets
                            .0
                            .credential_environment_names()
                            .map(str::to_owned)
                            .collect(),
                    ),
                    retry_feedback,
                    hooks,
                ),
            ),
            owner,
        };
        observations::start(self, session, &prompt_principal)
            .map_err(|error| refusal(error.to_string()))?;
        prompt_principal.started.settle(Ok(()));
        Ok(chain)
    }
}

/// What the chain's synchronous refusals established, for open_chain.
struct Preflight {
    providers: Arc<LiveProviders>,
    record: grapt_session::SessionRecord,
    compaction: super::compaction_config::CompactionSettings,
    targets: Arc<dyn super::production::ProductTargetAuthority>,
    issued: ResolvedModelTarget,
    tool_root: PathBuf,
    policy: super::permission_preset::ResolvedPolicy,
}

use task_application::{LiveApplication, OneLocation};

#[derive(Debug)]
pub struct LiveExecutor {
    dispatch: grapt_provider::DispatchObserver,
    error_text: grapt_provider::ProviderErrorText,
    coordinator: SessionCoordinator<LiveApplication>,
    artifacts: Arc<FilesystemArtifactStore>,
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<OsRandomIds>,
    clock: Arc<SystemClock>,
    hub: Arc<DogfoodEventHub>,
    publication: Arc<PublicationLedger>,
    interrupted: InterruptTruth,
    principals: Arc<PromptPrincipals>,
    active: Arc<LiveActiveExecutions>,
    wake_slots: Arc<wake_custody::WakeSlots>,
    mcp: Arc<super::mcp::McpRegistry>,
    plugins: Arc<super::plugins::PluginRuntime>,
    published: PublishedSnapshots,
    dispatch_samples: Mutex<grapt_provider::DispatchSamples>,
    tail_samples: Mutex<timing::TailSamples>,
    provider_errors: super::provider_errors::ProviderErrors,
    location: Arc<LiveLocation>,
}

#[cfg(test)]
#[path = "execution/published_snapshot_tests.rs"]
mod published_snapshot_tests;

pub(super) struct LiveDependencies {
    pub(super) catalog: Arc<super::CatalogProjection>,
    pub(super) wake_slots: Arc<WakeSlots>,
    pub(super) inbox: Arc<Mutex<Inbox>>,
    pub(super) status: Arc<super::session_status::SessionStatusEmitter>,
    pub(super) ids: Arc<OsRandomIds>,
    pub(super) clock: Arc<SystemClock>,
    pub(super) hub: Arc<DogfoodEventHub>,
    pub(super) permissions: Arc<Mutex<PermissionHttpState>>,
    pub(super) tool_permissions: Arc<ToolPermissionAuthority>,
    pub(super) mcp: Arc<super::mcp::McpRegistry>,
    pub(super) plugins: Arc<super::plugins::PluginRuntime>,
    pub(super) session_capabilities: Arc<Mutex<super::context::SessionCapabilities>>,
    pub(super) native_mail: Arc<super::native_mail::custody::NativeMailOwners>,
    /// The late-bound route back to the application, for the subagent tool.
    pub(super) child_seam: super::subagent::ChildSeam,
}

impl LiveExecutor {
    pub(super) fn terminals(&self) -> Arc<super::tools::TerminalRegistry> {
        Arc::clone(&self.location.terminals)
    }

    pub(super) fn close_terminals(&self, session: &SessionId) {
        self.location.terminals.close_session(session);
        self.location.session_ends.forget(session);
    }

    pub(super) fn cancel_processes(&self, session: &SessionId) {
        self.location.terminals.cancel_session(session);
    }

    pub(super) async fn stop_processes(&self, session: &SessionId) -> Result<(), String> {
        let (terminals, workers) = tokio::join!(
            self.location.terminals.stop_session(session),
            self.location.codemode_workers.stop_session(session),
        );
        let failures: Vec<_> = [terminals, workers]
            .into_iter()
            .filter_map(Result::err)
            .collect();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    pub(super) fn session_ends(&self) -> Arc<super::hooks::session_end::SessionEnds> {
        Arc::clone(&self.location.session_ends)
    }

    /// The target a Session with this selection runs on, resolved exactly as
    /// its Steps resolve it.
    pub(super) fn session_target(
        &self,
        selected: Option<&serde_json::Value>,
    ) -> Result<ResolvedModelTarget, String> {
        self.location.resolve_target(selected)
    }

    pub fn artifacts(&self) -> &FilesystemArtifactStore {
        &self.artifacts
    }

    pub fn active_executions(&self) -> Vec<(SessionId, ExecutionId)> {
        self.active.projected(self.coordinator.active_sessions())
    }
}

mod helpers;
pub(super) use helpers::next_step;
use helpers::{
    dto_string, durable_ref, finite, promoted_text, wire_event, wire_message, wire_session,
};

#[cfg(test)]
#[path = "execution/announce_tests.rs"]
mod announce_tests;
#[cfg(test)]
#[path = "execution/interrupt_truth_tests.rs"]
mod interrupt_truth_tests;
#[cfg(test)]
#[path = "execution/latch_tests.rs"]
mod latch_tests;

#[cfg(test)]
mod allocation_tests;

mod agent_definitions;
mod agent_mail;
mod authority_path;
mod backend_result;
mod background_child;
mod cache_scope;
mod capture;
mod carry;
mod catalog_config;
mod catalog_markdown;
mod catalog_mcp;
mod catalog_origin;
mod catalogs;
pub(crate) mod claude_plugins;
mod compact_route;
mod compaction_config;
pub(crate) mod connection_setup;
mod connections;
mod context;
mod context_projection;
mod continuation;
mod continuity_attention;
mod elicitation;
#[cfg(test)]
mod elicitation_registry_tests;
mod ephemeral;
mod execution;
pub(crate) mod first_use;
mod followup;
mod fork_route;
mod form;
mod form_catalog;
mod global_form;
mod global_form_events;
mod history;
mod history_compaction;
mod history_json;
mod history_notices;
mod hook_import;
mod hooks;
mod host_control;
mod host_resume;
mod human_input;
mod input_events;
mod input_metadata;
mod instructions;
mod interrupt_pending;
mod launch_mode;
mod legacy_names;
pub(crate) mod lifecycle;
mod list;
mod list_metadata;
mod locations;
mod mcp;
mod mcp_config;
mod mcp_content;
mod metadata_bound;
mod model_inventory;
mod native_mail;
mod path_display;
mod pending_list;
mod permission;
mod permission_preset;
mod permission_view;
#[cfg(feature = "loopback-provider-testkit")]
mod permission_view_fault;
pub(crate) mod plugins;
pub(crate) mod production;
mod projects;
pub(crate) mod prompt_driver;
mod prompt_images;
mod prompt_submission;
pub(crate) mod provider_capacity;
pub(crate) mod provider_errors;
mod provider_generation;
mod public_terminal;
mod read_projection;
mod reconcile_route;
mod rewind_route;
mod sandbox_worktree_config;
mod schemas;
mod selection;
mod session_config;
mod session_cost;
mod session_create;
mod session_info;
mod session_snapshot;
mod session_status;
mod session_title;
mod side_route;
mod skills_watch;
mod source_cache;
mod source_file;
#[cfg(feature = "loopback-provider-testkit")]
mod startup_fixture;
mod stateengine_resume;
mod step_events;
pub(crate) mod stream_diagnostics;
mod subagent;
mod support;
#[cfg(test)]
mod test_provider;
mod tools;
#[cfg(target_os = "linux")]
pub(crate) use tools::sandbox::posix::helper_main as sandbox_helper_main;
pub(crate) use tools::sandbox::preflight as sandbox_policy_preflight;
#[cfg(windows)]
pub(crate) use tools::shim_main as terminal_shim_main;
mod undo_route;
mod usage;
mod web_search;
mod workspace_diff;
mod workspace_worktree;

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_api::u3a::{
    AgentListResponseDto, AgentLocationQueryDto, ApplicationFuture, CatalogApplicationError,
    DogfoodApplicationPort, FormCancelApplicationError, FormCreateApplicationError,
    FormLookupApplicationError, FormReplyApplicationError, InterruptRequestDto,
    InvalidRequestErrorDto, LocationInfoDto, MessageListQueryDto, ModelDefaultLocationQueryDto,
    ModelDefaultResponseDto, ModelListResponseDto, ModelLocationQueryDto, NoContent,
    PermissionApplicationError, PermissionCreateRequestDto, PermissionCreateResponseDto,
    PermissionGlobalListResponseDto, PermissionListResponseDto, PermissionLocationQueryDto,
    PermissionReplyRequestDto, PermissionResponseDto, PluginListResponseDto, ProjectCurrentDto,
    ProjectCurrentLocationQueryDto, ProjectListResponseDto, PromptRequestDto, PromptResponseDto,
    ProviderListResponseDto, ProviderLocationQueryDto, ServerConnectedEvent,
    SessionActiveResponseDto, SessionCreateRequestDto, SessionListApplicationError,
    SessionListQueryDto, SessionLookupApplicationError, SessionMessagesApplicationError,
    SessionMessagesResponseDto, SessionPromptApplicationError, SessionResponseDto,
    SessionsResponseDto, SseLimits, WireId,
};
use grapt_api::{DogfoodEventHub, SealedPrincipal};
use grapt_artifact::{ArtifactStore, ReadBudget};
use grapt_inbox::{AdmitOutcome, Inbox};
use grapt_permission::{PermissionHttpState, ToolPermissionAuthority};
use grapt_platform::{CredentialAuthority, ProviderTargetAuthority};
use grapt_runtime::{recover_process_loss_attempts, SessionPorts};
use grapt_session::{AssistantMessage, SessionSeed};
use grapt_store::{AggregateId, AttemptRecordCeiling, OsRandomIds, SystemClock, WriteError};
#[cfg(test)]
use grapt_types::ResolvedModelTarget;
use grapt_types::{
    ClockAuthority, DeliveryMode, DeterministicAuthorities, EventId, ExecutionId, FormId,
    IdAuthority, MessageId, PermissionId, ProjectId, SessionId,
};
use serde_json::{json, Value};
use tokio::sync::watch;

pub(crate) use self::execution::LiveEndpoint;
use self::execution::{LiveDependencies, LiveExecutor};
use self::production::ProductionProviderLimits;
pub(crate) use self::public_terminal::{public_terminal, terminal_fragments};
use self::support::{
    active_read_failure, conflict, decode, invalid, not_found, public_content, ready,
    should_execute, static_dto, unknown,
};
use self::usage::CompleteUsage;

const MAX_ACTIVE_EXECUTIONS: usize = 64;
const ATTEMPT_RECEIPT_BYTES: u32 = 2_048;
/// Durable/runtime capture ceiling: one bounded provider block may be 1 MiB.
const ARTIFACT_READ_BYTES: u64 = grapt_runtime::MAX_BLOCK_CAPTURE_BYTES;
/// Public U3a string ceiling. This is deliberately separate from the durable
/// capture bound: the public projection may compact, but never narrow truth.
const PUBLIC_CONTENT_BYTES: usize = 256 * 1_024;
/// Artifact key of the live production execution backend.
pub(crate) const PRODUCTION_ARTIFACT_KEY: &[u8] = b"secode-live-headless1-artifacts-v1";

#[allow(
    missing_debug_implementations,
    reason = "the executor owns opaque provider credential custody"
)]
struct ExecutionBackend(Arc<LiveExecutor>);

#[allow(
    missing_debug_implementations,
    reason = "the execution request carries an opaque authenticated principal"
)]
struct ExecutionRequest {
    started: prompt_driver::Started,
    wake: Option<execution::WakeCustody>,
    external_notice: Option<host_control::ExternalNoticeFence>,
    session: SessionId,
    admission_seq: u64,
    input: MessageId,
    interrupted: watch::Receiver<bool>,
    principal: SealedPrincipal,
    agent: grapt_types::AgentId,
    execution: ExecutionId,
}

impl ExecutionBackend {
    async fn waiting_for_input(&self, session: &SessionId) -> Result<(), String> {
        self.0.waiting_for_input(session).await
    }
    fn close_terminals(&self, session: &SessionId) {
        self.0.close_terminals(session);
    }
    fn cancel_processes(&self, session: &SessionId) {
        self.0.cancel_processes(session);
    }
    async fn stop_terminals(&self, session: &SessionId) -> Result<(), String> {
        self.0.stop_processes(session).await
    }
    fn artifacts(&self) -> &grapt_artifact::FilesystemArtifactStore {
        self.0.artifacts()
    }
    fn session_target(
        &self,
        selected: Option<&serde_json::Value>,
    ) -> Result<Option<grapt_types::ModelRef>, String> {
        self.0
            .session_target(selected)
            .map(|target| Some(target.model_ref().clone()))
    }
    fn cache_request_metadata(
        &self,
        session: &SessionId,
        attempt: &grapt_types::PhysicalAttemptId,
    ) -> Option<serde_json::Value> {
        self.0.cache_request_metadata(session, attempt)
    }
    fn active_executions(&self) -> Vec<(SessionId, ExecutionId)> {
        self.0.active_executions()
    }
    async fn run(&self, request: ExecutionRequest) -> Result<(), backend_result::ExecutionFailure> {
        let ExecutionRequest {
            started,
            wake,
            external_notice,
            session,
            admission_seq,
            input,
            interrupted,
            principal,
            agent,
            execution,
        } = request;
        self.0
            .run(
                session,
                admission_seq,
                principal,
                agent,
                (
                    execution,
                    started,
                    wake.ok_or_else(|| {
                        backend_result::ExecutionFailure::Unpublished(
                            "production wake custody is absent".into(),
                        )
                    })?,
                    interrupted,
                    input,
                    external_notice,
                ),
            )
            .await
            .map(|_| ())
            .map_err(backend_result::ExecutionFailure::from)
    }
    fn interrupt(
        &self,
        session: &SessionId,
        execution: Option<&ExecutionId>,
    ) -> Result<Option<ExecutionId>, String> {
        self.0.interrupt_target(session, execution)
    }
    /// The complete stop a closed window owes its Session.
    ///
    /// A registered execution alone is not the whole turn: one may still be
    /// preparing, and admitted input may wait behind either. This is the same
    /// path an explicit user stop takes, so a close cannot leave a turn running
    /// where no window is left to watch it. The durable hold that keeps a later
    /// native wake from restarting the turn lives beside it in
    /// `DogfoodApplication::close_after_last_window`, which owns the inbox.
    fn close(&self, session: &SessionId) -> Result<(), String> {
        self.0
            .interrupt(session, None)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

pub(crate) fn event_hub() -> Result<Arc<DogfoodEventHub>, String> {
    event_hub_with_grace(grapt_api::u3a::DEFAULT_CLOSE_GRACE)
}

/// The same hub with a service deployment's own silent-loss grace.
pub(crate) fn event_hub_with_grace(
    close_grace: std::time::Duration,
) -> Result<Arc<DogfoodEventHub>, String> {
    let ids = OsRandomIds::seed().map_err(|error| error.to_string())?;
    let connected_id: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|_| "server-connected event id was invalid".to_owned())?;
    let connected = WireId::parse(connected_id.as_str().to_owned())
        .ok_or_else(|| "server-connected wire id was invalid".to_owned())?;
    let limits = SseLimits::production().map_err(|error| error.to_string())?;
    Ok(Arc::new(
        DogfoodEventHub::with_close_grace(
            limits,
            ServerConnectedEvent::new(connected),
            close_grace,
        )
        .map_err(|error| error.to_string())?,
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum DogfoodRefusal {
    #[error("dogfood composition refused: {0}")]
    Compose(String),
}

#[derive(Debug, Clone)]
struct CatalogProjection {
    accounts: std::collections::BTreeMap<grapt_types::ProviderId, grapt_types::AccountId>,
    protocols: std::collections::BTreeMap<grapt_types::ProviderId, grapt_provider::ProtocolFamily>,
    inventory: Option<Arc<grapt_platform::ModelInventory>>,
    project: ProjectId,
    directory: String,
    provider: String,
    model: String,
    default_effort: Option<grapt_types::ModelVariantId>,
    credential_record: Option<String>,
}

#[allow(
    missing_debug_implementations,
    reason = "the application owns the seeded production identity adapter"
)]
#[derive(Clone)]
pub struct DogfoodApplication {
    readers: Option<grapt_store::read_pool::ReadPool>,
    #[cfg(test)]
    _test_provider: Arc<std::sync::OnceLock<test_provider::Server>>,
    external_host: bool,
    service_root: PathBuf,
    wake_slots: Arc<execution::WakeSlots>,
    verified_carries: Arc<carry::VerifiedSessions>,
    prompt_mailbox: prompt_driver::PromptMailbox,
    agent_activity: tokio::sync::watch::Sender<()>,
    hub: Arc<DogfoodEventHub>,
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<OsRandomIds>,
    clock: Arc<SystemClock>,
    providers: provider_generation::Publication,
    skills_watch: Arc<skills_watch::Watch>,
    executor: Arc<ExecutionBackend>,
    permissions: Arc<Mutex<PermissionHttpState>>,
    permission_view: Arc<permission_view::PermissionView>,
    tool_permissions: Arc<ToolPermissionAuthority>,
    session_capabilities: Arc<Mutex<context::SessionCapabilities>>,
    native_mail: Option<Arc<native_mail::custody::NativeMailOwners>>,
    /// Configured MCP servers, started at most once and then HELD. It is on
    /// the application rather than on a request so a status poll reports a
    /// live session instead of forking a process per call.
    mcp: Arc<mcp::McpRegistry>,
    plugins: Arc<plugins::PluginRuntime>,
    /// This process's subagent runtime generation, taken once at composition.
    /// It separates the operations this runtime owns from those a dead one
    /// left behind; see [`subagent::adopt_runtime`].
    runtime_generation: grapt_types::RuntimeGeneration,
    /// Bounds of the lifetime metrics read behind every Session row.
    metrics_bound: MetricsBound,
    /// The launch modes this process has observed, so the close-grace deadline
    /// loop can honour a headless launch without a store read; see
    /// [`launch_mode::Launches`]. Shared across clones: every clone of one
    /// application answers the stop question from the same observations.
    launches: std::sync::Arc<launch_mode::Launches>,
}

/// A Session row's lifetime metrics take the accounting read's producer-day
/// bounds, not the transcript page's `MAX_HISTORY_ROWS` (SC-HIST-01).
#[derive(Debug, Clone, Copy)]
struct MetricsBound {
    rows: usize,
    bytes: usize,
}

impl MetricsBound {
    const PRODUCTION: Self = Self {
        rows: grapt_session::history::accounting::MAX_ACCOUNTING_ROWS,
        bytes: grapt_session::history::accounting::MAX_ACCOUNTING_BYTES,
    };
}

enum SessionMetrics {
    Known {
        usage: Option<CompleteUsage>,
        updated_ms: i64,
        accounting: Option<grapt_session::history::accounting::AccountingSnapshot>,
    },
    /// The lifetime read refused at its bound. The row still exists; its
    /// totals are an honest unknown, never a truncated sum.
    ExceedsBound { updated_ms: i64 },
}

impl DogfoodApplication {
    #[allow(clippy::too_many_arguments)]
    pub fn open_production(
        service_root: &Path,
        store: grapt_store::EventStore,
        targets: ProviderTargetAuthority,
        credential: CredentialAuthority,
        secondary_credentials: Vec<(grapt_types::ProviderId, CredentialAuthority)>,
        endpoint: LiveEndpoint,
        limits: ProductionProviderLimits,
        hub: Arc<DogfoodEventHub>,
        plugins: Arc<plugins::PluginRuntime>,
    ) -> Result<(Arc<Self>, prompt_driver::PromptDriver), DogfoodRefusal> {
        Self::open_production_mode(
            service_root,
            store,
            targets,
            credential,
            secondary_credentials,
            endpoint,
            limits,
            hub,
            plugins,
            false,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_production_with_external_host(
        service_root: &Path,
        store: grapt_store::EventStore,
        targets: ProviderTargetAuthority,
        credential: CredentialAuthority,
        secondary_credentials: Vec<(grapt_types::ProviderId, CredentialAuthority)>,
        endpoint: LiveEndpoint,
        limits: ProductionProviderLimits,
        hub: Arc<DogfoodEventHub>,
        plugins: Arc<plugins::PluginRuntime>,
        external_host: bool,
    ) -> Result<(Arc<Self>, prompt_driver::PromptDriver), DogfoodRefusal> {
        if !external_host {
            return Self::open_production(
                service_root,
                store,
                targets,
                credential,
                secondary_credentials,
                endpoint,
                limits,
                hub,
                plugins,
            );
        }
        Self::open_production_mode(
            service_root,
            store,
            targets,
            credential,
            secondary_credentials,
            endpoint,
            limits,
            hub,
            plugins,
            false,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn open_production_mode(
        service_root: &Path,
        mut store: grapt_store::EventStore,
        targets: ProviderTargetAuthority,
        credential: CredentialAuthority,
        secondary_credentials: Vec<(grapt_types::ProviderId, CredentialAuthority)>,
        endpoint: LiveEndpoint,
        limits: ProductionProviderLimits,
        hub: Arc<DogfoodEventHub>,
        plugins: Arc<plugins::PluginRuntime>,
        ephemeral: bool,
        external_host: bool,
    ) -> Result<(Arc<Self>, prompt_driver::PromptDriver), DogfoodRefusal> {
        #[cfg(feature = "loopback-provider-testkit")]
        if !ephemeral {
            startup_fixture::observe(service_root, &mut store).map_err(DogfoodRefusal::Compose)?;
        }
        schemas::install_fresh(&mut store).map_err(DogfoodRefusal::Compose)?;
        grapt_instruction::InstructionService::ensure_tables(&mut store)
            .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
        grapt_form::FormService::initialize(&mut store)
            .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
        grapt_shell::schema::initialize(&mut store)
            .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
        PermissionHttpState::initialize(&mut store)
            .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
        let ids = Arc::new(
            OsRandomIds::seed().map_err(|error| DogfoodRefusal::Compose(error.to_string()))?,
        );
        let clock = Arc::new(SystemClock);
        let receipt_bytes = NonZeroU32::new(ATTEMPT_RECEIPT_BYTES)
            .ok_or_else(|| DogfoodRefusal::Compose("attempt receipt bound is zero".to_owned()))?;
        let attempts = recover_process_loss_attempts(
            &mut store,
            clock.as_ref(),
            AttemptRecordCeiling::new(receipt_bytes),
        )
        .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
        for warning in &attempts.warnings {
            eprintln!("startup warning reason=attempt-recovery-skipped detail={warning}");
        }
        let forms = elicitation::recover(&mut store, ids.as_ref(), clock.as_ref())
            .map_err(DogfoodRefusal::Compose)?;
        for warning in &forms {
            eprintln!("startup warning reason=form-recovery-skipped detail={warning}");
        }
        session_status::SessionStatusEmitter::recover(&mut store, &clock, &ids);
        let inbox =
            Inbox::new(store).map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
        let readers = inbox
            .store()
            .read_pool()
            .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
        let inbox = Arc::new(Mutex::new(inbox));
        let mcp = Arc::new(
            mcp::registry_with_forms(
                (!ephemeral).then_some(service_root),
                inbox.clone(),
                ids.clone(),
                clock.clone(),
                hub.clone(),
            )
            .map_err(DogfoodRefusal::Compose)?,
        );
        let catalog = CatalogProjection::production(service_root, &targets, &credential)
            .map_err(DogfoodRefusal::Compose)?;
        let tool_permissions = Arc::new(ToolPermissionAuthority::default());
        let permissions = Arc::new(Mutex::new(PermissionHttpState::default()));
        // One registry, shared. The executor needs it to publish a session's
        // MCP tools to the model; the connection endpoint needs it to report
        // status. A second registry would start a second copy of every
        // configured server and report on connections nobody is calling.

        let session_capabilities = Arc::new(Mutex::new(context::SessionCapabilities::default()));
        let native_mail = if external_host {
            native_mail::custody::NativeMailOwners::with_external_host(
                inbox.clone(),
                session_capabilities.clone(),
                ids.clone(),
                clock.clone(),
                hub.clone(),
                service_root,
                true,
            )
        } else {
            native_mail::custody::NativeMailOwners::new(
                inbox.clone(),
                session_capabilities.clone(),
                ids.clone(),
                clock.clone(),
                hub.clone(),
                service_root,
            )
        };
        // The subagent tool runs its child through THIS application, and the
        // application is not built yet — it owns the executor that owns the
        // location that builds the tool runner. So the seam is created here,
        // threaded down empty, and populated the moment the application is in
        // its `Arc`, a few lines below.
        let child_seam: subagent::ChildSeam = Arc::new(std::sync::OnceLock::new());
        let status = session_status::SessionStatusEmitter::journaled(&ids, &clock, &hub, &inbox);
        let wake_slots = Arc::new(execution::WakeSlots::default());
        let open_executor = if ephemeral {
            LiveExecutor::open_ephemeral
        } else {
            LiveExecutor::open
        };
        let executor = open_executor(
            service_root,
            LiveDependencies {
                catalog: Arc::clone(&catalog),
                wake_slots: Arc::clone(&wake_slots),
                inbox: Arc::clone(&inbox),
                ids: Arc::clone(&ids),
                clock: Arc::clone(&clock),
                hub: Arc::clone(&hub),
                status: Arc::clone(&status),
                permissions: Arc::clone(&permissions),
                tool_permissions: Arc::clone(&tool_permissions),
                mcp: Arc::clone(&mcp),
                plugins: Arc::clone(&plugins),
                session_capabilities: Arc::clone(&session_capabilities),
                native_mail: Arc::clone(&native_mail),
                child_seam: Arc::clone(&child_seam),
            },
            targets,
            credential,
            secondary_credentials,
            endpoint,
            limits,
        )
        .map_err(|error| DogfoodRefusal::Compose(error.to_string()))?;
        let (prompt_mailbox, driver) = prompt_driver::PromptMailbox::new();
        let runtime_generation = subagent::adopt_runtime(&inbox, clock.now().as_millis())
            .map_err(DogfoodRefusal::Compose)?;
        let permission_view = permission_view::PermissionView::attach(&inbox);
        let mut driver = driver;
        driver.attach_mcp(mcp.clone());
        driver.attach_scan_indexes(inbox.clone());
        if !ephemeral {
            driver.attach_native_mail(native_mail.clone());
            driver.attach_session_ends(executor.session_ends());
        }
        let application = Arc::new(Self {
            readers,
            #[cfg(test)]
            _test_provider: Arc::new(std::sync::OnceLock::new()),
            external_host,
            service_root: service_root.to_path_buf(),
            verified_carries: Arc::new(carry::VerifiedSessions::default()),
            wake_slots,
            prompt_mailbox,
            agent_activity: tokio::sync::watch::channel(()).0,
            hub,
            inbox,
            ids,
            clock,
            providers: executor.provider_publication(),
            skills_watch: skills_watch::Watch::new(),
            executor: Arc::new(ExecutionBackend(Arc::new(executor))),
            permissions,
            permission_view,
            tool_permissions,
            session_capabilities,
            native_mail: (!ephemeral).then_some(native_mail),
            mcp,
            plugins,
            runtime_generation,
            metrics_bound: MetricsBound::PRODUCTION,
            launches: std::sync::Arc::new(launch_mode::Launches::default()),
        });
        // A `Weak`, so the application is not kept alive by the executor it
        // owns. `set` cannot fail here — this is the only writer — and a seam
        // that were somehow left empty refuses the child loudly rather than
        // skipping it.
        let _bound = child_seam.set(Arc::downgrade(&application));
        if let Some(native) = &application.native_mail {
            native.attach_wake_application(Arc::downgrade(&application));
            native.restore_selected();
        }
        lifecycle::attach_notice(&mut driver, &application);
        skills_watch::attach(&mut driver, &application);
        lifecycle::attach_close_grace(&mut driver, &application);
        Ok((application, driver))
    }

    fn mint_event(&self) -> Result<EventId, String> {
        format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| "event identity was invalid".to_owned())
    }

    fn lock_inbox(&self) -> MutexGuard<'_, Inbox> {
        match self.inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn ensure_session_writer(&self, session: &SessionId) -> Result<(), String> {
        self.verify_session_carry(session)?;
        let aggregate = AggregateId::Session(session.clone());
        let mut inbox = self.lock_inbox();
        match inbox.store().fencing_token(&aggregate) {
            Ok(_) => Ok(()),
            Err(WriteError::NotElected { .. }) => inbox
                .store_mut()
                .elect_writer(&aggregate)
                .map(|_| ())
                .map_err(|error| format!("session writer election refused: {error}")),
            Err(error) => Err(format!("session writer authority is unreadable: {error}")),
        }
    }

    fn session_response(
        &self,
        record: &grapt_session::SessionRecord,
    ) -> Result<SessionResponseDto, InvalidRequestErrorDto> {
        decode(json!({"data": self.session_info(record).map_err(invalid)?})).map_err(invalid)
    }

    fn get_record(&self, session: &SessionId) -> Result<grapt_session::SessionRecord, String> {
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        let record = SessionPorts::new(Arc::clone(&self.inbox), &authorities)
            .get(session)
            .map_err(|error| error.to_string())?;
        // Every recorded read of a Session is the moment the launch mode is
        // cheap to observe. Caching it here means the close-grace loop never
        // has to touch the store to honour a headless launch.
        if let Some(launch) = record.launch {
            self.launches.remember(session, launch);
        }
        Ok(record)
    }

    fn message_history(&self, session: &SessionId) -> Result<Vec<AssistantMessage>, String> {
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        SessionPorts::new(Arc::clone(&self.inbox), &authorities)
            .history(session)
            .map_err(|error| error.to_string())
    }

    fn read_artifact(&self, reference: &grapt_types::ArtifactRef) -> Result<String, String> {
        let budget = ReadBudget::new(ARTIFACT_READ_BYTES)
            .ok_or_else(|| "artifact read budget is zero".to_owned())?;
        let mut offset = 0u64;
        let mut bytes = Vec::with_capacity(reference.length() as usize);
        loop {
            let page = self
                .executor
                .artifacts()
                .fetch(reference, offset, budget)
                .map_err(|error| error.to_string())?;
            if bytes.len().saturating_add(page.bytes().len()) > ARTIFACT_READ_BYTES as usize {
                return Err("artifact read exceeded its 1 MiB durable bound".to_owned());
            }
            bytes.extend_from_slice(page.bytes());
            match page.next_offset() {
                Some(next) => offset = next,
                None => break,
            }
        }
        String::from_utf8(bytes).map_err(|_| "artifact is not UTF-8".to_owned())
    }
}

impl DogfoodApplicationPort for DogfoodApplication {
    fn session_rename(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionRenameRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(session_title::manual(
            self,
            session_id.into_inner(),
            request,
        ))
    }
    fn session_agent_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionAgentSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::agent(self, session_id.into_inner(), request))
    }

    fn session_model_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::model(self, session_id.into_inner(), request))
    }
    fn session_preset_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<grapt_api::u3a::SessionPresetResponseDto, SessionLookupApplicationError>
    {
        ready(selection::preset_get(self, session_id.into_inner()))
    }

    fn session_preset_select(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionPresetSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::preset(self, session_id.into_inner(), request))
    }

    fn session_subagent_model_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionSubagentModelResponseDto,
        SessionLookupApplicationError,
    > {
        ready(selection::subagent_get(self, session_id.into_inner()))
    }
    fn session_subagent_model_set(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionModelSelectDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::subagent_set(
            self,
            session_id.into_inner(),
            Some(request),
        ))
    }
    fn session_subagent_model_clear(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(selection::subagent_set(self, session_id.into_inner(), None))
    }
    fn sandbox_grant_list(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SandboxGrantListResponseDto,
        grapt_api::u3a::PermissionApplicationError,
    > {
        ready(permission::grants::list(self, session.into_inner()))
    }
    fn sandbox_grant_revoke(
        &self,
        principal: SealedPrincipal,
        session: WireId<SessionId>,
        request: grapt_api::u3a::SandboxGrantDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::SandboxGrantRevokeResponseDto,
        grapt_api::u3a::PermissionApplicationError,
    > {
        permission::grants::revoke(self, principal, session.into_inner(), request)
    }
    fn permission_saved_list(
        &self,
        _principal: SealedPrincipal,
        project: Option<WireId<ProjectId>>,
    ) -> ApplicationFuture<grapt_api::u3a::PermissionSavedListResponseDto, InvalidRequestErrorDto>
    {
        ready(permission::saved(self, project.map(WireId::into_inner)))
    }
    fn session_form_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<grapt_api::u3a::SessionFormListResponseDto, SessionLookupApplicationError>
    {
        ready((|| {
            let session = session_id.into_inner();
            context::require_session(self, &session)?;
            let held = self.lock_inbox();
            let forms = grapt_form::catalog::pending_forms_for(held.store(), Some(&session))
                .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
            decode(json!({"data": forms.into_iter().map(|form| form.info).collect::<Vec<_>>()}))
                .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))
        })())
    }

    fn global_form_get(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::FormResponseDto,
        grapt_api::u3a::FormLookupApplicationError,
    > {
        Box::pin(global_form::get(self.clone(), form_id.into_inner(), query))
    }
    fn global_form_state(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::FormStateResponseDto,
        grapt_api::u3a::FormLookupApplicationError,
    > {
        Box::pin(global_form::state(
            self.clone(),
            form_id.into_inner(),
            query,
        ))
    }
    fn global_form_reply(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, grapt_api::u3a::FormReplyApplicationError> {
        Box::pin(global_form::reply(
            self.clone(),
            form_id.into_inner(),
            query,
            request,
        ))
    }
    fn global_form_cancel(
        &self,
        _principal: SealedPrincipal,
        form_id: WireId<grapt_types::FormId>,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, grapt_api::u3a::FormCancelApplicationError> {
        Box::pin(global_form::cancel(
            self.clone(),
            form_id.into_inner(),
            query,
        ))
    }

    fn session_form_create(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::FormCreateRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::FormResponseDto, FormCreateApplicationError> {
        form::create_and_notify(self, session_id.into_inner(), request)
    }

    fn session_form_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<grapt_api::u3a::FormResponseDto, FormLookupApplicationError> {
        ready(form::get(
            self,
            session_id.into_inner(),
            form_id.into_inner(),
        ))
    }

    fn session_form_state(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<grapt_api::u3a::FormStateResponseDto, FormLookupApplicationError> {
        ready(form::state(
            self,
            session_id.into_inner(),
            form_id.into_inner(),
        ))
    }

    fn session_form_reply(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
        request: grapt_api::u3a::FormReplyRequestDto,
    ) -> ApplicationFuture<NoContent, FormReplyApplicationError> {
        ready(form::reply(
            self,
            session_id.into_inner(),
            form_id.into_inner(),
            request,
        ))
    }

    fn session_form_cancel(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        form_id: WireId<FormId>,
    ) -> ApplicationFuture<NoContent, FormCancelApplicationError> {
        ready(form::cancel(
            self,
            session_id.into_inner(),
            form_id.into_inner(),
        ))
    }

    fn session_pending_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionPendingListResponseDto,
        SessionLookupApplicationError,
    > {
        ready(self.pending_list(session_id))
    }

    fn shell_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::ShellListResponseDto, InvalidRequestErrorDto> {
        let application = self.clone();
        Box::pin(async move {
            let location = locations::resolve(&application.catalog_snapshot(), &query)
                .await
                .map_err(invalid)?;
            // The route stays for the desktop renderer's route contract, which
            // lists GET /api/shell. Nothing populates a durable shell inventory,
            // so the answer is an empty scoped list and no store is read.
            let data: Vec<grapt_api::u3a::ShellInfoDto> = Vec::new();
            decode(json!({"location": location.wire(), "data":data})).map_err(invalid)
        })
    }
    fn session_capability_put(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionCapabilityPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(context::capability(
            self,
            principal,
            session_id.into_inner(),
            request,
        ))
    }
    fn instruction_entry_list(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<grapt_api::u3a::InstructionEntryListDto, SessionLookupApplicationError>
    {
        ready(context::entries(self, session_id.into_inner()))
    }
    fn instruction_inspect(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<grapt_api::u3a::InstructionInspectionDto, SessionLookupApplicationError>
    {
        ready(context::inspect(self, session_id.into_inner()))
    }
    fn instruction_entry_put(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        key: grapt_api::u3a::InstructionEntryKeyDto,
        request: grapt_api::u3a::InstructionEntryPutDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(context::put(self, session_id.into_inner(), key, request))
    }
    fn instruction_entry_remove(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        key: grapt_api::u3a::InstructionEntryKeyDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(context::remove(self, session_id.into_inner(), key))
    }
    fn integration_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::IntegrationListResponseDto, InvalidRequestErrorDto> {
        connections::integrations(self, query)
    }

    fn plugin_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<PluginListResponseDto, InvalidRequestErrorDto> {
        plugins::list(self, query)
    }

    fn workspace_diff(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::WorkspaceDiffResponseDto, InvalidRequestErrorDto> {
        workspace_diff::response(Arc::clone(&self.catalog_snapshot()), query)
    }

    fn workspace_worktree(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::WorkspaceWorktreeRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::WorkspaceWorktreeResponseDto, InvalidRequestErrorDto>
    {
        workspace_worktree::response(self.clone(), query, request)
    }

    fn hook_import(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::HookImportRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::HookImportResponseDto, InvalidRequestErrorDto> {
        hook_import::request(self.clone(), query, request)
    }

    fn mcp_inspect(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpInspectionResponseDto, InvalidRequestErrorDto> {
        connections::inspect(self, query)
    }

    fn mcp_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpListResponseDto, InvalidRequestErrorDto> {
        connections::response(self.clone(), query, connections::Kind::McpServers)
    }

    fn mcp_configuration_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpConfigurationListResponseDto, InvalidRequestErrorDto>
    {
        connections::configuration(self, query)
    }

    fn mcp_add(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::McpAddRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        connections::add_runtime(self.clone(), server, query, request)
    }

    fn mcp_approval(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpApprovalPreviewDto, InvalidRequestErrorDto> {
        mcp_config::preview(self.clone(), server, query)
    }

    fn mcp_configuration(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::McpConfigurationRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        mcp_config::request(self.clone(), server, query, request)
    }

    fn mcp_reconnect(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        connections::reconnect(self.clone(), server, query)
    }

    fn mcp_auth(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::McpAuthRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpAuthResponseDto, InvalidRequestErrorDto> {
        mcp::auth_api::respond(self.clone(), server, query, request)
    }

    fn mcp_content(
        &self,
        _principal: SealedPrincipal,
        server: String,
        query: ProjectCurrentLocationQueryDto,
        request: grapt_api::u3a::McpContentRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpContentResponseDto, InvalidRequestErrorDto> {
        mcp_content::respond(self.clone(), server, query, request)
    }

    fn mcp_resources(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::McpResourcesResponseDto, InvalidRequestErrorDto> {
        mcp_content::all_resources(self.clone(), query)
    }

    fn question_request_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::QuestionListResponseDto, InvalidRequestErrorDto> {
        let catalog = Arc::clone(&self.catalog_snapshot());
        Box::pin(async move {
            let location = locations::resolve(&catalog, &query)
                .await
                .map_err(invalid)?;
            // Rust emits forms, not legacy question events. Do not turn forms
            // into questions or hide an invalid/unreadable location as empty.
            decode(json!({"location": location.wire(), "data": []})).map_err(invalid)
        })
    }

    fn form_request_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::FormListResponseDto, InvalidRequestErrorDto> {
        form_catalog::list(self.clone(), query)
    }

    fn command_inspect(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::CommandInspectionResponseDto, InvalidRequestErrorDto>
    {
        catalogs::command_response(self.clone(), query, true)
    }

    fn command_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::CommandListResponseDto, InvalidRequestErrorDto> {
        catalogs::command_response(self.clone(), query, false)
    }

    fn skill_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::SkillListResponseDto, InvalidRequestErrorDto> {
        catalogs::transformed_response(
            Arc::clone(&self.catalog_snapshot()),
            Arc::clone(&self.plugins),
            query,
            catalogs::Kind::Skills,
        )
    }

    fn reference_list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<grapt_api::u3a::ReferenceListResponseDto, InvalidRequestErrorDto> {
        catalogs::transformed_response(
            Arc::clone(&self.catalog_snapshot()),
            Arc::clone(&self.plugins),
            query,
            catalogs::Kind::References,
        )
    }

    fn location_get(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<LocationInfoDto, InvalidRequestErrorDto> {
        let catalog = Arc::clone(&self.catalog_snapshot());
        Box::pin(async move {
            let resolved = locations::resolve(&catalog, &query)
                .await
                .map_err(invalid)?;
            decode(resolved.wire()).map_err(invalid)
        })
    }

    fn project_list(
        &self,
        _principal: SealedPrincipal,
    ) -> ApplicationFuture<ProjectListResponseDto, InvalidRequestErrorDto> {
        ready(projects::list(self).map_err(invalid))
    }

    fn project_current(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<ProjectCurrentDto, InvalidRequestErrorDto> {
        let application = self.clone();
        Box::pin(async move {
            let resolved = locations::resolve_for(&application, &query)
                .await
                .map_err(invalid)?;
            decode(
                json!({"id": resolved.project.as_str(), "directory": resolved.project_directory}),
            )
            .map_err(invalid)
        })
    }

    fn provider_list(
        &self,
        _principal: SealedPrincipal,
        query: ProviderLocationQueryDto,
    ) -> ApplicationFuture<ProviderListResponseDto, CatalogApplicationError> {
        let application = self.clone();
        let plugins = Arc::clone(&self.plugins);
        let draft = match plugins::catalog_draft(self) {
            Ok(draft) => draft,
            Err(error) => {
                return ready(Err(CatalogApplicationError::InvalidRequest(invalid(error))));
            }
        };
        Box::pin(async move {
            let query = serde_json::to_value(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let query = decode(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let location = locations::resolve_for(&application, &query)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let transformed = plugins
                .transform_current("catalog", draft)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let data = transformed["providers"]
                .as_array()
                .ok_or_else(|| {
                    CatalogApplicationError::InvalidRequest(invalid(
                        "plugin catalog transform removed the provider list",
                    ))
                })?
                .iter()
                .map(|record| record["provider"].clone())
                .collect::<Vec<_>>();
            decode(json!({"location":location.wire(),"data":data}))
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
        })
    }

    fn model_list(
        &self,
        _principal: SealedPrincipal,
        query: ModelLocationQueryDto,
    ) -> ApplicationFuture<ModelListResponseDto, CatalogApplicationError> {
        let plugins = Arc::clone(&self.plugins);
        let draft = match plugins::catalog_draft(self) {
            Ok(draft) => draft,
            Err(error) => {
                return ready(Err(CatalogApplicationError::InvalidRequest(invalid(error))));
            }
        };
        let application = self.clone();
        Box::pin(async move {
            let query = serde_json::to_value(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let query = decode(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let location = locations::resolve_for(&application, &query)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let transformed = plugins
                .transform_current("catalog", draft)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let data = transformed["providers"]
                .as_array()
                .ok_or_else(|| {
                    CatalogApplicationError::InvalidRequest(invalid(
                        "plugin catalog transform removed the provider list",
                    ))
                })?
                .iter()
                .flat_map(|record| record["models"].as_array().into_iter().flatten().cloned())
                .collect::<Vec<_>>();
            decode(json!({"location":location.wire(),"data":data}))
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
        })
    }

    fn model_default(
        &self,
        _principal: SealedPrincipal,
        query: ModelDefaultLocationQueryDto,
    ) -> ApplicationFuture<ModelDefaultResponseDto, CatalogApplicationError> {
        let plugins = Arc::clone(&self.plugins);
        let draft = match plugins::catalog_draft(self) {
            Ok(draft) => draft,
            Err(error) => {
                return ready(Err(CatalogApplicationError::InvalidRequest(invalid(error))));
            }
        };
        let application = self.clone();
        Box::pin(async move {
            let query = serde_json::to_value(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let query = decode(query)
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let location = locations::resolve_for(&application, &query)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let transformed = plugins
                .transform_current("catalog", draft)
                .await
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
            let selected = transformed.get("default").cloned().unwrap_or(Value::Null);
            let provider = selected.get("providerID").and_then(Value::as_str);
            let model = selected.get("modelID").and_then(Value::as_str);
            let data = transformed["providers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|record| record["provider"]["id"].as_str() == provider)
                .flat_map(|record| record["models"].as_array().into_iter().flatten())
                .find(|item| item["id"].as_str() == model)
                .cloned()
                .unwrap_or(Value::Null);
            decode(json!({"location":location.wire(),"data":data}))
                .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
        })
    }

    fn agent_list(
        &self,
        _principal: SealedPrincipal,
        query: AgentLocationQueryDto,
    ) -> ApplicationFuture<AgentListResponseDto, InvalidRequestErrorDto> {
        let plugins = Arc::clone(&self.plugins);
        let application = self.clone();
        Box::pin(async move {
            let query = serde_json::to_value(query).map_err(invalid)?;
            let query = decode(query).map_err(invalid)?;
            let location = locations::resolve_for(&application, &query)
                .await
                .map_err(invalid)?;
            let agents = selection::agent_inventory(
                &application.catalog_snapshot(),
                plugins.service_root(),
                Path::new(&location.directory),
            )
            .map_err(invalid)?;
            let transformed = plugins
                .transform_current("agent", json!({"items":agents,"default":null}))
                .await
                .map_err(invalid)?;
            let data = transformed
                .get("items")
                .cloned()
                .ok_or_else(|| invalid("plugin agent transform removed the item list"))?;
            decode(json!({"location":location.wire(),"data":data})).map_err(invalid)
        })
    }

    fn session_list(
        &self,
        _principal: SealedPrincipal,
        query: SessionListQueryDto,
    ) -> ApplicationFuture<SessionsResponseDto, SessionListApplicationError> {
        self.read_projection(
            move |application| list::response(application, query),
            |_| SessionListApplicationError::Unavailable,
        )
    }

    fn session_active(
        &self,
        _principal: SealedPrincipal,
    ) -> ApplicationFuture<SessionActiveResponseDto, grapt_api::u3a::SessionActiveApplicationError>
    {
        self.read_projection(
            // Both the projection's own refusal and the reader wait go through one
            // classifier, so a busy wait cannot become a bad request on either path.
            |application| {
                application
                    .active_session_response()
                    .map_err(|detail| active_read_failure(&detail))
            },
            |detail| active_read_failure(&detail),
        )
    }

    fn session_create(
        &self,
        _principal: SealedPrincipal,
        request: SessionCreateRequestDto,
        launch: Option<grapt_types::SessionLaunch>,
    ) -> ApplicationFuture<SessionResponseDto, InvalidRequestErrorDto> {
        let application = self.clone();
        Box::pin(async move {
            let requested_id = request.id().as_value().map(|id| id.as_inner().clone());
            let request = serde_json::to_value(request).map_err(invalid)?;
            let prepared = application
                .prepare_create(&request, requested_id, launch)
                .await?;
            let created = {
                let authorities =
                    DeterministicAuthorities::new(&*application.clock, &*application.ids);
                SessionPorts::new(Arc::clone(&application.inbox), &authorities)
                    .create(&prepared.seed)
                    .map_err(invalid)?
            };
            let record = application.get_record(&created.session).map_err(invalid)?;
            sandbox_worktree_config::observe(
                &application,
                &created.session,
                Path::new(record.placement.directory()),
            )
            .await;
            application.session_response(&record)
        })
    }

    fn session_get(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
        self.read_session(session_id.into_inner())
    }

    fn session_sandbox_worktree_config_get(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SandboxWorktreeConfigResponseDto,
        SessionLookupApplicationError,
    > {
        sandbox_worktree_config::get(self.clone(), session.into_inner())
    }

    fn session_sandbox_worktree_config_create(
        &self,
        _principal: SealedPrincipal,
        session: WireId<SessionId>,
        request: grapt_api::u3a::SandboxWorktreeConfigRequestDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::SandboxWorktreeConfigResponseDto,
        SessionLookupApplicationError,
    > {
        sandbox_worktree_config::create(self.clone(), session.into_inner(), request)
    }

    fn session_delete(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        ready(context::delete_session(self, session_id.into_inner()))
    }

    fn side_question(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SideQuestionRequestDto,
    ) -> ApplicationFuture<grapt_api::u3a::SideQuestionResponseDto, SessionLookupApplicationError>
    {
        self.answer_side_question(session_id.into_inner(), request)
    }

    fn session_fork(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionForkRequestDto,
    ) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
        ready(self.fork_session_request(session_id.into_inner(), request))
    }

    fn session_messages(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        query: MessageListQueryDto,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
        self.session_messages_view(
            principal,
            session_id,
            query,
            grapt_api::u3a::MessageListView::Durable,
        )
    }

    fn session_messages_view(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        query: MessageListQueryDto,
        view: grapt_api::u3a::MessageListView,
    ) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
        self.read_projection(
            move |application| {
                history::messages_get(application, &session_id.into_inner(), query, view)
            },
            |_| SessionMessagesApplicationError::Unavailable,
        )
    }

    fn session_message(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        message_id: WireId<MessageId>,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionMessageResponseDto,
        grapt_api::u3a::SessionMessageApplicationError,
    > {
        self.read_projection(
            move |application| {
                history::message_get(
                    application,
                    &session_id.into_inner(),
                    &message_id.into_inner(),
                )
            },
            |_| grapt_api::u3a::SessionMessageApplicationError::Unavailable,
        )
    }

    fn session_prompt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PromptRequestDto,
    ) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError> {
        self.submit_prompt(principal, session_id, request)
    }

    fn session_interrupt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: InterruptRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        self.interrupt_with_pending(principal, session_id, request)
    }

    fn session_compact(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::CompactRequestDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::CompactResponseDto,
        grapt_api::u3a::SessionCompactApplicationError,
    > {
        ready(self.submit_compact(session_id, request))
    }

    fn permission_list(
        &self,
        principal: SealedPrincipal,
        query: PermissionLocationQueryDto,
    ) -> ApplicationFuture<PermissionGlobalListResponseDto, PermissionApplicationError> {
        permission::list(self, principal, query)
    }

    fn session_permission_create(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PermissionCreateRequestDto,
    ) -> ApplicationFuture<PermissionCreateResponseDto, PermissionApplicationError> {
        permission::create(self, principal, session_id, request)
    }

    fn session_permission_list(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<PermissionListResponseDto, PermissionApplicationError> {
        permission::session_list(self, principal, session_id)
    }

    fn session_permission_get(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request_id: WireId<PermissionId>,
    ) -> ApplicationFuture<PermissionResponseDto, PermissionApplicationError> {
        permission::get(self, principal, session_id, request_id)
    }

    fn session_permission_reply(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request_id: WireId<PermissionId>,
        request: PermissionReplyRequestDto,
    ) -> ApplicationFuture<NoContent, PermissionApplicationError> {
        permission::reply(self, principal, session_id, request_id, request)
    }
}

#[cfg(test)]
mod basic_tests;

#[cfg(test)]
mod committed_projection_tests;

#[cfg(test)]
mod session_metrics_tests;

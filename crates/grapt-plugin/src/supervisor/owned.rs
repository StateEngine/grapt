//! Owned plugin processes, their call grants, and generation health.

use std::sync::atomic::AtomicU64;
use std::sync::{Condvar, OnceLock, Weak};

use super::process::{settle_stream_end_classification, StreamEnd, ViolationKind};
use super::transport::{Channel, HostQueues};
use super::*;
use crate::protocol::HostRequest;

/// The exit-status observation bound at stream end: how long the reader
/// waits for the launcher's exit to become visible before naming the
/// still-running outcome.
const EXIT_STATUS_OBSERVATION_MILLIS: u64 = 250;

/// Product-defined authority carried by a live call grant.
pub type GrantScope = Arc<dyn std::any::Any + Send + Sync>;

/// A grant proven live on the process that sent a host request.
#[derive(Clone)]
pub struct ActiveGrant {
    id: u64,
    process: u64,
    scope: GrantScope,
}

impl ActiveGrant {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn scope(&self) -> &GrantScope {
        &self.scope
    }
}

impl core::fmt::Debug for ActiveGrant {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ActiveGrant")
            .field("id", &self.id)
            .field("process", &self.process)
            .finish_non_exhaustive()
    }
}

/// How an invocation may use host authority.
#[derive(Clone)]
pub enum CallAuthority {
    /// Registration-time work such as transforms; host requests carry no grant.
    Unscoped,
    /// A new top-level call. The process admits one at a time.
    Scoped { scope: GrantScope, call: String },
    /// Host work already running under `parent` invokes another callback.
    Inherit { parent: ActiveGrant, call: String },
}

impl core::fmt::Debug for CallAuthority {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unscoped => formatter.write_str("Unscoped"),
            Self::Scoped { call, .. } => formatter
                .debug_struct("Scoped")
                .field("call", call)
                .finish_non_exhaustive(),
            Self::Inherit { parent, call } => formatter
                .debug_struct("Inherit")
                .field("parent", &parent.id)
                .field("call", call)
                .finish(),
        }
    }
}

struct LiveGrant {
    scope: GrantScope,
    call: String,
    top: bool,
}

#[derive(Default)]
struct SlotState {
    top: Option<u64>,
    live: BTreeMap<u64, LiveGrant>,
}

pub(super) struct CallSlots {
    state: Mutex<SlotState>,
    released: Condvar,
}

pub(super) struct GrantGuard<'a> {
    slots: &'a CallSlots,
    id: Option<u64>,
}

impl Drop for GrantGuard<'_> {
    fn drop(&mut self) {
        let Some(id) = self.id else {
            return;
        };
        if let Ok(mut state) = self.slots.state.lock() {
            if let Some(grant) = state.live.remove(&id) {
                if grant.top {
                    state.top = None;
                }
            }
        }
        self.slots.released.notify_all();
    }
}

impl CallSlots {
    fn new() -> Self {
        Self {
            state: Mutex::new(SlotState::default()),
            released: Condvar::new(),
        }
    }

    fn admit<'a>(
        &'a self,
        grants: &AtomicU64,
        authority: &CallAuthority,
        process: u64,
        deadline: tokio::time::Instant,
    ) -> Result<(GrantGuard<'a>, Option<u64>), PluginError> {
        let next_id = || {
            grants
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| PluginError::Protocol("call grant identity overflowed".to_owned()))
        };
        match authority {
            CallAuthority::Unscoped => Ok((
                GrantGuard {
                    slots: self,
                    id: None,
                },
                None,
            )),
            CallAuthority::Inherit { parent, call } if parent.process == process => {
                let mut state = self.state.lock().map_err(|_| PluginError::Poisoned)?;
                if !state.live.contains_key(&parent.id) {
                    return Err(PluginError::Invocation(
                        "the parent plugin call grant is no longer live".to_owned(),
                    ));
                }
                let id = next_id()?;
                state.live.insert(
                    id,
                    LiveGrant {
                        scope: Arc::clone(&parent.scope),
                        call: call.clone(),
                        top: false,
                    },
                );
                Ok((
                    GrantGuard {
                        slots: self,
                        id: Some(id),
                    },
                    Some(id),
                ))
            }
            CallAuthority::Scoped { scope, call }
            | CallAuthority::Inherit {
                parent: ActiveGrant { scope, .. },
                call,
            } => {
                let mut state = self.state.lock().map_err(|_| PluginError::Poisoned)?;
                while state.top.is_some() {
                    let remaining = deadline
                        .checked_duration_since(tokio::time::Instant::now())
                        .ok_or(PluginError::Timeout("plugin call admission"))?;
                    state = self
                        .released
                        .wait_timeout(state, remaining)
                        .map_err(|_| PluginError::Poisoned)?
                        .0;
                }
                let id = next_id()?;
                state.top = Some(id);
                state.live.insert(
                    id,
                    LiveGrant {
                        scope: Arc::clone(scope),
                        call: call.clone(),
                        top: true,
                    },
                );
                Ok((
                    GrantGuard {
                        slots: self,
                        id: Some(id),
                    },
                    Some(id),
                ))
            }
        }
    }

    fn live(&self, id: u64, process: u64) -> Option<ActiveGrant> {
        let state = self.state.lock().ok()?;
        state.live.get(&id).map(|grant| ActiveGrant {
            id,
            process,
            scope: Arc::clone(&grant.scope),
        })
    }

    fn call_live(&self, call: &str) -> bool {
        self.state
            .lock()
            .map(|state| state.live.values().any(|grant| grant.call == call))
            .unwrap_or(false)
    }

    fn wait_call_released(&self, call: &str, deadline: tokio::time::Instant) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        while state.live.values().any(|grant| grant.call == call) {
            let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
            else {
                return false;
            };
            state = match self.released.wait_timeout(state, remaining) {
                Ok((state, _)) => state,
                Err(_) => return false,
            };
        }
        true
    }
}

/// Health shared by every process of one generation.
pub(super) struct GenerationHealth {
    generation: PluginGenerationId,
    reason: Mutex<Option<String>>,
    members: Mutex<Vec<Weak<PluginProcess>>>,
    host: Arc<dyn PluginHost>,
}

impl GenerationHealth {
    pub(super) fn new(generation: PluginGenerationId, host: Arc<dyn PluginHost>) -> Arc<Self> {
        Arc::new(Self {
            generation,
            reason: Mutex::new(None),
            members: Mutex::new(Vec::new()),
            host,
        })
    }

    pub(super) fn violation(&self) -> Option<String> {
        self.reason.lock().ok().and_then(|reason| reason.clone())
    }

    /// Invalidate the whole generation once: settle every pending exchange with
    /// the named reason and terminate every owned process.
    pub(super) fn invalidate(&self, reason: String) {
        {
            let Ok(mut held) = self.reason.lock() else {
                return;
            };
            if held.is_some() {
                return;
            }
            *held = Some(reason.clone());
        }
        let members = self
            .members
            .lock()
            .map(|members| members.iter().filter_map(Weak::upgrade).collect::<Vec<_>>())
            .unwrap_or_default();
        for member in &members {
            member.access.live.store(false, Ordering::Release);
            member.channel.close(&reason);
        }
        for member in &members {
            if member
                .control
                .terminate_for(ViolationKind::Other, reason.clone())
                .is_ok()
            {
                member.release_budget();
            }
        }
        self.host.generation_closed(self.generation);
    }
}

pub(super) struct PluginProcess {
    instance: u64,
    generation: PluginGenerationId,
    /// The guest id and the admitted declaration identity, bound at load.
    plugin: OnceLock<(Arc<str>, Arc<str>)>,
    channel: Channel,
    pub(super) control: ProcessControl,
    calls: CallSlots,
    health: Arc<GenerationHealth>,
    host: Arc<dyn PluginHost>,
    access: super::isolation::ProcessAccess,
    queues: OnceLock<HostQueues>,
    limits: BridgeLimits,
    /// Held until the process is confirmed reaped, not until the last
    /// reference drops, so a successor is admitted as soon as custody ends.
    budget: Mutex<Option<ProcessBudgetSlot>>,
}

impl PluginProcess {
    pub(super) fn start(
        runtime: &VerifiedRuntime,
        containment: &AggregateContainment,
        limits: BridgeLimits,
        instance: u64,
        health: &Arc<GenerationHealth>,
        budget: ProcessBudgetSlot,
        declaration: &PluginDeclaration,
    ) -> Result<Arc<Self>, PluginError> {
        let generation = health.generation;
        runtime.verify_before_spawn()?;
        let access = super::isolation::ProcessAccess::prepare(runtime, declaration)?;
        let spawned =
            match spawn_bridge(runtime, containment, limits, generation, instance, &access) {
                Ok(spawned) => spawned,
                Err(error) => {
                    containment.settle_failed_start(instance, budget);
                    return Err(error);
                }
            };
        let process = Arc::new(Self {
            instance,
            generation,
            plugin: OnceLock::new(),
            channel: Channel::new(spawned.input, generation, limits),
            control: spawned.control,
            calls: CallSlots::new(),
            health: Arc::clone(health),
            host: Arc::clone(&health.host),
            access,
            queues: OnceLock::new(),
            limits,
            budget: Mutex::new(Some(budget)),
        });
        if let Ok(mut members) = health.members.lock() {
            members.push(Arc::downgrade(&process));
        }
        let queues = HostQueues::start(&process, limits.pending_responses.clamp(1, 8))?;
        let _ = process.queues.set(queues);
        let reader = Arc::clone(&process);
        let output = spawned.output;
        thread::Builder::new()
            .name(format!("plugin-bridge-{}-{instance}", generation.value()))
            .spawn(move || super::transport::read_bridge_output(output, reader))
            .map_err(|error| PluginError::Spawn(error.to_string()))?;
        Ok(process)
    }

    fn release_budget(&self) {
        if let Ok(mut slot) = self.budget.lock() {
            slot.take();
        }
    }

    pub(super) fn generation(&self) -> PluginGenerationId {
        self.generation
    }

    pub(super) fn bind_plugin(&self, plugin: &str, owner: &str) -> Result<(), PluginError> {
        self.plugin
            .set((Arc::from(plugin), Arc::from(owner)))
            .map_err(|_| {
                PluginError::Registration("plugin process identity was already bound".to_owned())
            })
    }

    pub(super) fn channel(&self) -> &Channel {
        &self.channel
    }

    pub(super) fn frame_bytes(&self) -> usize {
        self.limits.frame_bytes
    }

    pub(super) fn host_queues(&self) -> Option<&HostQueues> {
        self.queues.get()
    }

    /// Exchange one request; a deadline invalidates the whole generation.
    pub(super) fn exchange(
        &self,
        operation: &'static str,
        payload: Value,
        timeout: Duration,
        stage: &'static str,
    ) -> Result<Value, PluginError> {
        if let Some(reason) = self.health.violation() {
            return Err(PluginError::Protocol(reason));
        }
        match self.channel.exchange(operation, payload, timeout, stage) {
            Err(PluginError::Timeout(stage)) => {
                self.health.invalidate(format!(
                    "plugin bridge timed out during {stage}; its generation was invalidated and terminated"
                ));
                Err(PluginError::Timeout(stage))
            }
            Err(PluginError::Protocol(reason)) => {
                let reason = self.control.violation().unwrap_or(reason);
                self.health.invalidate(reason.clone());
                Err(PluginError::Protocol(reason))
            }
            other => other,
        }
    }

    pub(super) fn invoke(
        &self,
        grants: &AtomicU64,
        operation: &'static str,
        mut payload: Value,
        authority: &CallAuthority,
        timeout: Duration,
        stage: &'static str,
    ) -> Result<Value, PluginError> {
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or(PluginError::Timeout(stage))?;
        let (_grant, id) = self
            .calls
            .admit(grants, authority, self.instance, deadline)?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("authority".to_owned(), json!(id));
            let call = match authority {
                CallAuthority::Unscoped => Value::Null,
                CallAuthority::Scoped { call, .. } | CallAuthority::Inherit { call, .. } => {
                    json!(call)
                }
            };
            object.insert("call".to_owned(), call);
        }
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .ok_or(PluginError::Timeout("plugin call admission"))?;
        self.exchange(operation, payload, remaining, stage)
    }

    /// Ask the bridge to abort one call and wait for its grant to settle.
    pub(super) fn cancel_call(&self, call: &str) -> Result<bool, PluginError> {
        if !self.calls.call_live(call) {
            return Ok(false);
        }
        let cleanup = Duration::from_millis(self.limits.cleanup_millis);
        let deadline = tokio::time::Instant::now()
            .checked_add(cleanup)
            .ok_or(PluginError::Timeout("plugin call cancellation"))?;
        self.exchange(
            "cancel-call",
            json!({"call": call}),
            cleanup,
            "plugin call cancellation",
        )?;
        if self.calls.wait_call_released(call, deadline) {
            return Ok(true);
        }
        self.health.invalidate(format!(
            "plugin call {call} did not settle within its {}ms cancellation bound; its generation was invalidated and terminated",
            self.limits.cleanup_millis
        ));
        Err(PluginError::Timeout("plugin call cancellation"))
    }

    pub(super) fn serve_host_request(&self, request: HostRequest) {
        let outcome = if request.operation == HostOperation::NetworkFetch {
            // Top-level module evaluation may fetch before the exported plugin
            // identity is bound. Authority is already fixed by this process's
            // admitted declaration, never by a guest-supplied identity.
            if self.is_running() {
                super::network::fetch(
                    &self.access.permissions,
                    || {
                        self.access.live.load(Ordering::Acquire)
                            && request
                                .authority
                                .is_none_or(|id| self.calls.live(id, self.instance).is_some())
                    },
                    request.payload,
                    self.limits.frame_bytes,
                )
            } else {
                Err("plugin_network_closed: this plugin generation is no longer live".into())
            }
        } else {
            self.host_context(request.authority)
                .and_then(|context| self.host.call(&context, request.operation, request.payload))
        };
        if let Err(error) = self
            .channel
            .write_host_response(request.correlation, outcome)
        {
            self.corrupt(format!(
                "plugin host response could not be delivered: {error}"
            ));
        }
    }

    fn host_context(&self, authority: Option<u64>) -> Result<PluginHostContext, String> {
        let (plugin, owner) =
            self.plugin.get().cloned().ok_or_else(|| {
                "plugin host requests require a loaded plugin identity".to_owned()
            })?;
        let grant = match authority {
            None => None,
            Some(id) => Some(self.calls.live(id, self.instance).ok_or_else(|| {
                "plugin call authority is not live on this plugin process".to_owned()
            })?),
        };
        Ok(PluginHostContext {
            generation: self.generation,
            process: self.instance,
            plugin,
            owner,
            grant,
        })
    }

    pub(super) fn corrupt(&self, reason: String) {
        self.health.invalidate(reason);
    }

    /// Settle every pending exchange when the bridge output stream ends.
    /// The launcher's exit is observed within the stated bound before the
    /// outcome is named; a stream error is classified against that
    /// observed state, never before it.
    pub(super) fn settle_stream_end(&self, stream_error: Option<String>) {
        self.access.live.store(false, Ordering::Release);
        let recorded = self.control.recorded_violation();
        let outcome = settle_stream_end_classification(
            recorded.as_ref(),
            &mut || self.control.has_exited().unwrap_or(true),
            Duration::from_millis(EXIT_STATUS_OBSERVATION_MILLIS),
            stream_error,
        );
        match outcome {
            StreamEnd::Exited(reason) | StreamEnd::StillRunning(reason) => {
                self.channel.close(&reason)
            }
            StreamEnd::Corrupt(reason) => self.corrupt(reason),
        }
    }

    pub(super) fn is_running(&self) -> bool {
        self.health.violation().is_none()
            && self.control.violation().is_none()
            && self.control.has_exited().is_ok_and(|exited| !exited)
    }

    pub(super) fn shutdown(&self) -> Result<(), PluginError> {
        self.access.live.store(false, Ordering::Release);
        let cleanup = if self.health.violation().is_none() && self.channel.closed_reason().is_none()
        {
            self.channel.exchange(
                "cleanup",
                Value::Null,
                Duration::from_millis(self.limits.cleanup_millis),
                "cleanup",
            )
        } else {
            Ok(Value::Null)
        };
        self.channel.close("plugin generation was retired");
        let terminated = self.control.terminate();
        if terminated.is_ok() {
            self.release_budget();
        }
        match (cleanup, terminated) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(_), Ok(())) => Ok(()),
        }
    }
}

/// Every process of one generation, keyed by the identity its transport proved.
pub(super) struct OwnedGeneration {
    pub(super) generation: Arc<PluginGeneration>,
    pub(super) processes: BTreeMap<String, Arc<PluginProcess>>,
    pub(super) health: Arc<GenerationHealth>,
    pub(super) limits: BridgeLimits,
    retired: Mutex<bool>,
}

impl OwnedGeneration {
    pub(super) fn new(
        generation: Arc<PluginGeneration>,
        processes: BTreeMap<String, Arc<PluginProcess>>,
        health: Arc<GenerationHealth>,
        limits: BridgeLimits,
    ) -> Self {
        Self {
            generation,
            processes,
            health,
            limits,
            retired: Mutex::new(false),
        }
    }

    pub(super) fn process(&self, plugin: &str) -> Result<&Arc<PluginProcess>, PluginError> {
        if let Some(reason) = self.health.violation() {
            return Err(PluginError::Protocol(reason));
        }
        self.processes.get(plugin).ok_or_else(|| {
            PluginError::Invocation(format!("plugin {plugin} is not part of this generation"))
        })
    }

    pub(super) fn is_running(&self) -> bool {
        self.health.violation().is_none()
            && self.processes.values().all(|process| process.is_running())
    }

    pub(super) fn retire(&self) -> Result<(), PluginError> {
        {
            let Ok(mut retired) = self.retired.lock() else {
                return Err(PluginError::Poisoned);
            };
            if *retired {
                return Ok(());
            }
            *retired = true;
        }
        let mut failure = None;
        for process in self.processes.values().rev() {
            if let Err(error) = process.shutdown() {
                failure.get_or_insert(error);
            }
        }
        self.health.host.generation_closed(self.generation.id());
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for OwnedGeneration {
    fn drop(&mut self) {
        let _ = self.retire();
    }
}

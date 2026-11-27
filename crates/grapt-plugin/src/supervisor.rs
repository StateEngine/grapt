mod budget;
mod isolation;
mod network;
mod owned;
mod process;
mod runtime;
mod transport;
mod validation;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::{Child, ChildStdin};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(windows)]
use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::generation::{PluginGeneration, PluginGenerationId, PluginRegistration};
use crate::protocol::{
    BridgeFrame, BridgeLimits, HostFrameKind, HostOperation, HostResponse, HostResponsePayload,
    Request, SupervisorBudget, DEFAULT_SUPERVISOR_BUDGET, PROTOCOL_VERSION,
};
use crate::source::SourceSnapshot;
use crate::PluginDeclaration;

use budget::{AggregateContainment, ProcessBudget, ProcessBudgetSlot};
pub use owned::{ActiveGrant, CallAuthority, GrantScope};
use owned::{GenerationHealth, OwnedGeneration, PluginProcess};
#[cfg(unix)]
#[doc(hidden)]
pub use process::journal::RecordedGroup;
use process::{spawn_bridge, BridgeInput, ProcessControl};
use runtime::{validate_pin, VerifiedRuntime};
use validation::child_path;

const PIN_BATCH: usize = 256;

type StartedProcesses = (
    BTreeMap<String, Arc<PluginProcess>>,
    Vec<PluginRegistration>,
    Vec<(usize, PluginError)>,
);

/// A declared plugin left out of a generation because it failed to start or
/// register. Its siblings still run; the caller shows the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPlugin {
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct BridgeRuntime {
    pub executable: PathBuf,
    pub bridge: PathBuf,
    pub expected: RuntimePin,
    /// Service-owned directory that receives the verified private runtime copy.
    pub private_root: PathBuf,
    pub recovery: RecoveryJournal,
}

/// Where a supervisor journals its bridge launches, and the injected clock
/// and identity sources it stamps them with, so a successor can reclaim the
/// launches of one that crashed (SC-MAC-02b). Only macOS needs it: Linux and
/// Windows contain bridges in the kernel. `root` must be stable across
/// runtime pins and outside `private_root`, which runtime cleanup removes.
#[derive(Debug, Clone)]
pub struct RecoveryJournal {
    pub root: PathBuf,
    pub clock: Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
    pub ids: Arc<dyn grapt_types::IdAuthority + Send + Sync>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePin {
    pub version: String,
    pub executable_sha256: String,
    pub bridge_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeIdentity {
    pub executable: PathBuf,
    pub version: String,
    pub sha256: String,
}

pub use crate::error::PluginError;

/// Authority the supervisor proved for one host request: the generation and
/// process that sent it, the plugin identity that process loaded, and the
/// call grant that was live on that process, if any.
#[derive(Debug, Clone)]
pub struct PluginHostContext {
    generation: PluginGenerationId,
    process: u64,
    plugin: Arc<str>,
    owner: Arc<str>,
    grant: Option<ActiveGrant>,
}

impl PluginHostContext {
    pub fn generation(&self) -> PluginGenerationId {
        self.generation
    }

    pub fn process(&self) -> u64 {
        self.process
    }

    /// The guest-exported plugin id, used for routing and registrations.
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// The host-admitted declaration identity that owns durable plugin state.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn grant(&self) -> Option<&ActiveGrant> {
        self.grant.as_ref()
    }
}

pub trait PluginHost: Send + Sync + 'static {
    fn call(
        &self,
        context: &PluginHostContext,
        operation: HostOperation,
        payload: Value,
    ) -> Result<Value, String>;

    fn generation_closed(&self, _generation: PluginGenerationId) {}
}

#[derive(Debug)]
struct RefusingPluginHost;

impl PluginHost for RefusingPluginHost {
    fn call(
        &self,
        _context: &PluginHostContext,
        operation: HostOperation,
        _payload: Value,
    ) -> Result<Value, String> {
        Err(format!(
            "plugin host operation {operation:?} is unavailable"
        ))
    }
}

pub struct PluginSupervisor {
    runtime: VerifiedRuntime,
    limits: BridgeLimits,
    budget: ProcessBudget,
    containment: AggregateContainment,
    host: Arc<dyn PluginHost>,
    current: RwLock<Option<Arc<OwnedGeneration>>>,
    preparation: Mutex<()>,
    next_generation: AtomicU64,
    next_process: AtomicU64,
    grants: Arc<AtomicU64>,
}

impl core::fmt::Debug for PluginSupervisor {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PluginSupervisor")
            .field("runtime", &self.runtime)
            .field("limits", &self.limits)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

/// A fully started generation that is not yet visible to callers.
pub struct CandidateGeneration {
    owned: Arc<OwnedGeneration>,
    grants: Arc<AtomicU64>,
    skipped: Vec<SkippedPlugin>,
}

impl core::fmt::Debug for CandidateGeneration {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CandidateGeneration")
            .field("generation", &self.owned.generation.id())
            .finish_non_exhaustive()
    }
}

impl CandidateGeneration {
    pub fn generation(&self) -> &Arc<PluginGeneration> {
        &self.owned.generation
    }

    /// The declared plugins this generation runs without.
    pub fn skipped(&self) -> &[SkippedPlugin] {
        &self.skipped
    }

    /// Run a registration-time callback such as a transform before publication.
    pub fn invoke_callback(
        &self,
        plugin: &str,
        callback: u64,
        value: Value,
    ) -> Result<Value, PluginError> {
        self.owned.process(plugin)?.invoke(
            &self.grants,
            "invoke-callback",
            json!({"callback": callback, "value": value}),
            &CallAuthority::Unscoped,
            Duration::from_millis(self.owned.limits.call_millis),
            "plugin callback",
        )
    }

    /// Retire the candidate without publishing it.
    pub fn discard(self) -> Result<(), PluginError> {
        self.owned.retire()
    }
}

/// How a supervisor contains its bridge processes (SC-MAC-02). Only the
/// kernel kinds bound a process tree whatever it does; the watchdog samples it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentKind {
    /// Linux: a systemd unit in a cgroup slice bounds memory, CPU and tasks.
    KernelCgroup,
    /// Windows: Job Objects bound memory, processes and CPU.
    KernelJob,
    /// macOS: a process group per bridge, bounded by a sampling watchdog
    /// that kills the tree. Not kernel-enforced. Groups are killed only while
    /// an unreaped sentinel of ours pins the group ID; a single process
    /// outside the group only under its re-read identity (start time, parent).
    /// Residual risk: start times are whole seconds and macOS has no process
    /// handle to signal through, so a PID reused within the same second, by a
    /// child of the same recorded parent, between that re-read and the signal,
    /// would be signalled.
    Watchdog,
}

impl ContainmentKind {
    #[cfg(target_os = "linux")]
    const PLATFORM: Self = Self::KernelCgroup;
    #[cfg(windows)]
    const PLATFORM: Self = Self::KernelJob;
    #[cfg(target_os = "macos")]
    const PLATFORM: Self = Self::Watchdog;
    // No supervisor is constructed here: containment refuses first.
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    const PLATFORM: Self = Self::Watchdog;

    /// What this containment does and does not bound, in plain words.
    pub fn description(self) -> &'static str {
        match self {
            Self::KernelCgroup => "kernel-enforced: a systemd cgroup bounds the bridge's memory, CPU and tasks",
            Self::KernelJob => "kernel-enforced: Windows Job Objects bound the bridge's memory, processes and CPU",
            Self::Watchdog => "macOS containment is watchdog-enforced, not kernel-enforced: memory is sampled resident memory that the watchdog kills, and a process that leaves the bridge's process group and parent chain is not contained",
        }
    }
}

impl PluginSupervisor {
    pub fn new(runtime: BridgeRuntime, limits: BridgeLimits) -> Result<Self, PluginError> {
        Self::with_host(runtime, limits, Arc::new(RefusingPluginHost))
    }

    pub fn with_host(
        runtime: BridgeRuntime,
        limits: BridgeLimits,
        host: Arc<dyn PluginHost>,
    ) -> Result<Self, PluginError> {
        Self::with_budget(runtime, limits, DEFAULT_SUPERVISOR_BUDGET, host)
    }

    pub fn with_budget(
        runtime: BridgeRuntime,
        limits: BridgeLimits,
        budget: SupervisorBudget,
        host: Arc<dyn PluginHost>,
    ) -> Result<Self, PluginError> {
        validate_pin(&runtime.expected)?;
        let processes = ProcessBudget::new(budget)?;
        // A supervisor that died without retiring its containment left it
        // here; reclaim it before establishing this one's.
        #[cfg(target_os = "linux")]
        process::linux::reclaim_orphaned_containment();
        let containment = AggregateContainment::establish(
            budget,
            &runtime.recovery,
            Duration::from_millis(limits.reap_millis),
        )?;
        let runtime = VerifiedRuntime::admit(&runtime)?;
        Ok(Self {
            runtime,
            limits,
            budget: processes,
            containment,
            host,
            current: RwLock::new(None),
            preparation: Mutex::new(()),
            next_generation: AtomicU64::new(1),
            next_process: AtomicU64::new(1),
            grants: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn runtime_identity(&self) -> &RuntimeIdentity {
        self.runtime.identity()
    }

    /// How this platform contains the supervisor's bridge processes.
    pub fn containment(&self) -> ContainmentKind {
        ContainmentKind::PLATFORM
    }

    pub fn live_processes(&self) -> usize {
        self.budget.live()
    }

    /// Start one process per declaration and collect its registration. The
    /// published generation is untouched whether this succeeds or fails.
    pub fn prepare(
        &self,
        declarations: Vec<PluginDeclaration>,
        app: Value,
    ) -> Result<CandidateGeneration, PluginError> {
        let _serial = self.preparation.lock().map_err(|_| PluginError::Poisoned)?;
        if declarations.is_empty() {
            return Err(PluginError::Registration(
                "activation requires at least one declaration".to_owned(),
            ));
        }
        let admitted = self.budget.reload_safe_plugins();
        if declarations.len() > admitted {
            return Err(PluginError::Registration(format!(
                "{} enabled plugins exceed the reload-safe bound of {admitted}: a reload starts every plugin process beside the published generation, so at most half of the {}-process supervisor budget may be enabled; disable plugins until at most {admitted} remain",
                declarations.len(),
                self.budget.limit()
            )));
        }
        let snapshots = declarations
            .iter()
            .map(|declaration| {
                let snapshot = declaration.executable.snapshot().ok_or_else(|| {
                    PluginError::Runtime(
                        "package acquisition requires an installed exact pin and is not performed by the bridge"
                            .to_owned(),
                    )
                })?;
                snapshot
                    .verify()
                    .map_err(|error| PluginError::Runtime(error.to_string()))?;
                Ok(snapshot)
            })
            .collect::<Result<Vec<_>, PluginError>>()?;
        let slots = self.budget.reserve(declarations.len())?;
        let generation = next_id(&self.next_generation, "generation")
            .and_then(|id| PluginGenerationId::new(id).ok_or(PluginError::StaleGeneration))?;
        let health = GenerationHealth::new(generation, Arc::clone(&self.host));
        let started = self.start_processes(&health, &declarations, &snapshots, slots, &app);
        let (processes, registrations, failed) = match started {
            Ok(started) => started,
            Err(error) => {
                health.invalidate(format!(
                    "plugin candidate generation failed to start: {error}"
                ));
                return Err(error);
            }
        };
        let mut skipped = Vec::with_capacity(failed.len());
        let mut kept = Vec::with_capacity(registrations.len());
        let mut failed = failed.into_iter().peekable();
        for (index, declaration) in declarations.into_iter().enumerate() {
            match failed.next_if(|(at, _)| *at == index) {
                Some((_, error)) => skipped.push(SkippedPlugin {
                    target: declaration.target,
                    reason: error.to_string(),
                }),
                None => kept.push(declaration),
            }
        }
        let validated = PluginGeneration::validate(
            generation,
            kept,
            registrations,
            self.limits.registrations,
            self.limits.schema_bytes,
        );
        let owned = Arc::new(OwnedGeneration::new(
            Arc::new(match validated {
                Ok(validated) => validated,
                Err(error) => {
                    for process in processes.values() {
                        let _ = process.shutdown();
                    }
                    self.host.generation_closed(generation);
                    return Err(error);
                }
            }),
            processes,
            health,
            self.limits,
        ));
        Ok(CandidateGeneration {
            owned,
            grants: Arc::clone(&self.grants),
            skipped,
        })
    }

    fn start_processes(
        &self,
        health: &Arc<GenerationHealth>,
        declarations: &[PluginDeclaration],
        snapshots: &[&SourceSnapshot],
        slots: Vec<ProcessBudgetSlot>,
        app: &Value,
    ) -> Result<StartedProcesses, PluginError> {
        let startup = Duration::from_millis(self.limits.startup_millis);
        let mut results = Vec::with_capacity(declarations.len());
        thread::scope(|scope| {
            let handles = declarations
                .iter()
                .zip(snapshots)
                .zip(slots)
                .map(|((declaration, snapshot), slot)| {
                    scope.spawn(move || {
                        let deadline = tokio::time::Instant::now()
                            .checked_add(startup)
                            .ok_or(PluginError::Timeout("plugin startup admission"))?;
                        let _permit = self.budget.startup_permit(deadline)?;
                        let instance = next_id(&self.next_process, "process")?;
                        let process = PluginProcess::start(
                            &self.runtime,
                            &self.containment,
                            self.limits,
                            instance,
                            health,
                            slot,
                            declaration,
                        )?;
                        match load_plugin(&process, declaration, snapshot, app, self.limits) {
                            Ok(registration) => Ok((process, registration)),
                            Err(error) => {
                                let _ = process.shutdown();
                                Err(error)
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                results.push(handle.join().unwrap_or_else(|_| {
                    Err(PluginError::Spawn(
                        "plugin startup thread panicked".to_owned(),
                    ))
                }));
            }
        });
        let mut started = Vec::with_capacity(results.len());
        let mut processes = BTreeMap::new();
        let mut registrations = Vec::with_capacity(results.len());
        let mut failure = None;
        let mut failed = Vec::new();
        for (index, result) in results.into_iter().enumerate() {
            match result {
                Ok((process, registration)) => {
                    started.push(Arc::clone(&process));
                    if processes
                        .insert(registration.plugin_id.clone(), process)
                        .is_some()
                    {
                        failure.get_or_insert(PluginError::Registration(format!(
                            "plugin id {} is declared by more than one admitted plugin",
                            registration.plugin_id
                        )));
                    }
                    registrations.push(registration);
                }
                // One plugin that cannot start costs only itself.
                Err(error) => failed.push((index, error)),
            }
        }
        // With no plugin left there is no generation to run without them.
        let failure = failure.or_else(|| {
            registrations
                .is_empty()
                .then(|| failed.drain(..).next().map(|(_, error)| error))
                .flatten()
        });
        if let Some(error) = failure {
            for process in started {
                let _ = process.shutdown();
            }
            return Err(error);
        }
        Ok((processes, registrations, failed))
    }

    /// Make a started candidate the current generation and retire its predecessor.
    pub fn publish(
        &self,
        candidate: CandidateGeneration,
    ) -> Result<Arc<PluginGeneration>, PluginError> {
        let owned = Arc::clone(&candidate.owned);
        if !owned.is_running() {
            // R3 (consolidated review HOLD, 2026-09-26): the generation
            // reason settles only when invalidate runs; a member
            // terminated by its own watchdog carries the reason on its
            // control until then, so publish reports that member's
            // violation (recorded, else containment) before the generic
            // exit literal - the C13 pre-publication exit was misnamed
            // by that fallback.
            let reason = owned
                .health
                .violation()
                .or_else(|| {
                    owned
                        .processes
                        .values()
                        .find_map(|process| {
                            (!process.is_running()).then(|| process.control.violation())
                        })
                        .flatten()
                })
                .unwrap_or_else(|| {
                    "a candidate plugin process exited before publication".to_owned()
                });
            let _ = candidate.discard();
            return Err(PluginError::Protocol(reason));
        }
        let previous = {
            let mut current = self.current.write().map_err(|_| PluginError::Poisoned)?;
            current.replace(Arc::clone(&owned))
        };
        if let Some(previous) = previous {
            let _ = previous.retire();
        }
        Ok(Arc::clone(&owned.generation))
    }

    pub fn activate(
        &self,
        declarations: Vec<PluginDeclaration>,
        app: Value,
    ) -> Result<Arc<PluginGeneration>, PluginError> {
        let candidate = self.prepare(declarations, app)?;
        self.publish(candidate)
    }

    pub fn current(&self) -> Result<Option<Arc<PluginGeneration>>, PluginError> {
        let current = self.current.read().map_err(|_| PluginError::Poisoned)?;
        Ok(current.as_ref().map(|owned| Arc::clone(&owned.generation)))
    }

    pub fn current_is_running(&self, expected: PluginGenerationId) -> Result<bool, PluginError> {
        let current = self.current.read().map_err(|_| PluginError::Poisoned)?;
        Ok(current
            .as_ref()
            .is_some_and(|owned| owned.generation.id() == expected && owned.is_running()))
    }

    fn owned(&self, generation: PluginGenerationId) -> Result<Arc<OwnedGeneration>, PluginError> {
        let held = self.current.read().map_err(|_| PluginError::Poisoned)?;
        held.as_ref()
            .filter(|owned| owned.generation.id() == generation)
            .map(Arc::clone)
            .ok_or(PluginError::StaleGeneration)
    }

    pub fn invoke(
        &self,
        generation: PluginGenerationId,
        plugin: &str,
        callback: u64,
        input: Value,
        context: Value,
        authority: CallAuthority,
    ) -> Result<Value, PluginError> {
        let owned = self.owned(generation)?;
        owned.process(plugin)?.invoke(
            &self.grants,
            "invoke-tool",
            json!({"callback": callback, "input": input, "context": context}),
            &authority,
            Duration::from_millis(self.limits.call_millis),
            "tool invocation",
        )
    }

    pub fn invoke_callback(
        &self,
        generation: PluginGenerationId,
        plugin: &str,
        callback: u64,
        value: Value,
        authority: CallAuthority,
    ) -> Result<Value, PluginError> {
        let owned = self.owned(generation)?;
        owned.process(plugin)?.invoke(
            &self.grants,
            "invoke-callback",
            json!({"callback": callback, "value": value}),
            &authority,
            Duration::from_millis(self.limits.call_millis),
            "plugin callback",
        )
    }

    /// Cancel one call on one plugin without disturbing other calls.
    pub fn cancel_call(
        &self,
        generation: PluginGenerationId,
        plugin: &str,
        call: &str,
    ) -> Result<bool, PluginError> {
        let owned = self.owned(generation)?;
        owned.process(plugin)?.cancel_call(call)
    }

    pub fn shutdown(&self) -> Result<(), PluginError> {
        let current = {
            let mut held = self.current.write().map_err(|_| PluginError::Poisoned)?;
            held.take()
        };
        current.map_or(Ok(()), |current| current.retire())
    }

    pub fn cancel_generation(&self, generation: PluginGenerationId) -> Result<bool, PluginError> {
        let current = {
            let mut held = self.current.write().map_err(|_| PluginError::Poisoned)?;
            if held
                .as_ref()
                .is_some_and(|owned| owned.generation.id() == generation)
            {
                held.take()
            } else {
                None
            }
        };
        let Some(current) = current else {
            return Ok(false);
        };
        current
            .health
            .invalidate("plugin bridge exited because its generation was cancelled".to_owned());
        let _ = current.retire();
        Ok(true)
    }
}

impl Drop for PluginSupervisor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn next_id(counter: &AtomicU64, label: &str) -> Result<u64, PluginError> {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            value.checked_add(1)
        })
        .map_err(|_| PluginError::Registration(format!("plugin {label} identity overflowed")))
}

/// Pin the admitted module files, load the plugin to learn its identity, bind
/// that identity to the process, and only then run setup.
fn load_plugin(
    process: &PluginProcess,
    declaration: &PluginDeclaration,
    snapshot: &SourceSnapshot,
    app: &Value,
    limits: BridgeLimits,
) -> Result<PluginRegistration, PluginError> {
    let startup = Duration::from_millis(limits.startup_millis);
    let files = snapshot.files().collect::<Vec<_>>();
    for batch in files.chunks(PIN_BATCH) {
        let rows = batch
            .iter()
            .map(|file| json!([file.path, file.sha256, file.bytes]))
            .collect::<Vec<_>>();
        process.startup_exchange(
            "pin-files",
            json!({"files": rows}),
            startup,
            "plugin source pinning",
        )?;
    }
    let loaded = process.startup_exchange(
        "load",
        json!({
            "root": child_path(snapshot.root()),
            "entrypoint": snapshot.entrypoint(),
            "sha256": snapshot.digest(),
            "limits": limits,
        }),
        startup,
        "plugin load",
    )?;
    let plugin = loaded
        .get("plugin_id")
        .and_then(Value::as_str)
        .filter(|plugin| !plugin.is_empty() && plugin.len() <= 256)
        .ok_or_else(|| {
            PluginError::Registration("plugin load returned no bounded identity".to_owned())
        })?;
    process.bind_plugin(plugin, &declaration.executable.admitted_identity())?;
    let registration = process.startup_exchange(
        "activate",
        json!({
            "app": app,
            "plugin": {
                "target": declaration.target,
                "options": declaration.options,
                "origin": declaration.origin,
                "source": declaration.source,
            },
        }),
        startup,
        "activation",
    )?;
    let registration: PluginRegistration = serde_json::from_value(registration)
        .map_err(|error| PluginError::Registration(error.to_string()))?;
    if registration.plugin_id != plugin {
        return Err(PluginError::Registration(
            "plugin registration identity differs from its loaded identity".to_owned(),
        ));
    }
    Ok(registration)
}

#[cfg(test)]
mod tests {
    use super::process::ViolationKind;
    use super::RefusingPluginHost;
    use super::*;
    use crate::{resolve_declarations, ConfigDocument, PluginOrigin, DEFAULT_BRIDGE_LIMITS};
    use sha2::{Digest, Sha256};
    use std::error::Error;

    /// Removes the fixture root when the test scope ends, so a failed or
    /// early-returned witness can never leave a private runtime directory.
    struct TestRoot(PathBuf);

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug)]
    struct FixedClock;

    impl grapt_types::ClockAuthority for FixedClock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(1_788_220_800_000)
        }
    }

    #[derive(Debug)]
    struct OneId;

    impl grapt_types::IdAuthority for OneId {
        fn mint(&self) -> grapt_types::IdToken {
            grapt_types::IdToken::new(format!("unit{}nonce", std::process::id()))
                .unwrap_or_else(|| panic!("a non-empty token was refused"))
        }
    }

    fn test_node() -> Result<PathBuf, Box<dyn Error>> {
        let candidate = if let Some(path) = std::env::var_os("NODE") {
            PathBuf::from(path)
        } else {
            let (program, argument) = if cfg!(windows) {
                ("where.exe", "node.exe")
            } else {
                ("which", "node")
            };
            let output = Command::new(program).arg(argument).output()?;
            if !output.status.success() {
                return Err("Node is required for the publish-reason witness".into());
            }
            let text = String::from_utf8(output.stdout)?;
            let first = text
                .lines()
                .next()
                .ok_or("node lookup returned no executable")?;
            PathBuf::from(first)
        };
        let output = Command::new(&candidate)
            .arg("-e")
            .arg(
                r#"if (process.versions.node.split('.')[0] !== '24') {
                    throw new Error(`plugin tests require Node 24, found ${process.version}; install Node 24 and set NODE to its executable`)
                }
                process.stdout.write(process.execPath)"#,
            )
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "node candidate {candidate:?} did not run: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        let resolved = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        if !resolved.is_absolute() || !resolved.is_file() {
            return Err("node candidate did not resolve to an absolute file".into());
        }
        Ok(resolved)
    }

    fn file_sha256(path: &Path) -> Result<String, Box<dyn Error>> {
        let digest = Sha256::digest(std::fs::read(path)?);
        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// The R3 witness: a member whose control carries a recorded violation
    /// while the generation reason has not settled yet - the exact window
    /// between the watchdog's terminate_for and the stream-end
    /// invalidation - must be named by publish instead of the generic
    /// "exited before publication" literal.
    #[test]
    fn publish_names_the_recorded_member_violation_before_the_generic_exit(
    ) -> Result<(), Box<dyn Error>> {
        let root = std::env::temp_dir().join(format!(
            "secode-plugin-publish-reason-{}",
            std::process::id()
        ));
        let _root_guard = TestRoot(root.clone());
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;
        let plugin = root.join("plugin.mjs");
        std::fs::write(
            &plugin,
            "export default {id:'fixture.publish.reason',setup(){}}\n",
        )?;
        let executable = test_node()?;
        let version =
            String::from_utf8(Command::new(&executable).arg("--version").output()?.stdout)?
                .trim()
                .to_owned();
        let bridge =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/plugin-bridge/index.mjs");
        let runtime = BridgeRuntime {
            expected: RuntimePin {
                version,
                executable_sha256: file_sha256(&executable)?,
                bridge_sha256: file_sha256(&bridge)?,
            },
            executable,
            bridge: bridge.to_path_buf(),
            private_root: root.join("runtime"),
            recovery: RecoveryJournal {
                root: root.join("recovery"),
                clock: Arc::new(FixedClock),
                ids: Arc::new(OneId),
            },
        };
        let supervisor = PluginSupervisor::with_host(
            runtime,
            DEFAULT_BRIDGE_LIMITS,
            Arc::new(RefusingPluginHost),
        )?;
        let resolved = resolve_declarations(&[ConfigDocument {
            directory: plugin.parent().ok_or("plugin has no parent")?.to_path_buf(),
            source: "explicit-fixture".to_owned(),
            origin: PluginOrigin::LaunchExplicit,
            value: serde_json::json!({"plugins":[{"package":"./plugin.mjs","options":{"prefix":"ok"}}]}),
        }])?;
        if !resolved.blocked.is_empty() {
            return Err(format!("fixture declaration was blocked: {:?}", resolved.blocked).into());
        }
        let candidate = supervisor.prepare(resolved.active, serde_json::json!({}))?;
        let member = candidate
            .owned
            .processes
            .values()
            .next()
            .ok_or("candidate has no member process")?;
        member.control.record_violation(
            ViolationKind::Resource,
            "PLR-R3 unit witness: the member violation was recorded before publication".to_owned(),
        )?;
        assert!(
            candidate.owned.health.violation().is_none(),
            "witness precondition failed: the generation reason already settled"
        );
        let refused = supervisor
            .publish(candidate)
            .err()
            .ok_or("a not-running candidate was published")?;
        let message = refused.to_string();
        assert!(
            message.contains("PLR-R3 unit witness"),
            "publish discarded the recorded member violation: {message}"
        );
        assert!(
            !message.contains("exited before publication"),
            "publish fell back to the generic exit literal: {message}"
        );
        Ok(())
    }
}

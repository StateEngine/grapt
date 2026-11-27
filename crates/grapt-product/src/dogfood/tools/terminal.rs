//! Session-owned terminal resources. Tool settlements remain owned by the runtime.
//!
//! Modified by Grapt: typed session ownership, bounded reservations and
//! output capture acknowledgement; no parallel durable operation store.
use super::*;
use grapt_shell::{CleanupState, PipeState, ShellTerminal};
use grapt_types::OperationId;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};
mod arguments;
mod grants;
pub(super) use grants::GrantLease;
mod command;
#[cfg(target_os = "linux")]
pub(super) mod linux_cleanup;
mod observed;
use grapt_tools_local::terminal::output;
mod process;
#[cfg(target_os = "macos")]
pub(in crate::dogfood) use process::macos_cleanup;
#[cfg(windows)]
pub(super) use process::{launch_path, shell_cwd};
pub(in crate::dogfood) mod recovery;
mod report;
mod session_stop;
mod shell_owners;
mod shell_path;
#[cfg(windows)]
use grapt_tools_local::terminal::shim_protocol;
#[cfg(windows)]
pub(super) use grapt_tools_local::terminal::stop;
mod supervise;
#[cfg(test)]
mod tests;
#[cfg(windows)]
pub(super) mod windows_diagnostics;
#[cfg(windows)]
use grapt_tools_local::terminal::windows_input;
#[cfg(windows)]
pub(super) mod windows_job;
#[cfg(windows)]
mod windows_msys;
#[cfg(windows)]
pub(super) mod windows_shim;
pub(super) use arguments::{
    permission_resource, prepare_exec, process_capability, ExecArguments, ExecSpec, InputLine,
    StdinArguments, StdinSpec, EXEC_COMMAND, WRITE_STDIN,
};
#[cfg(windows)]
pub(crate) use windows_shim::shim_main;

pub(super) const EXEC_DESCRIPTION: &str = "Run cmd with optional PTY; session_id/output feeds write_stdin. shell: prepared PATH name or absolute path. login defaults false; true loads profiles. Marks omissions; workdir in workspace. shell_environment_policy strips credentials. Ends process group at exit; escaped daemons may survive. Windows msys_unconfined=true asks Once/Session consent for MSYS children; grants nothing.";

pub(in crate::dogfood) fn exec_description(environment: &shell_environment::Environment) -> String {
    format!(
        "{EXEC_DESCRIPTION} Available shells: {}.{}",
        shell_path::available(environment),
        if environment.sandbox().is_some() {
            " Denied write: request_sandbox_access for retry."
        } else {
            ""
        }
    )
}
pub(super) const STDIN_DESCRIPTION: &str = "Poll exec_command or send chars (empty polls). Poll wait 5..300s; write 250..30000ms. Opaque. Pass back exactly the output_cursor from the most recent response to continue; do not add page lengths or compute it. Handles expire on eviction/restart: use history; never rerun. Failed/cancelled input stops process; never replay uncertain input. Windows Ctrl-C stops the owned job without PTY; tty=true sends console Ctrl-C.";

const GLOBAL_RESOURCES: usize = 64;
const SESSION_RESOURCES: usize = 8;
const DRAIN_GRACE: Duration = Duration::from_secs(2);
pub(super) use grapt_tools_local::terminal::stop::STOP_SETTLE_BOUND;
/// How long a console Ctrl-C may take to end a Windows terminal command
/// before its job is terminated instead.
#[cfg(windows)]
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const INTERRUPT_FALLBACK: &str = "Ctrl-C did not end the command within 2 seconds (an ancestor process may have disabled console Ctrl-C)";
/// Windows' exit status for a process ended by Ctrl-C, as an exit code.
const CONTROL_C_EXIT_CODE: i32 = -1_073_741_510;

/// The result's account of a forced interrupt: that termination was
/// requested, and the exit actually observed. Which of the command, the
/// shim or an escalation produced a status is not known here, so no order
/// or cause is claimed beyond what the status itself is.
fn interrupt_note(forced: bool, exit: Option<&ShellTerminal>) -> Option<String> {
    forced.then(|| match exit {
        None => format!("{INTERRUPT_FALLBACK}; termination of its job was requested"),
        Some(ShellTerminal::Exited(CONTROL_C_EXIT_CODE)) => format!(
            "{INTERRUPT_FALLBACK}; termination of its job was requested and it exited with the Ctrl-C exit status"
        ),
        Some(ShellTerminal::Exited(code)) => format!(
            "{INTERRUPT_FALLBACK}; termination of its job was requested and it exited with status {code}"
        ),
        Some(_) => format!(
            "{INTERRUPT_FALLBACK}; termination of its job was requested; the status reports how it ended"
        ),
    })
}

#[derive(Default)]
struct Entries {
    grant_generation: u64,
    by_id: BTreeMap<OperationId, Arc<Entry>>,
    order: VecDeque<OperationId>,
    /// Closed Sessions' terminals whose cleanup has not completed. No handle
    /// reaches them any more, but each keeps its resource permit, so a
    /// descendant whose exit is unconfirmed can never return global capacity.
    /// Bounded by the permits themselves; pruned once cleanup completes.
    quarantine: Vec<Arc<Entry>>,
}

impl Entries {
    fn prune_quarantine(&mut self) {
        self.quarantine.retain(|entry| !entry.evictable());
    }
}

pub(in crate::dogfood) struct TerminalRegistry {
    entries: Mutex<Entries>,
    capacity: Arc<Semaphore>,
    history: Option<Arc<observed::History>>,
    shells: shell_owners::ShellOwners,
}

impl std::fmt::Debug for TerminalRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalRegistry")
            .field("resources", &lock(&self.entries).by_id.len())
            .finish()
    }
}

impl Default for TerminalRegistry {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            capacity: Arc::new(Semaphore::new(GLOBAL_RESOURCES)),
            history: None,
            shells: shell_owners::ShellOwners::default(),
        }
    }
}

#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct Observation {
    enforcement: RealizedEnforcement,
    output: output::OutputBuffer,
    cursor: u64,
    exit: Option<ShellTerminal>,
    readers: usize,
    writer_open: bool,
    pipe: PipeState,
    cleanup: CleanupState,
    cleanup_error: Option<String>,
    /// A Ctrl-C written to the terminal did not end the command, and the
    /// fallback requested termination. What happened is read from `exit`.
    forced_interrupt: bool,
    end_recorded: bool,
    /// The blocking spawn owner has returned and cannot install more workers.
    startup_settled: bool,
    supervised: bool,
    did_not_start: bool,
    persistence_error: Option<String>,
    process_notes: Vec<String>,
}

impl Observation {
    fn cleanup(&self) -> CleanupState {
        if self.cleanup == CleanupState::Complete && self.readers == 0 && !self.writer_open {
            CleanupState::Complete
        } else {
            CleanupState::Pending
        }
    }

    fn observed_cleanup_error(&self) -> Option<String> {
        let error = match (&self.cleanup_error, &self.persistence_error) {
            (Some(cleanup), Some(persistence)) => Some(format!("{cleanup}; {persistence}")),
            (Some(error), None) | (None, Some(error)) => Some(error.clone()),
            (None, None) => None,
        };
        error.or_else(|| {
            (self.exit.is_some() && self.pipe == PipeState::TimedOut
                && (self.readers != 0 || self.writer_open)).then(|| {
                    format!("terminal I/O cleanup is incomplete: {} output readers and {} input writer remain; their resource capacity stays reserved", self.readers, usize::from(self.writer_open))
                })
        })
    }
}

struct Input {
    bytes: Vec<u8>,
    reply: oneshot::Sender<Result<(), String>>,
}

struct Entry {
    operation: OperationId,
    session: SessionId,
    spec: ExecSpec,
    write_resources: Vec<PathBuf>,
    observation: Mutex<Observation>,
    control: Mutex<Option<Arc<Mutex<process::Control>>>>,
    input: Mutex<Option<mpsc::Sender<Input>>>,
    /// Input delivered since the last finished line, so a command typed in
    /// pieces is decided whole; `None` once that input is unknown.
    pending_input: Mutex<Option<String>>,
    cancelled: AtomicBool,
    interaction: AtomicBool,
    changed: Notify,
    cancel: Notify,
    _resource: Arc<OwnedSemaphorePermit>,
    history: Option<Arc<observed::History>>,
    ending: Mutex<()>,
}

/// The longest unfinished input line kept for permission decisions.
const MAX_PENDING_INPUT: usize = 64 * 1024;

impl Entry {
    /// The permission resource lines that join `chars` to the unfinished
    /// line delivered before it, and the line `chars` leaves once delivered.
    /// Nothing is recorded here: a write can be refused or fail after this.
    fn input_line(&self, chars: &str) -> (String, InputLine) {
        let before = lock(&self.pending_input).clone();
        let Some(earlier) = before.clone() else {
            let section = format!("\n{}", grapt_permission::command_safety::UNREAD_INPUT);
            return (
                section,
                InputLine {
                    before,
                    after: None,
                },
            );
        };
        let joined = earlier + chars;
        // An editor ignores Ctrl-C and Ctrl-D in some states, so the line
        // is also read as if they were not typed.
        let line = joined.replace(['\u{3}', '\u{4}'], "");
        let section = if line == chars {
            String::new()
        } else {
            format!("\nstdin line: {line}")
        };
        let tail = grapt_permission::command_safety::unfinished_line(&joined);
        let after = (tail.len() <= MAX_PENDING_INPUT).then(|| tail.to_owned());
        (section, InputLine { before, after })
    }

    /// Record a write's outcome: a delivered decided write leaves its line;
    /// undecided or uncertain input leaves the line unknown.
    fn record_input(&self, line: Option<&InputLine>, delivered: bool) {
        *lock(&self.pending_input) = line
            .filter(|_| delivered)
            .and_then(|line| line.after.clone());
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        lock(&self.input).take();
        if let Some(control) = lock(&self.control).as_ref() {
            let result = lock(control).cancel();
            if let Err(error) = result {
                lock(&self.observation).cleanup_error = Some(error);
            }
        }
        self.cancel.notify_one();
    }

    fn evictable(&self) -> bool {
        let data = lock(&self.observation);
        data.exit.is_some()
            && data.startup_settled
            && data.end_recorded
            && data.pipe.is_settled()
            && data.cleanup() == CleanupState::Complete
            && data.observed_cleanup_error().is_none()
            && !self.interaction.load(Ordering::Acquire)
    }

    fn acquire(self: &Arc<Self>) -> Result<Interaction, String> {
        self.interaction
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "terminal is busy with another interaction".to_owned())?;
        Ok(Interaction {
            entry: Arc::clone(self),
        })
    }
}

struct Interaction {
    entry: Arc<Entry>,
}
impl Drop for Interaction {
    fn drop(&mut self) {
        self.entry.interaction.store(false, Ordering::Release);
    }
}

/// A destructive interaction is retained only once its output artifact exists.
/// Dropping an unacknowledged input delivery stops the process: replaying it
/// could otherwise execute an uncertain write twice.
pub(in crate::dogfood::tools) struct Delivery {
    entry: Arc<Entry>,
    _interaction: Interaction,
    cursor: u64,
    cancel_on_drop: bool,
}

impl Delivery {
    pub fn captured(mut self) {
        lock(&self.entry.observation).cursor = self.cursor;
        self.cancel_on_drop = false;
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.entry.cancel();
        }
    }
}

impl TerminalRegistry {
    pub(in crate::dogfood) fn with_history(
        inbox: Arc<Mutex<Inbox>>,
        ids: Arc<OsRandomIds>,
        clock: Arc<SystemClock>,
    ) -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            capacity: Arc::new(Semaphore::new(GLOBAL_RESOURCES)),
            history: Some(Arc::new(observed::History::new(inbox, ids, clock))),
            shells: shell_owners::ShellOwners::default(),
        }
    }

    pub(in crate::dogfood) fn contains(
        &self,
        session: &SessionId,
        operation: &OperationId,
    ) -> bool {
        lock(&self.entries)
            .by_id
            .get(operation)
            .is_some_and(|entry| &entry.session == session)
    }

    /// Signal only this session's processes, retaining handles and permissions.
    pub(in crate::dogfood) fn cancel_session(&self, session: &SessionId) {
        self.shells.cancel_session(session);
        let entries = lock(&self.entries);
        for entry in entries.by_id.values().chain(&entries.quarantine) {
            if &entry.session == session {
                entry.cancel();
            }
        }
    }

    pub(in crate::dogfood) fn close_session(&self, session: &SessionId) {
        self.shells.cancel_session(session);
        let mut entries = lock(&self.entries);
        entries.grant_generation = entries.grant_generation.saturating_add(1);
        let mut closed = Vec::new();
        entries.by_id.retain(|_, entry| {
            if &entry.session == session {
                entry.cancel();
                closed.push(Arc::clone(entry));
                false
            } else {
                true
            }
        });
        let retained: std::collections::BTreeSet<_> = entries.by_id.keys().cloned().collect();
        entries.order.retain(|id| retained.contains(id));
        entries.quarantine.extend(closed);
        entries.prune_quarantine();
    }

    /// Fence new execution and drain its execution owner before calling.
    /// This confirms cleanup without removing handles or changing grants.
    pub(in crate::dogfood) async fn stop_session(&self, session: &SessionId) -> Result<(), String> {
        let (terminals, shells) = tokio::join!(
            session_stop::stop_session(self, session),
            self.stop_shells(session),
        );
        match (terminals, shells) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(reason), Ok(())) | (Ok(()), Err(reason)) => Err(reason),
            (Err(terminals), Err(shells)) => Err(format!("{terminals}; {shells}")),
        }
    }

    async fn stop_shells(&self, session: &SessionId) -> Result<(), String> {
        self.shells.stop(session).await
    }

    fn reserve(
        &self,
        session: SessionId,
        operation: OperationId,
        mut spec: ExecSpec,
    ) -> Result<Arc<Entry>, String> {
        let mut entries = lock(&self.entries);
        grants::validate(&entries, &spec)?;
        entries.prune_quarantine();
        if entries.by_id.contains_key(&operation) {
            return Err("duplicate terminal identity".into());
        }
        loop {
            let same = entries
                .by_id
                .values()
                .filter(|entry| entry.session == session)
                .count();
            if same < SESSION_RESOURCES && entries.by_id.len() < GLOBAL_RESOURCES {
                break;
            }
            let victim = entries
                .order
                .iter()
                .find(|id| {
                    entries.by_id.get(*id).is_some_and(|entry| {
                        (same < SESSION_RESOURCES || entry.session == session) && entry.evictable()
                    })
                })
                .cloned();
            let victim = victim
                .ok_or("terminal resource limit reached; finish or stop an existing process")?;
            entries.by_id.remove(&victim);
            entries.order.retain(|id| id != &victim);
        }
        let permit = Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| "terminal resources are still draining; capacity is unavailable")?;
        let write_resources = spec.environment.release_write_targets();
        let entry = Arc::new(Entry {
            operation: operation.clone(),
            session,
            spec,
            write_resources,
            observation: Mutex::new(Observation {
                enforcement: RealizedEnforcement::HarnessOnly,
                output: output::OutputBuffer::default(),
                cursor: 0,
                exit: None,
                readers: 0,
                writer_open: false,
                pipe: PipeState::Open,
                cleanup: CleanupState::Pending,
                cleanup_error: None,
                forced_interrupt: false,
                end_recorded: self.history.is_none(),
                startup_settled: false,
                supervised: false,
                did_not_start: false,
                persistence_error: None,
                process_notes: Vec::new(),
            }),
            control: Mutex::new(None),
            input: Mutex::new(None),
            pending_input: Mutex::new(Some(String::new())),
            cancelled: AtomicBool::new(false),
            interaction: AtomicBool::new(false),
            changed: Notify::new(),
            cancel: Notify::new(),
            _resource: Arc::new(permit),
            history: self.history.clone(),
            ending: Mutex::new(()),
        });
        entries.order.push_back(operation.clone());
        entries.by_id.insert(operation, Arc::clone(&entry));
        Ok(entry)
    }

    fn lookup(&self, session: &SessionId, operation: &OperationId) -> Result<Arc<Entry>, String> {
        if let Some(entry) = lock(&self.entries)
            .by_id
            .get(operation)
            .filter(|entry| &entry.session == session)
            .cloned()
        {
            return Ok(entry);
        }
        if let Some(history) = &self.history {
            if let Some(reason) = history.missing(session, operation)? {
                return Err(reason);
            }
        }
        Err("terminal session_id is absent, expired, or belongs to another Session; status unknown; use history({}) to find this session's retained results; do not rerun the command to recover its output".into())
    }

    pub(super) fn prepare_stdin(
        &self,
        root: &Path,
        session: &SessionId,
        input: &BoundedToolInput,
    ) -> Result<PreparedEffect, String> {
        let mut spec = arguments::decode_stdin(input)?;
        let entry = self.lookup(session, &spec.operation)?;
        let (action, resource, capability) = if spec.chars.is_empty() {
            let capability = CapabilityId::derive(&ResolvedCapability::new(
                EffectClass::ToolInvoke,
                None,
                FilesystemAuthority::default(),
                EgressAuthority::default(),
                RealizedEnforcement::HarnessOnly,
            ));
            (
                "shell.poll".to_owned(),
                "session terminal output".to_owned(),
                capability,
            )
        } else {
            let (section, line) = entry.input_line(&spec.chars);
            spec.line = Some(line);
            (
                "shell".to_owned(),
                format!(
                    "{}\nworkdir: {}\nstdin: {}{section}",
                    entry.spec.command,
                    path_text(&entry.spec.cwd),
                    spec.chars,
                ),
                arguments::process_capability(root, &entry.spec),
            )
        };
        Ok(PreparedEffect {
            target: EffectTarget::TerminalStdin(spec),
            action,
            resource,
            capability,
        })
    }

    pub(super) async fn exec(
        &self,
        session: &SessionId,
        operation: OperationId,
        spec: ExecSpec,
    ) -> Result<(Vec<u8>, Delivery), mcp_call::EffectFailure> {
        if let Some(history) = &self.history {
            history
                .record(
                    session,
                    &operation,
                    grapt_event::session_terminal::TerminalState::Starting,
                    None,
                )
                .map_err(|error| {
                    mcp_call::EffectFailure::from(format!(
                        "command did not start; terminal admission could not be saved: {error}"
                    ))
                })?;
        }
        self.persist_completed();
        let entry = match self.reserve(session.clone(), operation.clone(), spec.clone()) {
            Ok(entry) => entry,
            Err(error) => {
                if let Some(history) = &self.history {
                    history.record(session, &operation, grapt_event::session_terminal::TerminalState::NotStarted { reason: error.clone() }, None)
                        .map_err(|record| mcp_call::EffectFailure::from(format!("command did not start: {error}; its refusal could not be saved: {record}")))?;
                }
                return Err(mcp_call::EffectFailure::from(error));
            }
        };
        let interaction = entry.acquire().map_err(mcp_call::EffectFailure::from)?;
        let mut delivery = Delivery {
            entry: Arc::clone(&entry),
            _interaction: interaction,
            cursor: 0,
            cancel_on_drop: true,
        };
        let spawned_entry = Arc::clone(&entry);
        // The blocking owner must settle its own startup failure: the caller
        // can disappear while this task is queued, dropping its JoinHandle.
        let started = tokio::task::spawn_blocking(move || {
            let result = supervise::start(Arc::clone(&spawned_entry), spec)
                .map_err(|failure| settle_spawn_failure(&spawned_entry, failure));
            lock(&spawned_entry.observation).startup_settled = true;
            spawned_entry.changed.notify_waiters();
            result
        })
        .await
        .map_err(|error| {
            settle_spawn_failure(
                &entry,
                process::SpawnFailure::from(format!("terminal spawn task failed: {error}")),
            )
        })?;
        started?;
        let (bytes, cursor) = observe(&entry, entry.spec.wait_ms, entry.spec.page_bytes, None)
            .await
            .map_err(mcp_call::EffectFailure::from)?;
        delivery.cursor = cursor;
        Ok((bytes, delivery))
    }

    pub(super) fn contained_input(&self, session: &SessionId, spec: &StdinSpec) -> bool {
        !spec.chars.is_empty()
            && self
                .lookup(session, &spec.operation)
                .is_ok_and(|entry| entry.spec.environment.sandbox().is_some())
    }

    pub(super) async fn stdin(
        &self,
        session: &SessionId,
        spec: &StdinSpec,
    ) -> Result<(Vec<u8>, Delivery), mcp_call::EffectFailure> {
        let entry = self
            .lookup(session, &spec.operation)
            .map_err(mcp_call::EffectFailure::from)?;
        let interaction = entry.acquire().map_err(mcp_call::EffectFailure::from)?;
        let mut delivery = Delivery {
            entry: Arc::clone(&entry),
            _interaction: interaction,
            cursor: 0,
            cancel_on_drop: false,
        };
        // Refuse an invalid retry cursor before sending input.
        if let Some(cursor) = spec.cursor {
            let data = lock(&entry.observation);
            if cursor > data.output.total() {
                return Err(mcp_call::EffectFailure::from(format!(
                    "cursor {cursor} is past the observed output; pass the latest returned output_cursor ({}) unchanged.",
                    data.cursor
                )));
            }
        }
        if !entry.spec.tty && spec.chars == "\u{3}" {
            let control = lock(&entry.control)
                .clone()
                .ok_or_else(|| mcp_call::EffectFailure::from("terminal has no process handle"))?;
            lock(&control)
                .interrupt()
                .map_err(mcp_call::EffectFailure::from)?;
            delivery.cancel_on_drop = true;
        } else if !spec.chars.is_empty() {
            // Another write delivered since this one was decided would change
            // the line it completes.
            if spec
                .line
                .as_ref()
                .is_some_and(|line| *lock(&entry.pending_input) != line.before)
            {
                return Err(mcp_call::EffectFailure::from(
                    "terminal input changed after this write was decided; send it again".to_owned(),
                ));
            }
            let sender = lock(&entry.input).clone().ok_or_else(|| {
                mcp_call::EffectFailure::from("terminal stdin is closed".to_owned())
            })?;
            delivery.cancel_on_drop = true;
            let (reply, received) = oneshot::channel();
            let writing = async {
                sender
                    .send(Input {
                        bytes: spec.chars.as_bytes().to_vec(),
                        reply,
                    })
                    .await
                    .map_err(|_| "terminal writer stopped")?;
                received
                    .await
                    .map_err(|_| "terminal input delivery is unknown")?
                    .map_err(|_| "terminal input write failed")
            };
            let written = tokio::time::timeout(DRAIN_GRACE, writing).await;
            entry.record_input(spec.line.as_ref(), matches!(written, Ok(Ok(()))));
            match written {
                Ok(Ok(())) => (),
                _ => return Err(mcp_call::EffectFailure { reason: ToolFailureReason::EffectUnknown, detail: "terminal stdin delivery is uncertain; process cleanup was requested; do not replay this input".into() }),
            }
            #[cfg(windows)]
            if entry.spec.tty && spec.chars.contains('\u{3}') {
                interrupt_fallback(&entry);
            }
        }
        let (bytes, cursor) = observe(&entry, spec.wait_ms, spec.page_bytes, spec.cursor)
            .await
            .map_err(mcp_call::EffectFailure::from)?;
        delivery.cursor = cursor;
        Ok((bytes, delivery))
    }
}

/// A console Ctrl-C written to a Windows terminal never stays silently lost:
/// if the command is still running after [`INTERRUPT_GRACE`], its job is
/// terminated with the Ctrl-C status and the result says so.
#[cfg(windows)]
fn interrupt_fallback(entry: &Arc<Entry>) {
    let entry = Arc::clone(entry);
    tokio::spawn(async move {
        tokio::time::sleep(INTERRUPT_GRACE).await;
        if lock(&entry.observation).exit.is_some() {
            return;
        }
        let Some(control) = lock(&entry.control).clone() else {
            return;
        };
        let forced = lock(&control).force_interrupt();
        {
            let mut data = lock(&entry.observation);
            match forced {
                // The command exited before the fallback acted: nothing to report.
                Ok(false) => return,
                Ok(true) => data.forced_interrupt = true,
                Err(error) => {
                    data.forced_interrupt = true;
                    data.cleanup_error = Some(error);
                }
            }
        }
        entry.changed.notify_waiters();
    });
}

fn settle_spawn_failure(
    entry: &Entry,
    mut failure: process::SpawnFailure,
) -> mcp_call::EffectFailure {
    #[cfg(windows)]
    if let Some(record) = &failure.diagnostic {
        windows_diagnostics::log(record);
    }
    let process_started = lock(&entry.control).is_some();
    if sandbox::receipt(&failure.detail).is_none() {
        let enforcement = if process_started {
            lock(&entry.observation).enforcement.clone()
        } else if entry.spec.environment.sandbox().is_some() {
            sandbox::degraded(failure.detail.clone())
        } else {
            entry.spec.environment.intended()
        };
        let ran = if process_started {
            Some(true)
        } else {
            (!failure.effects_uncertain).then_some(false)
        };
        failure.detail = sandbox::failure(&enforcement, &failure.detail, ran);
    }
    entry.cancel();
    let uncertain = process_started || failure.effects_uncertain;
    if !process_started {
        let mut data = lock(&entry.observation);
        if let Some(receipt) = sandbox::Receipt::parse(&failure.detail) {
            data.enforcement = receipt.enforcement;
        }
        data.exit = Some(ShellTerminal::Failed(failure.detail.clone()));
        data.supervised = true;
        data.did_not_start = !uncertain;
        data.pipe = PipeState::Drained;
        if uncertain {
            data.cleanup_error = Some(
                "spawn failed without returned process handles; cleanup cannot be confirmed".into(),
            );
        } else {
            data.cleanup = CleanupState::Complete;
        }
    }
    if !process_started {
        entry.persist_end();
    }
    let mut receipt = sandbox::receipt(&failure.detail).unwrap_or_else(|| serde_json::json!({}));
    let mut detail = receipt
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("terminal startup failed")
        .to_owned();
    if let Some(error) = &lock(&entry.observation).persistence_error {
        detail = format!("{detail}; {error}; operation {}", entry.operation.as_str());
    }
    if uncertain {
        detail.push_str("; command effects or cleanup are uncertain; do not replay the command");
        if !process_started {
            receipt["ran"] = serde_json::Value::Null;
        }
    }
    receipt["error"] = serde_json::json!(detail);
    let detail = receipt.to_string();
    lock(&entry.observation).startup_settled = true;
    entry.changed.notify_waiters();
    if uncertain {
        mcp_call::EffectFailure {
            reason: ToolFailureReason::EffectUnknown,
            detail,
        }
    } else {
        mcp_call::EffectFailure::from(detail)
    }
}

async fn observe(
    entry: &Entry,
    wait_ms: u64,
    bound: usize,
    cursor: Option<u64>,
) -> Result<(Vec<u8>, u64), String> {
    let _elapsed = tokio::time::timeout(Duration::from_millis(wait_ms), async {
        loop {
            let changed = entry.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let settled = {
                let data = lock(&entry.observation);
                data.exit.is_some()
                    && data.pipe.is_settled()
                    && (data.cleanup() == CleanupState::Complete
                        || data.observed_cleanup_error().is_some())
            };
            if settled {
                break;
            }
            changed.await;
        }
    })
    .await;
    entry.persist_end();
    let data = lock(&entry.observation);
    let page = data
        .output
        .page(cursor.unwrap_or(data.cursor), bound, data.pipe.is_settled())?;
    let next = page.cursor;
    let mut sandbox_notes = entry.spec.environment.sandbox_notes(&page.bytes);
    sandbox_notes.extend(data.process_notes.iter().cloned());
    let report = report::ProcessReport {
        sandbox_notes,
        enforcement: data.enforcement.clone(),
        operation: entry.operation.clone(),
        exit: data.exit.clone(),
        pipe: data.pipe,
        cleanup: data.cleanup(),
        cleanup_error: data.observed_cleanup_error(),
        interrupt: interrupt_note(data.forced_interrupt, data.exit.as_ref()),
        total_bytes: data.output.total(),
        origin: entry.spec.origin.clone(),
        page,
    };
    Ok((report.encode()?, next))
}

impl Drop for TerminalRegistry {
    fn drop(&mut self) {
        for entry in lock(&self.entries).by_id.values() {
            entry.cancel();
        }
    }
}

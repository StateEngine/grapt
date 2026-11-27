use super::*;
use grapt_artifact::{ArtifactStore, ReadBudget};
use grapt_codemode::{GuestResult, HostFailure, HostReply, HostResult, WorkerMessage};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinSet;

type Jobs = Arc<tokio::sync::Mutex<JoinSet<HostCompletion>>>;

struct HostCompletion {
    reply: HostReply,
    files: usize,
    bytes: u64,
}

#[derive(Debug, Clone)]
struct Process {
    child: Arc<tokio::sync::Mutex<Child>>,
    jobs: Jobs,
    /// The OS container the worker runs in; the last clone dropped ends it and
    /// anything it started.
    group: Arc<processkit::ProcessGroup>,
}

impl Process {
    /// End whatever the worker left running and wait, until `deadline`, to see
    /// its tree empty: the worker itself exiting says nothing about what it
    /// started, and a kill is not proof that its targets are gone.
    async fn settle_tree(&self, deadline: tokio::time::Instant) -> Result<(), String> {
        let report = self
            .group
            .stop(Duration::ZERO, true)
            .await
            .map_err(|error| format!("process tree did not drain: {error}"))?;
        confirm_empty(report.members_after(), deadline, || {
            self.group.members().ok().map(|members| members.len())
        })
        .await
    }
}

/// How often an unconfirmed tree's membership is read again.
const MEMBERSHIP_POLL: Duration = Duration::from_millis(10);

/// Succeed only once the tree is observed empty by `deadline`: `first` and
/// then each `probe` is a member count, `None` when membership could not be
/// read. Survivors or an unreadable membership at `deadline`, or an empty
/// tree first seen after it, fail, so the caller keeps custody of a worker it
/// cannot prove gone in time.
async fn confirm_empty(
    first: Option<usize>,
    deadline: tokio::time::Instant,
    mut probe: impl FnMut() -> Option<usize>,
) -> Result<(), String> {
    let mut observed = first;
    loop {
        let now = tokio::time::Instant::now();
        if now <= deadline && observed == Some(0) {
            return Ok(());
        }
        if now >= deadline {
            return Err(undrained(observed));
        }
        // Half the time left, so a probe still lands before the deadline when
        // the timer wakes late; a sleep of all of it would wake past it.
        let remaining = deadline - now;
        let step = (remaining / 2).max(Duration::from_millis(1));
        tokio::time::sleep(remaining.min(MEMBERSHIP_POLL).min(step)).await;
        if tokio::time::Instant::now() > deadline {
            continue;
        }
        observed = probe();
    }
}

/// Why a tree was not confirmed empty by its deadline.
fn undrained(observed: Option<usize>) -> String {
    match observed {
        Some(0) => "process tree was seen empty only after its deadline".into(),
        Some(count) => format!("{count} process tree members survived the stop"),
        None => "process tree membership is unreadable; it may not have drained".into(),
    }
}

#[derive(Debug, Default)]
pub(in crate::dogfood) struct WorkerRegistry {
    children: Mutex<std::collections::BTreeMap<(SessionId, ToolCallId), Process>>,
}

impl WorkerRegistry {
    /// Stop retained workers after the session's execution has drained.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(in crate::dogfood) async fn stop_session(&self, session: &SessionId) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let mut affected = self
            .with_children_until(deadline, |children| {
                children
                    .iter()
                    .filter(|((owner, _), _)| owner == session)
                    .map(|(key, process)| (key.clone(), process.clone(), false))
                    .collect::<Vec<_>>()
            })
            .await?;
        let mut failures = Vec::new();
        // Signal every snapshotted child before draining any nested jobs.
        for ((_, call), process, terminating) in &mut affected {
            match tokio::time::timeout_at(deadline, process.child.lock()).await {
                Ok(mut child) => match child.start_kill() {
                    Ok(()) => *terminating = true,
                    Err(error) => match child.try_wait() {
                        // A concurrent reap may have collected this child
                        // after the registry snapshot, before we took its lock.
                        Ok(Some(_)) => *terminating = true,
                        status => {
                            failures.push(format!(
                                "CodeMode worker {}: start_kill failed: {error}",
                                call.as_str()
                            ));
                            if let Err(error) = status {
                                failures.push(format!(
                                    "CodeMode worker {}: try_wait failed: {error}",
                                    call.as_str()
                                ));
                            }
                        }
                    },
                },
                Err(_) => failures.push(format!(
                    "CodeMode worker {}: timed out acquiring child for termination",
                    call.as_str()
                )),
            }
        }
        let mut confirmed = Vec::new();
        for (key, process, terminating) in &affected {
            let call = key.1.as_str();
            let before = failures.len();
            let (waited, drained) = tokio::join!(
                tokio::time::timeout_at(deadline, async {
                    let mut child = process.child.lock().await;
                    child.wait().await
                }),
                tokio::time::timeout_at(deadline, async {
                    let mut jobs = process.jobs.lock().await;
                    jobs.abort_all();
                    while let Some(result) = jobs.join_next().await {
                        match result {
                            Err(error) if !error.is_cancelled() => {
                                failures.push(format!(
                                    "CodeMode worker {call}: nested job failed: {error}"
                                ));
                            }
                            _ => {}
                        }
                    }
                }),
            );
            match waited {
                Ok(Ok(_)) => {
                    match tokio::time::timeout_at(deadline, process.settle_tree(deadline)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => failures.push(format!("CodeMode worker {call}: {error}")),
                        Err(_) => failures.push(format!(
                            "CodeMode worker {call}: timed out draining its process tree"
                        )),
                    }
                }
                Ok(Err(error)) => {
                    failures.push(format!("CodeMode worker {call}: wait failed: {error}"));
                }
                Err(_) => failures.push(format!(
                    "CodeMode worker {call}: timed out acquiring child or waiting for exit"
                )),
            }
            if drained.is_err() {
                failures.push(format!(
                    "CodeMode worker {call}: timed out acquiring or draining nested jobs"
                ));
            }
            if *terminating && failures.len() == before {
                confirmed.push((key, process));
            }
        }
        match self
            .with_children_until(deadline, |children| {
                for (key, process) in confirmed {
                    if children.get(key).is_some_and(|current| {
                        Arc::ptr_eq(&current.child, &process.child)
                            && Arc::ptr_eq(&current.jobs, &process.jobs)
                    }) {
                        children.remove(key);
                    }
                }
                children.keys().any(|(owner, _)| owner == session)
            })
            .await
        {
            Ok(false) => {}
            Ok(true) => failures.push(
                "CodeMode workers remain registered for this session; retry session stop".into(),
            ),
            Err(error) => failures.push(error),
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    async fn with_children_until<T>(
        &self,
        deadline: tokio::time::Instant,
        access: impl FnOnce(&mut std::collections::BTreeMap<(SessionId, ToolCallId), Process>) -> T,
    ) -> Result<T, String> {
        // A blocking registry lock would escape the overall stop deadline.
        tokio::time::timeout_at(deadline, async {
            loop {
                match self.children.try_lock() {
                    Ok(mut children) => return access(&mut children),
                    Err(std::sync::TryLockError::Poisoned(error)) => {
                        return access(&mut error.into_inner());
                    }
                    Err(std::sync::TryLockError::WouldBlock) => {}
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| {
            "CodeMode registry lock timed out; workers remain retained for a session stop retry"
                .into()
        })
    }
}

#[derive(Debug)]
pub(in crate::dogfood::tools) struct Workers {
    registry: Arc<WorkerRegistry>,
    session: SessionId,
    owned: Mutex<std::collections::BTreeSet<ToolCallId>>,
}

impl Workers {
    pub(in crate::dogfood::tools) fn new(
        registry: Arc<WorkerRegistry>,
        session: SessionId,
    ) -> Self {
        Self {
            registry,
            session,
            owned: Mutex::new(std::collections::BTreeSet::new()),
        }
    }
    fn spawn(
        &self,
        call: &ToolCallId,
    ) -> Result<(ChildStdin, ChildStdout, Jobs), ToolFailureReason> {
        let key = (self.session.clone(), call.clone());
        let mut children = lock(&self.registry.children);
        if children.len() >= 3 || children.contains_key(&key) {
            return Err(ToolFailureReason::AuthorityUnavailable);
        }
        let executable = std::env::current_exe().map_err(|_| ToolFailureReason::EffectFailed)?;
        let mut command = Command::new(executable);
        command
            .arg(grapt_codemode::WORKER_ARGUMENT)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            if let Some(root) = std::env::var_os("SystemRoot") {
                command.env("SystemRoot", root);
            }
            command.creation_flags(0x0800_0000);
        }
        let group = processkit::ProcessGroup::new().map_err(|_| ToolFailureReason::EffectFailed)?;
        // On Windows the group's own spawn resets the creation flags to
        // CREATE_SUSPENDED alone, dropping CREATE_NO_WINDOW. The worker starts
        // nothing until it reads its first request, written only after this
        // returns, so joining the job once spawned leaves nothing outside it.
        #[cfg(windows)]
        let mut child = {
            let child = command
                .spawn()
                .map_err(|_| ToolFailureReason::EffectFailed)?;
            group
                .adopt(&child)
                .map_err(|_| ToolFailureReason::EffectFailed)?;
            child
        };
        #[cfg(not(windows))]
        let mut child = group
            .spawn(command)
            .map_err(|_| ToolFailureReason::EffectFailed)?;
        let pipes = child.stdin.take().zip(child.stdout.take());
        let jobs = Arc::new(tokio::sync::Mutex::new(JoinSet::new()));
        children.insert(
            key,
            Process {
                child: Arc::new(tokio::sync::Mutex::new(child)),
                jobs: Arc::clone(&jobs),
                group: Arc::new(group),
            },
        );
        lock(&self.owned).insert(call.clone());
        pipes
            .map(|(input, output)| (input, output, jobs))
            .ok_or(ToolFailureReason::EffectUnknown)
    }

    async fn reap(&self, call: &ToolCallId, kill: bool) -> Option<bool> {
        let key = (self.session.clone(), call.clone());
        let process = lock(&self.registry.children).get(&key).cloned();
        let Some(process) = process else {
            return Some(true);
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let result = tokio::time::timeout_at(deadline, async {
            let mut jobs = process.jobs.lock().await;
            jobs.abort_all();
            while jobs.join_next().await.is_some() {}
            drop(jobs);
            let mut child = process.child.lock().await;
            if kill {
                let _kill = child.start_kill();
            }
            let status = child.wait().await.map_err(|error| error.to_string())?;
            drop(child);
            process.settle_tree(deadline).await?;
            Ok::<_, String>(status)
        })
        .await;
        if let Ok(Ok(status)) = result {
            lock(&self.registry.children).remove(&key);
            lock(&self.owned).remove(call);
            Some(status.success())
        } else {
            None
        }
    }

    pub(in crate::dogfood::tools) async fn settle(&self) -> bool {
        let calls: Vec<_> = lock(&self.owned).iter().cloned().collect();
        let mut complete = true;
        for call in calls {
            complete &= self.reap(&call, true).await.is_some();
        }
        complete
    }
}

pub(in crate::dogfood::tools) async fn run(
    request: ExecuteRequest,
    nested: grapt_runtime::NestedTools,
    call: ToolCallId,
    workers: Arc<Workers>,
    ids: Arc<OsRandomIds>,
    store: Arc<FilesystemArtifactStore>,
) -> Result<Vec<u8>, mcp_call::EffectFailure> {
    let (mut input, mut output, jobs) = match workers.spawn(&call) {
        Ok(pipes) => pipes,
        Err(reason) => return Err(failure(reason, "CodeMode worker could not start")),
    };
    let result = async {
        let startup = crate::dogfood::mcp::nested_startup::NestedStartups::default();
        let clock = grapt_mcp_client::HumanWait::default();
        let deadline = clock.deadline(Duration::from_secs(30))?;
        let running = deadline.timeout(exchange(
            &request,
            &nested,
            &ids,
            &store,
            &jobs,
            &startup,
            (&mut input, &mut output),
        ));
        tokio::pin!(running);
        let mut human_pause = None;
        loop {
            tokio::select! {
                result = &mut running => break result,
                () = tokio::time::sleep(Duration::from_millis(50)) => {
                    // Ordinary host work and tool-form waits retain the guest's
                    // execution cap. Only startup owned/joined by this VM's
                    // active nested MCP calls excludes bounded human time.
                    let waiting = startup.is_waiting()?;
                    if waiting && human_pause.is_none() {
                        human_pause = Some(clock.enter()?);
                    } else if !waiting {
                        human_pause = None;
                    }
                    if clock.paused_elapsed() >= Duration::from_secs(600) {
                        break Err(grapt_mcp_client::HttpError::TimedOut);
                    }
                }
            }
        }
    }
    .await;
    drop(input);
    drop(output);
    let kill = !matches!(result, Ok(Ok(_)));
    match workers.reap(&call, kill).await {
        None => {
            return Err(failure(
                ToolFailureReason::EffectUnknown,
                "CodeMode worker settlement is unknown",
            ))
        }
        Some(false) if !kill => {
            return Err(failure(
                ToolFailureReason::EffectFailed,
                "CodeMode worker exited unsuccessfully",
            ))
        }
        Some(_) => {}
    }
    match result {
        Ok(Ok(GuestResult::Completed { value })) => {
            let encoded = match value {
                None => Ok(b"undefined".to_vec()),
                Some(serde_json::Value::String(text))
                    if text.len() <= grapt_codemode::OUTPUT_BYTES =>
                {
                    Ok(text.into_bytes())
                }
                Some(value) => grapt_codemode::bounded_json(&value, grapt_codemode::OUTPUT_BYTES),
            };
            encoded.map_err(|_| {
                failure(
                    ToolFailureReason::CaptureFailed,
                    "CodeMode result exceeded its output bound",
                )
            })
        }
        Ok(Err(reason)) => Err(failure(reason, "CodeMode host exchange failed")),
        Ok(Ok(GuestResult::Failed { reason: grapt_codemode::GuestFailure::ApprovalUnavailable, .. })) => Err(failure(
            ToolFailureReason::ApprovalUnavailable,
            "approval unavailable (headless): no matching approver is attached; no effect ran for the refused call",
        )),
        Ok(Ok(GuestResult::Failed { reason, detail, nested_failure })) => Err(failure(
            ToolFailureReason::EffectFailed,
            crate::dogfood::tools::codemode_failure::capture(reason, nested_failure, detail.as_deref()),
        )),
        Err(_) => Err(failure(
            ToolFailureReason::EffectUnknown,
            "CodeMode execution timed out",
        )),
    }
}

fn failure(reason: ToolFailureReason, detail: impl Into<String>) -> mcp_call::EffectFailure {
    mcp_call::EffectFailure {
        reason,
        detail: detail.into(),
    }
}

async fn exchange(
    request: &ExecuteRequest,
    nested: &grapt_runtime::NestedTools,
    ids: &OsRandomIds,
    store: &Arc<FilesystemArtifactStore>,
    jobs: &Jobs,
    startup: &crate::dogfood::mcp::nested_startup::NestedStartups,
    pipes: (&mut ChildStdin, &mut ChildStdout),
) -> Result<GuestResult, ToolFailureReason> {
    let (input, output) = pipes;
    write(input, request).await?;
    let mut children = jobs.lock().await;
    let mut seen = std::collections::BTreeSet::new();
    let (mut files, mut bytes) = (0usize, 0u64);
    loop {
        // Retain an in-progress framed read while child completions are sent;
        // cancelling read_exact midway would lose the frame boundary.
        let reading = read(output);
        tokio::pin!(reading);
        let message = loop {
            tokio::select! {
                message = &mut reading => break message?,
                result = children.join_next(), if !children.is_empty() => {
                    let reply = result.ok_or(ToolFailureReason::EffectUnknown)?
                        .map_err(|_| ToolFailureReason::EffectUnknown)?;
                    files = files.saturating_add(reply.files);
                    bytes = bytes.saturating_add(reply.bytes);
                    if files > 32 || bytes > 512 * 1024 { return Err(ToolFailureReason::CaptureFailed); }
                    write(input, &reply.reply).await?;
                }
            }
        };
        match message {
            WorkerMessage::Finished { result } => {
                if !children.is_empty() {
                    return Err(ToolFailureReason::EffectUnknown);
                }
                return Ok(result);
            }
            WorkerMessage::Call { call } => {
                if !admit_call(request, &call, children.len(), &mut seen) {
                    return Err(ToolFailureReason::EffectUnknown);
                }
                let bytes =
                    grapt_codemode::bounded_json(&call.arguments, grapt_codemode::ARGUMENT_BYTES)
                        .map_err(|_| ToolFailureReason::EffectFailed)?;
                let schema = request
                    .tools
                    .iter()
                    .find(|tool| tool.name == call.tool)
                    .ok_or(ToolFailureReason::EffectUnknown)?;
                if let Err(error) =
                    super::super::schema::validate(&schema.input_schema.to_string(), &bytes)
                {
                    write(
                        input,
                        &HostReply {
                            ordinal: call.ordinal,
                            result: HostResult::Failed {
                                reason: HostFailure::InvalidArguments,
                                message: Some(format!(
                                    "tool input does not match its published schema: {error}"
                                )),
                            },
                        },
                    )
                    .await?;
                    continue;
                }
                let child = ToolCallId::new(format!("call_{}", ids.mint().as_str()))
                    .ok_or(ToolFailureReason::EffectFailed)?;
                let bounded =
                    BoundedToolInput::new(bytes).map_err(|_| ToolFailureReason::EffectFailed)?;
                let nested = nested.clone();
                let store = Arc::clone(store);
                let running = startup
                    .observe(async move {
                        let (result, files, bytes) =
                            match nested.call(child, call.tool, call.catalog, bounded).await {
                                Ok(outcome) => host_result(outcome, &store),
                                Err(_) => (
                                    HostResult::Failed {
                                        reason: HostFailure::ResultUnknown,
                                        message: None,
                                    },
                                    0,
                                    0,
                                ),
                            };
                        HostCompletion {
                            reply: HostReply {
                                ordinal: call.ordinal,
                                result,
                            },
                            files,
                            bytes,
                        }
                    })
                    .map_err(|_| ToolFailureReason::EffectFailed)?;
                children.spawn(running);
            }
        }
    }
}

fn admit_call(
    request: &ExecuteRequest,
    call: &grapt_codemode::HostCall,
    in_flight: usize,
    seen: &mut std::collections::BTreeSet<u16>,
) -> bool {
    call.catalog == request.catalog
        && request.tools.iter().any(|tool| tool.name == call.tool)
        && call.arguments.is_object()
        && in_flight < grapt_codemode::IN_FLIGHT
        && seen.len() < grapt_codemode::HOST_CALLS
        && seen.insert(call.ordinal)
}

fn host_result(
    outcome: ToolTerminalOutcome,
    store: &FilesystemArtifactStore,
) -> (HostResult, usize, u64) {
    let message = match &outcome {
        ToolTerminalOutcome::Failed {
            detail: Some(detail),
            ..
        } => failure_text(detail, store),
        _ => None,
    };
    let result = match outcome {
        ToolTerminalOutcome::Completed {
            output: Some(output),
        } => read_machine(&output, store),
        ToolTerminalOutcome::Completed { output: None } => Ok((serde_json::Value::Null, 0, 0)),
        ToolTerminalOutcome::Failed {
            reason: ToolFailureReason::ApprovalUnavailable,
            ..
        } => Err(HostFailure::ApprovalUnavailable),
        ToolTerminalOutcome::Failed {
            reason: ToolFailureReason::AuthorityUnavailable,
            ..
        } => Err(HostFailure::AuthorityUnavailable),
        ToolTerminalOutcome::Failed {
            reason: ToolFailureReason::EffectUnknown,
            ..
        } => Err(message
            .as_deref()
            .and_then(super::super::mcp_failure::nested)
            .unwrap_or(HostFailure::ResultUnknown)),
        ToolTerminalOutcome::ResultMissing => Err(HostFailure::ResultUnknown),
        ToolTerminalOutcome::Failed { .. } => Err(message
            .as_deref()
            .and_then(super::super::mcp_failure::nested)
            .unwrap_or(HostFailure::EffectFailed)),
        ToolTerminalOutcome::Cancelled { .. } => Err(HostFailure::Cancelled),
    };
    match result {
        Ok((value, files, bytes)) => (HostResult::Completed { value }, files, bytes),
        Err(reason) => (HostResult::Failed { reason, message }, 0, 0),
    }
}

/// A nested tool's captured failure text, at most [`grapt_codemode::DETAIL_BYTES`].
fn failure_text(
    detail: &grapt_types::ToolTerminalArtifact,
    store: &FilesystemArtifactStore,
) -> Option<String> {
    // Verify one complete artifact chunk before selecting the guest prefix.
    let budget = ReadBudget::new(65_536)?;
    let page = store.fetch(detail.reference(), 0, budget).ok()?;
    grapt_codemode::bounded_detail(&String::from_utf8_lossy(page.bytes()))
}

fn read_machine(
    output: &grapt_types::ToolTerminalArtifact,
    store: &FilesystemArtifactStore,
) -> Result<(serde_json::Value, usize, u64), HostFailure> {
    if !output.disposition().is_complete()
        || output.reference().length() > grapt_codemode::ARGUMENT_BYTES as u64
    {
        return Err(HostFailure::EffectFailed);
    }
    let budget =
        ReadBudget::new(grapt_codemode::ARGUMENT_BYTES as u64).ok_or(HostFailure::EffectFailed)?;
    let mut bytes = Vec::new();
    let mut offset = 0;
    loop {
        let page = store
            .fetch(output.reference(), offset, budget)
            .map_err(|_| HostFailure::EffectFailed)?;
        if bytes.len().saturating_add(page.bytes().len()) > grapt_codemode::ARGUMENT_BYTES {
            return Err(HostFailure::EffectFailed);
        }
        bytes.extend_from_slice(page.bytes());
        match page.next_offset() {
            Some(next) if next > offset => offset = next,
            None => break,
            _ => return Err(HostFailure::EffectFailed),
        }
    }
    let envelope: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| HostFailure::EffectFailed)?;
    let files = envelope
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .ok_or(HostFailure::EffectFailed)?;
    let mut total = 0u64;
    for file in files {
        let length = file
            .get("length")
            .and_then(serde_json::Value::as_u64)
            .ok_or(HostFailure::EffectFailed)?;
        if length > 64 * 1024 {
            return Err(HostFailure::EffectFailed);
        }
        total = total.saturating_add(length);
    }
    if files.len() > 32 || total > 512 * 1024 {
        return Err(HostFailure::EffectFailed);
    }
    let value = envelope
        .get("value")
        .cloned()
        .ok_or(HostFailure::EffectFailed)?;
    Ok((value, files.len(), total))
}

async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<WorkerMessage, ToolFailureReason> {
    let mut prefix = [0; 4];
    reader
        .read_exact(&mut prefix)
        .await
        .map_err(|_| ToolFailureReason::EffectUnknown)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > grapt_codemode::FRAME_BYTES {
        return Err(ToolFailureReason::EffectFailed);
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|_| ToolFailureReason::EffectUnknown)?;
    serde_json::from_slice(&bytes).map_err(|_| ToolFailureReason::EffectFailed)
}

async fn write<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), ToolFailureReason> {
    let bytes = grapt_codemode::bounded_json(value, grapt_codemode::FRAME_BYTES)
        .map_err(|_| ToolFailureReason::EffectFailed)?;
    let length = u32::try_from(bytes.len()).map_err(|_| ToolFailureReason::EffectFailed)?;
    writer
        .write_all(&length.to_le_bytes())
        .await
        .map_err(|_| ToolFailureReason::EffectUnknown)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| ToolFailureReason::EffectUnknown)?;
    writer
        .flush()
        .await
        .map_err(|_| ToolFailureReason::EffectUnknown)
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn worker_calls_require_exact_catalog_name_shape_and_remaining_capacity() -> Result<(), String>
    {
        let tool = grapt_types::ToolDefinitionId::new("echo").ok_or("tool")?;
        let catalog = grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?;
        let request = ExecuteRequest {
            code: String::new(),
            catalog: catalog.clone(),
            tools: vec![grapt_codemode::CatalogTool {
                output_schema: None,
                name: tool.clone(),
                description: String::new(),
                input_schema: serde_json::json!({"type":"object"}),
            }],
        };
        let call = grapt_codemode::HostCall {
            ordinal: 0,
            catalog,
            tool,
            arguments: serde_json::json!({}),
        };
        let mut seen = std::collections::BTreeSet::new();
        assert!(admit_call(&request, &call, 7, &mut seen));
        assert!(
            !admit_call(&request, &call, 0, &mut seen),
            "duplicate ordinal replayed"
        );
        for invalid in [
            grapt_codemode::HostCall {
                catalog: grapt_types::ToolSchemaVersion::new("stale").ok_or("stale")?,
                ..call.clone()
            },
            grapt_codemode::HostCall {
                tool: grapt_types::ToolDefinitionId::new("foreign").ok_or("foreign")?,
                ..call.clone()
            },
            grapt_codemode::HostCall {
                arguments: serde_json::json!([]),
                ..call.clone()
            },
        ] {
            let mut fresh = std::collections::BTreeSet::new();
            assert!(!admit_call(&request, &invalid, 0, &mut fresh));
            assert!(fresh.is_empty(), "refused call consumed an ordinal");
        }
        assert!(!admit_call(
            &request,
            &call,
            8,
            &mut std::collections::BTreeSet::new()
        ));
        let mut full: std::collections::BTreeSet<_> = (1..=64).collect();
        assert!(!admit_call(&request, &call, 0, &mut full));
        assert_eq!(full.len(), 64);
        Ok(())
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;

    #[test]
    fn tool01a_guest_detail_follows_the_typed_cause() {
        use grapt_codemode::GuestFailure;
        for (reason, detail, expected) in [
            (
                GuestFailure::GuestError,
                Some("Error: boom"),
                "CodeMode execution failed: GuestError: Error: boom",
            ),
            (
                GuestFailure::JobLimit,
                None,
                "CodeMode execution failed: JobLimit",
            ),
            (
                GuestFailure::GuestError,
                Some(" \u{7} "),
                "CodeMode execution failed: GuestError",
            ),
        ] {
            let captured = crate::dogfood::tools::codemode_failure::capture(reason, None, detail);
            assert!(captured.starts_with("CodeMode cause: "));
            assert!(captured.ends_with(expected));
        }
    }

    #[test]
    fn tool01a_a_nested_failure_hands_its_captured_text_to_the_guest() -> Result<(), String> {
        let root = std::env::temp_dir().join(format!(
            "grapt-tool01a-nested-detail-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store = FilesystemArtifactStore::open(
            root.join("artifacts"),
            b"tool01a-nested-detail".to_vec(),
            Box::new(grapt_store::SystemClock),
        )
        .map_err(|error| error.to_string())?;
        let outcome = super::super::super::failure_detail::captured_failure(
            &store,
            ToolFailureReason::EffectFailed,
            &format!("permission denied: {}", "y".repeat(8 * 1024)),
        );
        let (result, files, bytes) = host_result(outcome, &store);
        assert_eq!((files, bytes), (0, 0));
        match result {
            HostResult::Failed {
                reason: HostFailure::EffectFailed,
                message: Some(message),
            } => {
                assert!(message.starts_with("permission denied: yyy"));
                assert!(message.len() <= grapt_codemode::DETAIL_BYTES);
            }
            other => return Err(format!("nested failure lost its text: {other:?}")),
        }
        for cause in [
            HostFailure::McpProtocol,
            HostFailure::ProducerVersionRefused,
        ] {
            let outcome = super::super::super::failure_detail::captured_failure(
                &store,
                ToolFailureReason::EffectUnknown,
                &super::super::mcp_failure::capture(cause, "PRIVATE_PROTOCOL_TOKEN", true),
            );
            let (result, _, _) = host_result(outcome, &store);
            assert!(matches!(result, HostResult::Failed { reason, .. } if reason == cause));
        }
        let (result, _, _) = host_result(
            ToolTerminalOutcome::Failed {
                reason: ToolFailureReason::EffectFailed,
                detail: None,
            },
            &store,
        );
        assert!(matches!(
            result,
            HostResult::Failed {
                reason: HostFailure::EffectFailed,
                message: None
            }
        ));
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}

#[cfg(test)]
mod settle_tests {
    use super::*;

    fn soon() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_millis(60)
    }

    #[tokio::test]
    async fn a_tree_not_observed_empty_is_never_reported_settled() {
        let survivors = confirm_empty(Some(1), soon(), || Some(1)).await;
        assert!(survivors.is_err_and(|reason| reason.contains("1 process tree members")));
        let unreadable = confirm_empty(None, soon(), || None).await;
        assert!(unreadable.is_err_and(|reason| reason.contains("unreadable")));
    }

    #[tokio::test]
    async fn a_tree_that_empties_before_the_deadline_settles() {
        let mut reads = [Some(2), None, Some(0)].into_iter();
        let drained = confirm_empty(Some(3), soon(), || reads.next().flatten()).await;
        assert_eq!(drained, Ok(()));
    }

    /// The waits shrink toward the deadline, so a tree that empties inside
    /// the final poll interval still settles.
    #[tokio::test(start_paused = true)]
    async fn emptying_inside_the_last_interval_settles() {
        let start = tokio::time::Instant::now();
        let deadline = start + Duration::from_millis(17);
        let empty_at = start + Duration::from_millis(13);
        let drained = confirm_empty(Some(1), deadline, || {
            Some(usize::from(tokio::time::Instant::now() < empty_at))
        })
        .await;
        assert_eq!(drained, Ok(()));
    }

    /// A wake that lands past the deadline never confirms, even if the tree
    /// would by then read empty.
    #[tokio::test(start_paused = true)]
    async fn a_wake_past_the_deadline_never_confirms() {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(17);
        // The poll's wake is delayed past the deadline by a stalled runtime.
        let late = confirm_empty(Some(1), deadline, || Some(0));
        let stall = tokio::time::advance(Duration::from_millis(30));
        let (late, ()) = tokio::join!(late, stall);
        assert!(
            late.is_err(),
            "a late wake confirmed an empty tree: {late:?}"
        );
    }
}

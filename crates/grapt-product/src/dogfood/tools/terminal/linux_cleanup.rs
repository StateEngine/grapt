//! Linux cleanup while the unreaped leader reserves its group and own session.
use rustix::process::{pidfd_open, pidfd_send_signal, Pid, PidfdFlags, Signal};
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStrExt;
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Semaphore, SemaphorePermit, TryAcquireError};
use tokio::time::Instant;

const MAX_CENSUS_ENTRIES: usize = 65_536;
const MAX_STAT_BYTES: u64 = 8192;
static CENSUS_SLOTS: Semaphore = Semaphore::const_new(4);

pub(super) struct Observation {
    result: oneshot::Receiver<Result<bool, String>>,
    signals: Arc<Mutex<bool>>,
}

impl Observation {
    pub(super) fn poll(&mut self, deadline: Instant) -> Result<Option<bool>, String> {
        match self.result.try_recv() {
            Ok(result) => result.map(Some),
            Err(oneshot::error::TryRecvError::Empty) => {
                check_deadline(deadline)?;
                Ok(None)
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                Err("terminal process-group census worker did not return a result".into())
            }
        }
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        // Fence the short pidfd signal operation, never procfs I/O. Even a
        // queued or blocked worker cannot signal after its owner may reap.
        *super::lock(&self.signals) = false;
    }
}

/// Observe the original group and, when the leader owns its session, terminate
/// job-control groups in that session too. Never include a shared host session.
/// The caller keeps the leader unreaped until the observation completes or is
/// dropped; dropping it revokes the worker's signal permission before reaping.
pub(in crate::dogfood) async fn group_running(
    group: u32,
    deadline: Instant,
) -> Result<bool, String> {
    let permit = tokio::time::timeout_at(deadline, CENSUS_SLOTS.acquire())
        .await
        .map_err(|_| "terminal process-group census capacity deadline exceeded")?
        .map_err(|_| "terminal process-group census capacity is unavailable")?;
    let mut observation = launch(group, deadline, permit);
    tokio::time::timeout_at(deadline, &mut observation.result)
        .await
        .map_err(|_| "terminal process-group cleanup exceeded its observation deadline")?
        .map_err(|_| "terminal process-group census worker did not return a result")?
}

/// The synchronous terminal control only polls a bounded result channel. It
/// neither scans procfs under its mutex nor queues blocking tasks without a slot.
pub(super) fn start(group: u32, deadline: Instant) -> Result<Option<Observation>, String> {
    check_deadline(deadline)?;
    match CENSUS_SLOTS.try_acquire() {
        Ok(permit) => Ok(Some(launch(group, deadline, permit))),
        Err(TryAcquireError::NoPermits) => Ok(None),
        Err(TryAcquireError::Closed) => {
            Err("terminal process-group census capacity is unavailable".into())
        }
    }
}

fn launch(group: u32, deadline: Instant, permit: SemaphorePermit<'static>) -> Observation {
    let (sender, receiver) = oneshot::channel();
    let signals = Arc::new(Mutex::new(true));
    let worker_signals = Arc::clone(&signals);
    let _worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = census(group, deadline, &worker_signals);
        // No observation or signal occurs after publishing the result, so the
        // owner can safely reap its reserved leader once it receives success.
        let _sent = sender.send(result);
    });
    Observation {
        result: receiver,
        signals,
    }
}

fn census(group: u32, deadline: Instant, signals: &Mutex<bool>) -> Result<bool, String> {
    check_deadline(deadline)?;
    let group = i32::try_from(group)
        .map_err(|_| "terminal process group exceeds platform identity bounds")?;
    if group <= 1 {
        return Err("terminal group observation requires a private positive group".into());
    }
    let leader_path = std::path::PathBuf::from(format!("/proc/{group}/stat"));
    let reserved = record(&leader_path, group)?
        .ok_or("terminal process-group census omitted the reserved leader")?;
    if reserved.group != group {
        return Err("terminal process-group leader no longer owns its reserved group".into());
    }
    let owns_session = reserved.session == group;
    let directory = std::fs::read_dir("/proc")
        .map_err(|error| format!("terminal process-group census unavailable: {error}"))?;
    let mut leader = false;
    let mut running = false;
    for (index, entry) in directory.enumerate() {
        check_deadline(deadline)?;
        if index >= MAX_CENSUS_ENTRIES {
            return Err("terminal process-group census exceeds 65536 entries".into());
        }
        let entry = entry
            .map_err(|error| format!("terminal process-group census entry failed: {error}"))?;
        let name = entry.file_name();
        let name = name.as_bytes();
        if name.is_empty() || !name.iter().all(u8::is_ascii_digit) {
            continue;
        }
        let pid = number(name)?;
        let path = entry.path().join("stat");
        let Some(task) = record(&path, pid)? else {
            continue;
        };
        if pid == group {
            if task.group != group || task.session != reserved.session {
                return Err("terminal process-group reserved leader identity changed".into());
            }
            leader = true;
        }
        if (task.group == group || (owns_session && task.session == group)) && task.running() {
            running = true;
            if owns_session && task.group != group {
                terminate_session_member(&path, pid, group, deadline, signals)?;
            }
        }
    }
    if !leader {
        return Err("terminal process-group census omitted the reserved leader".into());
    }
    check_deadline(deadline)?;
    Ok(running)
}

fn check_deadline(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err("terminal process-group cleanup exceeded its observation deadline".into());
    }
    Ok(())
}

fn terminate_session_member(
    path: &std::path::Path,
    pid: i32,
    session: i32,
    deadline: Instant,
    signals: &Mutex<bool>,
) -> Result<(), String> {
    check_deadline(deadline)?;
    let identity = Pid::from_raw(pid).ok_or("terminal session member has an invalid PID")?;
    let handle = match pidfd_open(identity, PidfdFlags::empty()) {
        Ok(handle) => handle,
        Err(rustix::io::Errno::SRCH) => return Ok(()),
        Err(error) => {
            return Err(format!(
                "terminal session member PID {pid} could not be pinned for cleanup: {error}"
            ));
        }
    };
    // Open the pidfd BEFORE checking membership again. If the numeric PID is
    // reused during this read, the handle can only signal the old, dead task.
    // Never signal a scanned PGID: its leader need not be ours to reap/reserve.
    let Some(task) = record(path, pid)? else {
        return Ok(());
    };
    if task.session == session && task.running() {
        let enabled = super::lock(signals);
        if !*enabled {
            return Err("terminal process-group census signal permission was revoked".into());
        }
        check_deadline(deadline)?;
        match pidfd_send_signal(&handle, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => (),
            Err(error) => {
                return Err(format!(
                    "terminal session member PID {pid} termination failed: {error}"
                ));
            }
        }
    }
    Ok(())
}

fn record(path: &std::path::Path, pid: i32) -> Result<Option<Task>, String> {
    let bytes = match stat(path) {
        Ok(bytes) => bytes,
        // Other processes may disappear during a census. The reserved
        // leader cannot: absence is rejected by the final leader check.
        Err(error)
            if error.kind() == ErrorKind::NotFound
                || error.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "terminal process-group stat for PID {pid} could not be read: {error}"
            ));
        }
    };
    if bytes.len() as u64 > MAX_STAT_BYTES {
        return Err("terminal process-group stat exceeds 8192 bytes".into());
    }
    let task = parse(&bytes)?;
    if task.pid != pid {
        return Err("terminal process-group stat identity does not match its entry".into());
    }
    Ok(Some(task))
}

fn stat(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_STAT_BYTES + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn number(bytes: &[u8]) -> Result<i32, String> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return Err("terminal process-group stat contains an invalid identity".into());
    }
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse::<i32>().ok())
        .ok_or_else(|| "terminal process-group stat contains an invalid identity".into())
}

struct Task {
    pid: i32,
    group: i32,
    session: i32,
    state: u8,
    threads: i32,
}

impl Task {
    fn running(&self) -> bool {
        // A zombie main thread alone does not prove that the rest of
        // its thread group stopped. Stopped tasks can also resume.
        !matches!(self.state, b'Z' | b'X' | b'x') || self.threads != 1
    }
}

fn parse(bytes: &[u8]) -> Result<Task, String> {
    // comm can contain spaces, parentheses and non-UTF-8 bytes. The last ')'
    // ends it; state, parent PID, process group and session follow.
    let start = bytes
        .iter()
        .position(|byte| *byte == b'(')
        .ok_or("terminal process-group stat has no command delimiter")?;
    let end = bytes
        .iter()
        .rposition(|byte| *byte == b')')
        .filter(|end| *end > start)
        .ok_or("terminal process-group stat has no closing command delimiter")?;
    if start == 0 || bytes.get(start - 1) != Some(&b' ') || bytes.get(end + 1) != Some(&b' ') {
        return Err("terminal process-group stat has malformed command delimiters".into());
    }
    let pid = number(bytes[..start].trim_ascii())?;
    let mut fields = bytes[end + 1..]
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let state = fields
        .next()
        .and_then(|state| match state {
            [state] => Some(*state),
            _ => None,
        })
        .filter(|state| b"RSDZTtXxKWPI".contains(state))
        .ok_or("terminal process-group stat has an invalid task state")?;
    let _parent = number(
        fields
            .next()
            .ok_or("terminal process-group stat has no parent")?,
    )?;
    let group = number(
        fields
            .next()
            .ok_or("terminal process-group stat has no group")?,
    )?;
    let session = number(
        fields
            .next()
            .ok_or("terminal process-group stat has no session")?,
    )?;
    // num_threads is field 20; the next unconsumed field is tty_nr (7).
    let threads = number(
        fields
            .nth(13)
            .ok_or("terminal process-group stat has no thread count")?,
    )?;
    if threads == 0 {
        return Err("terminal process-group stat has an invalid thread count".into());
    }
    Ok(Task {
        pid,
        group,
        session,
        state,
        threads,
    })
}

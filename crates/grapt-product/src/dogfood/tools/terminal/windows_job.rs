//! Windows shell lifetime: every descendant dies with the job.
//!
//! Copyright 2025 OpenAI.
//! Apache-2.0; modified by Grapt: safe pinned adapters and bounded exit census.
//! Every descendant stays in a kill-on-close job. Breakaway is never allowed,
//! so a descendant asking for CREATE_BREAKAWAY_FROM_JOB is refused at creation
//! rather than outliving the command.
//! Both handles below refer to the SAME job: the spawner assigns before resume,
//! and the observer confirms that the contained tree has emptied.
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::sync::Arc;
use std::time::Duration;
use windows_spawn::{Command, CreationFlags, DropPolicy, Job, SpawnOptions};

use super::process::SpawnFailure;
use super::stop::CONFIRM_BOUND;
use super::windows_diagnostics::Context;
const CONFIRM_POLL: Duration = Duration::from_millis(10);

pub(in crate::dogfood::tools) struct ConfirmedJob {
    job: Job,
    observer: Arc<win32job::Job>,
    msys: Option<super::windows_msys::Observer>,
}

impl ConfirmedJob {
    #[cfg(test)]
    pub fn create() -> Result<Self, String> {
        Self::create_inner()
    }

    pub(in crate::dogfood::tools) fn create_observed(
        context: &Context,
    ) -> Result<Self, SpawnFailure> {
        let capture = windows_spawn::diagnostics::Capture::new();
        Self::create_inner().map_err(|detail| {
            let mut failure = SpawnFailure::from(detail);
            failure.diagnostic = Some(context.failure(
                capture.failure(),
                None,
                serde_json::json!({"phase":"setup_failed","requested_policy":"kill_on_close"}),
            ));
            failure
        })
    }

    fn create_inner() -> Result<Self, String> {
        let mut limits = win32job::ExtendedLimitInfo::new();
        limits.limit_kill_on_job_close();
        let observer = win32job::Job::create_with_limit_info(&limits).map_err(job_error)?;
        // Duplicate a borrowed job handle through a safe adapter; ownership stays
        // explicit and neither duplicate is inheritable. Never treat it as I/O.
        let file = filedescriptor::FileDescriptor::dup(&BorrowedJob(&observer))
            .and_then(|handle| handle.as_file())
            .map_err(|error| {
                use std::error::Error as _;
                if let Some(source) = error
                    .source()
                    .and_then(|source| source.downcast_ref::<std::io::Error>())
                {
                    windows_spawn::diagnostics::record("DuplicateHandle(cleanup job)", source);
                }
                format!("terminal cleanup handle unavailable: {error}")
            })?;
        let job = Job::from_handle(file.into())
            .map_err(|error| format!("terminal cleanup job unavailable: {error}"))?;
        Ok(Self {
            job,
            observer: Arc::new(observer),
            msys: None,
        })
    }

    pub fn observe_msys(&mut self) {
        self.msys = Some(super::windows_msys::Observer::start(Arc::downgrade(
            &self.observer,
        )));
    }

    pub fn stop_msys(&self) {
        if let Some(observer) = &self.msys {
            observer.stop();
        }
    }

    pub fn observed_msys(&self) -> bool {
        self.msys
            .as_ref()
            .is_some_and(|observer| observer.observed())
    }

    pub fn msys_observation_requested(&self) -> bool {
        self.msys.is_some()
    }

    /// Start `command` inside the cleanup job, retaining its launch stage.
    /// The pinned adapter returns every creation error before it has resumed the
    /// suspended child. A resume failure is kept uncertain separately.
    #[cfg(test)]
    pub fn spawn(
        &self,
        command: &mut Command,
        flags: CreationFlags,
    ) -> Result<windows_spawn::Child, String> {
        let cwd = match command.get_current_dir() {
            Some(cwd) => cwd.to_path_buf(),
            None => std::env::current_dir()
                .map_err(|error| format!("terminal workdir is unavailable: {error}"))?,
        };
        let context = Context::new(
            std::path::Path::new(command.get_program()),
            None,
            &cwd,
            None,
            "one-shot",
        );
        self.spawn_tracked(command, flags, &context)
            .map_err(|failure| {
                if let Some(record) = &failure.diagnostic {
                    super::windows_diagnostics::log(record);
                }
                failure.detail
            })
    }

    pub(in crate::dogfood::tools) fn spawn_tracked(
        &self,
        command: &mut Command,
        flags: CreationFlags,
        context: &Context,
    ) -> Result<windows_spawn::Child, SpawnFailure> {
        let before = self.diagnostic_state();
        let capture = windows_spawn::diagnostics::Capture::new();
        let options = SpawnOptions::new()
            .job(&self.job)
            .drop_policy(DropPolicy::Detach)
            .creation_flags(flags);
        let suspended = command
            .spawn_suspended_with(options)
            .map_err(|error| {
                let point = capture.failure();
                let call = point.map(|point| point.call).unwrap_or("process preparation");
                let mut failure = SpawnFailure::from(format!(
                    "Windows process creation refused before resume at {call}: {error}; command did not start; safe to retry"
                ));
                failure.diagnostic = Some(context.failure(point, error.raw_os_error(),
                    serde_json::json!({"before":before,"after":self.diagnostic_state()})));
                failure
            })?;
        suspended
            .resume()
            .map_err(|error| {
                let mut failure = SpawnFailure::uncertain(format!(
                    "Windows process resume failed: {error}; a suspended process was created, and the shell may have started during resume"
                ));
                failure.diagnostic = Some(context.failure(capture.failure(), error.raw_os_error(),
                    serde_json::json!({"before":before,"after":self.diagnostic_state()})));
                failure
            })
    }

    fn diagnostic_state(&self) -> serde_json::Value {
        let active = match self.active() {
            Ok(count) => serde_json::json!({"active":count}),
            Err(error) => serde_json::json!({"unreadable":error}),
        };
        serde_json::json!({"policy":"kill_on_close","members":active})
    }

    pub fn terminate(&self, exit_code: u32) -> Result<(), String> {
        self.stop_msys();
        self.job
            .terminate(exit_code)
            .map_err(|error| format!("terminal job cleanup failed: {error}"))
    }

    /// True once no process remains in the job.
    pub fn exited(&self) -> Result<bool, String> {
        self.active().map(|count| count == 0)
    }

    /// How many processes remain in the job.
    pub fn active(&self) -> Result<usize, String> {
        // The adapter refuses an inventory larger than 1025 entries; an
        // unreadable or oversized inventory never means empty.
        self.observer
            .query_process_id_list()
            .map(|members| members.len())
            .map_err(|error| format!("terminal job exit observation failed: {error}"))
    }

    /// Terminate, then wait at most [`CONFIRM_BOUND`] for the job to empty.
    /// Blocking: for the shim's own thread, never an async worker.
    pub fn terminate_and_confirm(&self, exit_code: u32) -> Result<(), String> {
        self.terminate(exit_code)?;
        let deadline = tokio::time::Instant::now() + CONFIRM_BOUND;
        loop {
            if self.exited()? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(unconfirmed(self.active().unwrap_or(0)));
            }
            std::thread::sleep(CONFIRM_POLL);
        }
    }
}

impl Drop for ConfirmedJob {
    fn drop(&mut self) {
        self.stop_msys();
    }
}

fn job_error(error: win32job::JobError) -> String {
    let call = match &error {
        win32job::JobError::CreateFailed(_) => "CreateJobObjectW",
        win32job::JobError::SetInfoFailed(_) => "SetInformationJobObject",
        win32job::JobError::GetInfoFailed(_) => "QueryInformationJobObject",
        win32job::JobError::AssignFailed(_) => "AssignProcessToJobObject",
        _ => "win32job",
    };
    let source: std::io::Error = error.into();
    if let Some(raw) = source.raw_os_error() {
        // windows-result converts HRESULTs directly to io::Error. Recover the
        // Win32 code only for the documented HRESULT_FROM_WIN32 facility.
        let bits = u32::from_ne_bytes(raw.to_ne_bytes());
        let code = if bits & 0xffff_0000 == 0x8007_0000 {
            i32::try_from(bits & 0xffff).unwrap_or(raw)
        } else {
            raw
        };
        windows_spawn::diagnostics::record(call, &std::io::Error::from_raw_os_error(code));
    }
    format!("terminal cleanup policy unavailable at {call}: {source}")
}

pub(in crate::dogfood::tools) fn unconfirmed(active: usize) -> String {
    format!(
        "terminal job still had {active} active process(es) {} ms after termination; its capacity stays reserved",
        CONFIRM_BOUND.as_millis()
    )
}

/// Borrows the adapter's live job handle solely for kernel duplication.
struct BorrowedJob<'a>(&'a win32job::Job);
impl AsRawHandle for BorrowedJob<'_> {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.handle() as RawHandle
    }
}

#[cfg(test)]
mod tests;

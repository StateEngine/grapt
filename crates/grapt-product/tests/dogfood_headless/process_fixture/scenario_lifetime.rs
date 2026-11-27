//! EOF ownership and an independent finite deadline for a complete scenario.
use super::*;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use tokio::time::Instant;

const DRAIN: Duration = Duration::from_secs(10);

enum Completion {
    Finished(Instant),
    Panicking,
}

/// The worker owns the child while the scenario uses its HTTP interface. It can
/// retire the service even when the scenario is blocked waiting for a response.
pub(crate) struct Scenario {
    complete: Option<SyncSender<Completion>>,
    worker: Option<thread::JoinHandle<Result<(), String>>>,
}

impl Scenario {
    /// Startup has its own readiness bound. Thereafter the scenario has one
    /// overall allowance around its permission, settlement and transcript work.
    pub(crate) fn spawn(root: &Path, provider: SocketAddr) -> (SocketAddr, Self) {
        let mut child = spawn_controlled(root, provider);
        let address = await_ready(root, &mut child);
        (address, Self::start(child, Duration::from_secs(120)))
    }

    fn start(mut child: ManagedChild, allowance: Duration) -> Self {
        child.preserve_failure_diagnostics();
        let deadline = Instant::now() + allowance;
        let (complete, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let expired = match receiver.recv_timeout(remaining) {
                Ok(Completion::Panicking) => {
                    drop(child);
                    return Ok(());
                }
                Ok(Completion::Finished(at)) => at >= deadline,
                Err(RecvTimeoutError::Timeout) => true,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("fixture scenario lost its completion owner".to_owned());
                }
            };
            if expired {
                eprintln!(
                    "fixture scenario watchdog exceeded {}ms; closing control EOF",
                    allowance.as_millis()
                );
            }
            retire(child)?;
            if expired {
                return Err(format!(
                    "fixture scenario watchdog exceeded {}ms; owned product reaped",
                    allowance.as_millis()
                ));
            }
            Ok(())
        });
        Self {
            complete: Some(complete),
            worker: Some(worker),
        }
    }

    pub(crate) fn finish(mut self) {
        if let Err(error) = self.join() {
            panic!("{error}");
        }
    }

    fn join(&mut self) -> Result<(), String> {
        if let Some(complete) = self.complete.take() {
            let completion = if thread::panicking() {
                Completion::Panicking
            } else {
                Completion::Finished(Instant::now())
            };
            let _ = complete.send(completion);
        }
        self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| "fixture scenario watchdog worker panicked".to_owned())?
        })
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        if let Err(error) = self.join() {
            if thread::panicking() {
                eprintln!("{error}");
            } else {
                panic!("{error}");
            }
        }
    }
}

fn retire(mut child: ManagedChild) -> Result<(), String> {
    if let Some(status) = child
        .try_wait()
        .map_err(|error| format!("fixture scenario status failed: {error}"))?
    {
        return Err(format!(
            "fixture scenario product exited before completion: {status}"
        ));
    }
    drop(child.stdin.take());
    let wait = PollDeadline::within(DRAIN);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("fixture scenario status failed: {error}"))?
        {
            break status;
        }
        if !wait.pending() {
            child
                .kill()
                .map_err(|error| format!("fixture scenario kill failed: {error}"))?;
            child
                .wait()
                .map_err(|error| format!("fixture scenario reap failed: {error}"))?;
            return Err(format!(
                "fixture scenario EOF drain exceeded {wait}; product killed"
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let output = child
        .wait_with_output()
        .map_err(|error| format!("fixture scenario output failed: {error}"))?;
    if !status.success() {
        return Err(format!(
            "fixture scenario product exited {status}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("fixture scenario exit receipt invalid: {error}"))?;
    if receipt["registration_released"] != true {
        return Err(format!(
            "fixture scenario did not retire registration: {receipt}"
        ));
    }
    Ok(())
}

/// A stuck scenario keeps EOF open; the worker must close it independently,
/// reap the real service and fail the scenario even if graceful exit succeeds.
#[test]
#[should_panic(expected = "fixture scenario watchdog exceeded")]
fn a_hung_scenario_is_retired_and_fails() {
    let _journey = process_journey_custody();
    let root = root("scenario-watchdog");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let scenario = Scenario::start(child, Duration::from_millis(100));
    let wait = PollDeadline::within(Duration::from_secs(5));
    assert!(
        wait.poll(Duration::from_millis(10), || {
            TcpStream::connect(address).is_err().then_some(())
        })
        .is_some(),
        "watchdog did not retire the listener independently of scenario completion"
    );
    scenario.finish();
}

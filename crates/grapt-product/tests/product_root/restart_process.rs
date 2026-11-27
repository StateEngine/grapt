//! Readiness diagnostics for the six launches in a form-recovery journey.
use super::*;
use std::process::{Child, ExitStatus};
use tokio::time::Instant;

#[path = "restart_process/output.rs"]
mod output;
use output::Output;

#[derive(Clone, Copy, Debug)]
pub(super) enum Stage {
    AnswerInitial,
    AnswerRecovered,
    AnswerFinal,
    CancelInitial,
    CancelRecovered,
    CancelFinal,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::AnswerInitial => "answer/initial",
            Self::AnswerRecovered => "answer/recovered",
            Self::AnswerFinal => "answer/final",
            Self::CancelInitial => "cancel/initial",
            Self::CancelRecovered => "cancel/recovered",
            Self::CancelFinal => "cancel/final",
        }
    }
}

pub(super) struct StartupTrace {
    journey: &'static str,
    completed: Vec<(Stage, Duration)>,
}

impl StartupTrace {
    pub(super) fn new(journey: &'static str) -> Self {
        Self {
            journey,
            completed: Vec::new(),
        }
    }

    pub(super) fn launch(
        &mut self,
        stage: Stage,
        root: &Path,
        command: Command,
    ) -> (CapturedProcess, serde_json::Value) {
        let started = Instant::now();
        let mut process = CapturedProcess::spawn(command).unwrap_or_else(|error| {
            panic!(
                "{}; launch failed: {error}; stdout tail=unavailable; stderr tail=unavailable",
                self.context(stage, started.elapsed())
            )
        });
        match process.ready(root) {
            Ok(ready) => {
                self.completed.push((stage, started.elapsed()));
                (process, ready)
            }
            Err(reason) => {
                let context = self.context(stage, started.elapsed());
                let cleanup = process.stop();
                let tails = process.tails(cleanup.is_ok());
                panic!("{context}; {reason}; cleanup={cleanup:?}; {tails}");
            }
        }
    }

    fn context(&self, stage: Stage, elapsed: Duration) -> String {
        let completed = self
            .completed
            .iter()
            .map(|(stage, elapsed)| format!("{}={}ms", stage.label(), elapsed.as_millis()))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} startup readiness failed: stage={}, elapsed={}ms; completed stages=[{completed}]",
            self.journey,
            stage.label(),
            elapsed.as_millis()
        )
    }
}

pub(super) struct CapturedProcess {
    child: Child,
    stdout: Option<Output>,
    stderr: Option<Output>,
}

impl CapturedProcess {
    fn spawn(mut command: Command) -> std::io::Result<Self> {
        let mut process = Self {
            child: command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
            stdout: None,
            stderr: None,
        };
        let stdout = process
            .child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("stdout pipe absent"))?;
        let stderr = process
            .child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("stderr pipe absent"))?;
        process.stdout = Some(Output::start(stdout));
        process.stderr = Some(Output::start(stderr));
        Ok(process)
    }

    fn ready(&mut self, root: &Path) -> Result<serde_json::Value, String> {
        // Preserve the existing 500 polls and sleep placement. This change
        // records the failure; it does not alter the startup allowance.
        for _ in 0..500 {
            match fs::read(root.join("serve-ready.json")) {
                Ok(bytes) => {
                    let value: serde_json::Value = serde_json::from_slice(&bytes)
                        .map_err(|error| format!("ready receipt is corrupt: {error}"))?;
                    if value["pid"].as_u64() == Some(u64::from(self.child.id())) {
                        return Ok(value);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("ready receipt is unreadable: {error}")),
            }
            match self.child.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => {
                    return Err(format!("restart exited {status} before readiness"))
                }
                Err(error) => return Err(format!("restart process observation failed: {error}")),
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err("restart did not publish its own ready receipt within 5000 milliseconds".to_owned())
    }

    pub(super) fn clean_stop(&mut self) {
        drop(self.child.stdin.take());
        assert!(self.exit().success(), "clean Product shutdown failed");
    }

    pub(super) fn kill_and_reap(&mut self) {
        self.child
            .kill()
            .unwrap_or_else(|error| panic!("crash injection failed: {error}"));
        self.child
            .wait()
            .unwrap_or_else(|error| panic!("crash reap failed: {error}"));
    }

    pub(super) fn exit(&mut self) -> ExitStatus {
        for _ in 0..500 {
            match self.child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(error) => panic!("exit observation failed: {error}"),
            }
        }
        panic!("product did not exit within five seconds");
    }

    fn stop(&mut self) -> std::io::Result<ExitStatus> {
        if let Ok(Some(status)) = self.child.try_wait() {
            return Ok(status);
        }
        self.child.kill()?;
        self.child.wait()
    }

    fn tails(&mut self, reaped: bool) -> String {
        format!(
            "stdout {}; stderr {}",
            Output::collect(self.stdout.take(), reaped),
            Output::collect(self.stderr.take(), reaped)
        )
    }
}

impl Drop for CapturedProcess {
    fn drop(&mut self) {
        let cleanup = self.stop();
        if self.stdout.is_some() || self.stderr.is_some() {
            let tails = self.tails(cleanup.is_ok());
            if thread::panicking() {
                eprintln!("form-recovery retained output: {tails}");
            }
        }
    }
}

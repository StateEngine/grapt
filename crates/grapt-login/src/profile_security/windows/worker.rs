//! One fixed managed child, owned and joined by its synchronous caller.
use std::{
    io::{self, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use super::super::{request_budget::RequestBudget, windows_protocol as protocol};

pub(super) struct Worker {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    requests: u32,
}

impl Worker {
    pub(super) fn spawn() -> io::Result<Self> {
        let root = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .filter(|root| root.is_absolute())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "SystemRoot must be absolute")
            })?;
        let mut child = Command::new(root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
            .env_clear()
            .env("SystemRoot", &root)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                super::SCRIPT,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let pipes = child.stdin.take().zip(child.stdout.take());
        let Some((input, output)) = pipes else {
            let _killed = child.kill();
            let _joined = child.wait();
            return Err(io::Error::other("profile protection pipes unavailable"));
        };
        Ok(Self {
            child,
            input,
            output,
            requests: 0,
        })
    }

    pub(super) fn exhausted(&self) -> bool {
        self.requests == u32::MAX
    }

    pub(super) fn run(
        &mut self,
        operation: &str,
        bytes: &[u8],
        budget: &mut RequestBudget,
    ) -> io::Result<Vec<u8>> {
        budget.check()?;
        if self.exhausted() {
            return Err(io::Error::other("profile protection request bound reached"));
        }
        self.requests += 1;
        let id = self.requests;
        let request = protocol::request(id, operation, bytes)?;
        let input = &mut self.input;
        let output = &mut self.output;
        let child = &mut self.child;
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || input.write_all(&request));
            let reader = scope.spawn(move || protocol::read(output));
            let status = (|| loop {
                if reader.is_finished() && writer.is_finished() {
                    return child.try_wait();
                }
                if let Some(status) = child.try_wait()? {
                    return Ok(Some(status));
                }
                budget.pause()?;
            })();
            if status.is_err() {
                let _killed = child.kill();
                let _joined = child.wait();
            }
            let written = writer
                .join()
                .map_err(|_| io::Error::other("profile protection writer failed"))?;
            let captured = reader
                .join()
                .map_err(|_| io::Error::other("profile protection reader failed"))?;
            let status = status?;
            written?;
            let result = protocol::response(id, &captured?)?;
            if status.is_some_and(|status| !status.success()) {
                return Err(super::super::child_refusal(b""));
            }
            Ok(result)
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _joined = self.child.wait();
    }
}

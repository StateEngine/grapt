//! The SDK's fixed CLI invocation shares bounded process-tree containment.
use std::{ffi::OsStr, io, process::Output, time::Duration};

#[derive(Debug)]
pub(super) struct Bounded;

#[async_trait::async_trait]
impl azure_identity::Executor for Bounded {
    async fn run(&self, program: &OsStr, args: &[&OsStr]) -> io::Result<Output> {
        let result = processkit::Command::new(program)
            .args(args)
            .timeout(Duration::from_secs(10))
            .output_buffer(processkit::OutputBufferPolicy::fail_loud(1024).with_max_bytes(64 * 1024))
            .output_bytes()
            .await
            .and_then(processkit::ProcessResult::ensure_success)
            .map_err(|_| io::Error::other("Azure CLI invocation did not complete successfully within its time and output bounds"))?;
        // A zero exit was observed above; only successful output reaches the SDK.
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        };
        Ok(Output {
            status,
            stderr: result.stderr().as_bytes().to_vec(),
            stdout: result.into_stdout(),
        })
    }
}

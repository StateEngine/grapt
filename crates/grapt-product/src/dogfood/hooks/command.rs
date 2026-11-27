//! Configured lifecycle command execution and structured refusal output.
//!
//! output_parser}.rs` at e3149ddb11e24edea64999030f218447336667ed.
//! Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
//! Licensed under the Apache License, Version 2.0 (the "License");
//! you may not use this file except in compliance with the License.
//! You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0.
//! Distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
//! KIND, either express or implied. See LICENSE-APACHE for the full license.
//! Modified by Grapt: literal user argv, strict UTF-8, bounded capture,
//! fail-closed errors, and processkit containment on every exit, including success.
//!
//! Production callers retain this work through their session's process owner.
//! POSIX process groups contain lifetime, not a sandbox: a new session can escape.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::config::HookCommand;
use super::helper;
use crate::dogfood::tools::shell_environment::Environment;
use crate::dogfood::tools::ShellControl;

const MAX_STREAM_BYTES: usize = 16 * 1024;

/// Run one already user-allowlisted declaration. The caller supplies a prepared
/// inherit-minus-secrets environment and bounded lifecycle JSON. V2 tool hooks
/// receive the tool input; prompt, provider credentials and transcript stay absent.
/// Decisions are returned to the ordinary preparation/permission owner.
pub(super) async fn run(
    command: &HookCommand,
    environment: &Environment,
    input: &[u8],
) -> Result<super::decision::HookResult, String> {
    run_controlled(command, environment, input, None).await
}

pub(super) async fn run_controlled(
    command: &HookCommand,
    environment: &Environment,
    input: &[u8],
    control: Option<&ShellControl>,
) -> Result<super::decision::HookResult, String> {
    let (status, out, err) = ConfiguredCommand {
        argv: &command.command,
        directory: &command.directory,
        environment,
        input,
        timeout_ms: command.timeout_ms,
        plugin: command.plugin.as_ref(),
        invoked_as: None,
    }
    .run(helper::Control {
        owner: control,
        cancellation: None,
        deadline: tokio::time::Instant::now() + Duration::from_millis(command.timeout_ms),
    })
    .await?;
    interpret(status, &out, &err, command.matcher.is_some())
}

/// Shared bounded configured-command owner; callers interpret their own output.
pub(in crate::dogfood) struct ConfiguredCommand<'a> {
    pub argv: &'a [String],
    pub directory: &'a Path,
    pub environment: &'a Environment,
    pub input: &'a [u8],
    pub timeout_ms: u64,
    pub plugin: Option<&'a super::plugin::Context>,
    pub invoked_as: Option<&'a str>,
}

impl ConfiguredCommand<'_> {
    pub(in crate::dogfood) async fn run(
        self,
        mut control: helper::Control<'_>,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
        let Self {
            argv,
            directory,
            environment,
            input,
            timeout_ms,
            plugin,
            invoked_as,
        } = self;
        if input.len() > MAX_STREAM_BYTES {
            return Err("configured command stdin exceeds 16384 bytes".into());
        }
        std::str::from_utf8(input).map_err(|_| "configured command stdin is not UTF-8")?;
        let (program, arguments) = argv
            .split_first()
            .ok_or("configured command argv is empty")?;
        if !Path::new(program).is_absolute() || !directory.is_absolute() {
            return Err("configured executable and directory must be absolute".into());
        }
        if !(1..=60_000).contains(&timeout_ms) {
            return Err("configured command timeout must be in 1..60000".into());
        }
        control.deadline = control
            .deadline
            .min(tokio::time::Instant::now() + Duration::from_millis(timeout_ms));
        let mut child_command = tokio::process::Command::new(program);
        // Image identity and dispatch name are separate: never re-resolve a shim
        // symlink chain just to preserve argv[0].
        #[cfg(unix)]
        if let Some(name) = invoked_as {
            use std::os::unix::process::CommandExt;
            child_command.as_std_mut().arg0(name);
        }
        #[cfg(not(unix))]
        let _ = invoked_as;
        child_command
            .args(arguments)
            .current_dir(directory)
            .env_clear()
            .envs(environment.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(context) = plugin {
            for (name, value) in context.environment() {
                child_command.env(name, value);
            }
        }
        grapt_service::launcher_environment::scrub(child_command.as_std_mut());
        let (status, (out, err)) =
            helper::run(child_command, control, |stdin, stdout, stderr| async move {
                let write = async {
                    let mut stdin = stdin;
                    // An early exit is legal; BrokenPipe does not turn an exit-2
                    // refusal into an input error. All other writer failures refuse.
                    match stdin.write_all(input).await {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                        Err(_) => Err("hook stdin write failed".to_owned()),
                    }
                    // Drop stdin at the end of this branch to deliver EOF while drains
                    // and wait are still running, not after they complete.
                };
                let ((), out, err) =
                    tokio::try_join!(write, capture(stdout, "stdout"), capture(stderr, "stderr"),)?;
                Ok::<_, String>((out, err))
            })
            .await?;
        Ok((status, out, err))
    }
}

async fn capture(mut stream: impl AsyncRead + Unpin, label: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(MAX_STREAM_BYTES);
    let mut buffer = [0; 4096];
    loop {
        // Read one sentinel byte beyond the cap, never retain it. A complete
        // UTF-8 JSON value at the boundary must not hide a denied/truncated tail.
        let available = buffer.len().min(MAX_STREAM_BYTES - bytes.len() + 1);
        let count = stream
            .read(&mut buffer[..available])
            .await
            .map_err(|_| format!("hook {label} read failed"))?;
        if count == 0 {
            return Ok(bytes);
        }
        if count > MAX_STREAM_BYTES - bytes.len() {
            return Err(format!("hook {label} exceeds 16384 bytes"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn interpret(
    status: ExitStatus,
    out: &[u8],
    err: &[u8],
    controls: bool,
) -> Result<super::decision::HookResult, String> {
    let out = std::str::from_utf8(out).map_err(|_| "hook stdout is not UTF-8")?;
    let err = std::str::from_utf8(err).map_err(|_| "hook stderr is not UTF-8")?;
    match status.code() {
        Some(0) => super::decision::parse(out, controls),
        Some(2) => Err(format!(
            "hook denied (exit 2): {}",
            nonempty(err).unwrap_or("no reason supplied")
        )),
        Some(code) => Err(format!("hook failed with exit code {code}")),
        None => Err("hook terminated without an exit code".into()),
    }
}

fn nonempty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;

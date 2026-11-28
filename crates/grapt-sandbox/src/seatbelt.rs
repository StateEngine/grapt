// Copyright 2025 OpenAI. Apache-2.0.
// . Modified by Grapt: shared
// bounded policy, native readiness and existing caller-owned terminal lifetime.
use super::{ContainedShell, ContainedShellSpawner, ShellRequest, SpawnFailure};
use std::path::PathBuf;

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
mod launch;
#[cfg(target_os = "macos")]
pub(super) mod native;
mod profile;
#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
mod ready;

#[cfg(target_os = "macos")]
pub const INNER_ARGUMENT: &str = "--grapt-seatbelt-exec";

/// Only the immutable system launcher is used in production. The helper is the
/// caller's installed image, dispatched before its runtime/configuration starts.
#[derive(Debug)]
pub struct Seatbelt {
    helper: PathBuf,
    executable: PathBuf,
}

impl Seatbelt {
    pub fn new(helper: PathBuf) -> Self {
        Self {
            helper,
            executable: "/usr/bin/sandbox-exec".into(),
        }
    }

    #[cfg(all(test, not(windows)))]
    pub(super) fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = executable;
        self
    }

    /// Prepare piped protocol execution without a shell or an inner helper.
    /// The caller still owns process-group setup, pipes and the final reap.
    #[cfg(target_os = "macos")]
    pub fn piped_command(
        request: &crate::ProcessRequest,
    ) -> Result<std::process::Command, SpawnFailure> {
        request.validate()?;
        let mut policy = request.policy.clone();
        policy.readable_roots.extend([
            "/System/Library".into(),
            "/usr/lib".into(),
            "/usr/bin/env".into(),
        ]);
        let profile = profile::prepare(&policy).map_err(|reason| refused(reason, false))?;
        let mut command = std::process::Command::new("/usr/bin/sandbox-exec");
        command.env_clear().arg("-p").arg(profile.text);
        for (key, path) in profile.parameters {
            command.arg("-D").arg(format!("{key}={}", path.display()));
        }
        command
            .arg("/usr/bin/env")
            .arg("-i")
            .args(&request.environment)
            .arg(&request.executable)
            .args(&request.arguments)
            .current_dir(&request.working_directory);
        Ok(command)
    }

    /// Called in the single-threaded child only after Seatbelt installed its
    /// policy. No untrusted shell source runs before the readiness receipt.
    #[cfg(target_os = "macos")]
    pub fn inner_main(arguments: &[String]) -> Option<Result<(), String>> {
        use std::io::Write;
        use std::os::fd::AsFd;
        use std::os::unix::process::CommandExt;
        if arguments.first().is_none_or(|arg| arg != INNER_ARGUMENT) {
            return None;
        }
        Some((|| {
            let [_, original_stderr, directory, source] = arguments else {
                return Err("Seatbelt helper arguments are malformed".into());
            };
            if source.is_empty() || source.len() > 65536 || source.contains('\0') {
                return Err("Seatbelt command is malformed".into());
            }
            std::env::set_current_dir(directory)
                .map_err(|error| format!("Seatbelt working directory unavailable: {error}"))?;
            let descriptor = original_stderr
                .parse::<i32>()
                .map_err(|_| "Seatbelt stderr descriptor is malformed")?;
            if descriptor <= 2 {
                return Err("Seatbelt stderr descriptor is not private".into());
            }
            let mut startup =
                std::fs::File::from(std::io::stderr().as_fd().try_clone_to_owned().map_err(
                    |error| format!("Seatbelt startup descriptor unavailable: {error}"),
                )?);
            let original = std::fs::OpenOptions::new()
                .write(true)
                .open(format!("/dev/fd/{descriptor}"))
                .map_err(|error| format!("Seatbelt caller stderr unavailable: {error}"))?;
            nix::unistd::dup2_stderr(&original).map_err(|error| {
                format!("Seatbelt caller stderr could not be restored: {error}")
            })?;
            nix::unistd::close(descriptor).map_err(|error| {
                format!("Seatbelt private stderr descriptor could not close: {error}")
            })?;
            drop(original);
            startup
                .write_all(native::READY)
                .map_err(|error| format!("Seatbelt readiness failed: {error}"))?;
            drop(startup);
            let error = std::process::Command::new("/bin/sh")
                .args(["-c", source])
                .exec();
            Err(format!("Seatbelt shell could not start: {error}"))
        })())
    }
}

fn refused(reason: impl Into<String>, possible: bool) -> SpawnFailure {
    SpawnFailure::for_backend("seatbelt", reason, possible)
}

impl ContainedShellSpawner for Seatbelt {
    fn spawn(&self, request: &ShellRequest) -> Result<ContainedShell, SpawnFailure> {
        self.spawn_with_write_targets(request, &[])
    }

    fn spawn_with_write_targets(
        &self,
        request: &ShellRequest,
        targets: &[crate::WriteTarget],
    ) -> Result<ContainedShell, SpawnFailure> {
        if !targets.is_empty() {
            return Err(refused("Seatbelt write grants are not supported", false));
        }
        request
            .validate()
            .map_err(|reason| refused(reason, false))?;
        if !self.helper.is_absolute() || !self.executable.is_absolute() {
            return Err(refused(
                "Seatbelt needs absolute launcher and helper paths",
                false,
            ));
        }
        let profile = profile::prepare(&request.policy).map_err(|reason| refused(reason, false))?;
        #[cfg(target_os = "macos")]
        {
            native::spawn(&self.executable, &self.helper, request, profile).map(|child| {
                ContainedShell {
                    child,
                    _write_targets: Vec::new(),
                }
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = profile;
            Err(refused(
                "Seatbelt process containment requires macOS",
                false,
            ))
        }
    }
}

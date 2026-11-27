//! The system launcher installs Seatbelt before the trusted image reports ready.
//! Process groups, terminal streams and cancellation stay with the caller.
pub(crate) use super::launch::Child;
use super::{launch, profile::Profile, refused, INNER_ARGUMENT};
use crate::{ShellRequest, SpawnFailure};
use nix::poll::{poll, PollFd, PollFlags};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;

pub(super) const READY: &[u8] = b"GRAPT_SEATBELT_READY\n";

pub(super) fn spawn(
    executable: &Path,
    helper: &Path,
    request: &ShellRequest,
    mut profile: Profile,
) -> Result<Child, SpawnFailure> {
    let original_stderr = std::io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|error| {
            refused(
                format!("Seatbelt caller stderr unavailable: {error}"),
                false,
            )
        })?;
    // Both parent descriptors stay CLOEXEC. The file action installs only the
    // intended child's private descriptor; concurrent execs inherit neither.
    let inherited_stderr = original_stderr.try_clone().map_err(|error| {
        refused(
            format!("Seatbelt stderr destination unavailable: {error}"),
            false,
        )
    })?;
    profile
        .passthrough_stderr(inherited_stderr.as_raw_fd())
        .map_err(|reason| refused(reason, false))?;
    let mut arguments = vec![
        executable.as_os_str().to_owned(),
        "-p".into(),
        profile.text.into(),
    ];
    for (key, path) in profile.parameters {
        arguments.push(format!("-D{key}={}", path.display()).into());
    }
    arguments.extend([
        OsString::from("--"),
        helper.as_os_str().to_owned(),
        INNER_ARGUMENT.into(),
        inherited_stderr.as_raw_fd().to_string().into(),
        request.working_directory.as_os_str().to_owned(),
        request.command_line.clone().into(),
    ]);
    let environment = request
        .environment
        .iter()
        .filter(|entry| {
            let name = entry.split_once('=').map(|(name, _)| name).unwrap_or("");
            !name.starts_with("DYLD_")
                && !name.starts_with("__XPC_DYLD_")
                && !matches!(name, "BASH_ENV" | "ENV")
        })
        .map(OsString::from)
        .collect::<Vec<_>>();
    let (mut process, mut pipe) = launch::spawn(
        executable,
        &arguments,
        &environment,
        &original_stderr,
        &inherited_stderr,
    )
    .map_err(|error| refused(format!("sandbox-exec is unavailable: {error}"), false))?;
    drop(original_stderr);
    drop(inherited_stderr);
    let started = (|| {
        let prefix = ready(&mut pipe, request.timeout_ms)?;
        std::io::stderr().write_all(&prefix).map_err(|error| {
            refused(
                format!("Seatbelt startup diagnostics failed: {error}"),
                true,
            )
        })
    })();
    match started {
        Ok(()) => Ok(process),
        Err(error) => {
            let _kill = process.kill();
            let _wait = process.wait();
            Err(error)
        }
    }
}

fn ready(pipe: &mut std::io::PipeReader, timeout_ms: u32) -> Result<Vec<u8>, SpawnFailure> {
    let mut bytes = Vec::new();
    let interval = u16::try_from((timeout_ms.min(5000) / 32).max(1))
        .map_err(|_| refused("Seatbelt startup bound is invalid", false))?;
    for _ in 0..32 {
        let mut descriptors = [PollFd::new(pipe.as_fd(), PollFlags::POLLIN)];
        match poll(&mut descriptors, interval) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => (),
            Err(error) => {
                return Err(refused(
                    format!("Seatbelt startup observation failed: {error}"),
                    true,
                ))
            }
        }
        let mut chunk = [0; 128];
        let count = pipe
            .read(&mut chunk)
            .map_err(|error| refused(format!("Seatbelt startup read failed: {error}"), true))?;
        bytes.extend_from_slice(&chunk[..count]);
        let position = bytes
            .windows(READY.len())
            .position(|window| window == READY);
        if String::from_utf8_lossy(&bytes[..position.unwrap_or(bytes.len())])
            .to_ascii_lowercase()
            .contains("deprecated")
        {
            return Err(refused(
                "sandbox-exec reports that Seatbelt is deprecated on this host",
                position.is_some(),
            ));
        }
        if let Some(position) = position {
            bytes.drain(position..position + READY.len());
            return Ok(bytes);
        }
        if count == 0 {
            return Err(refused(
                format!(
                    "sandbox-exec did not establish containment: {}",
                    String::from_utf8_lossy(&bytes).trim()
                ),
                false,
            ));
        }
    }
    Err(refused(
        "Seatbelt startup timed out or exceeded 4096 bytes",
        true,
    ))
}

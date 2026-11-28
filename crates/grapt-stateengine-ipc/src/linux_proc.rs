// SPDX-License-Identifier: MIT OR Apache-2.0
//! Process identity on Linux, read from `/proc` without any system call
//! wrapper: the effective uid the kernel reports to a peer, the start time in
//! clock ticks, and the executable's digest. It observes this client for its
//! own binding and the host process for the server check.
use crate::peer::{PeerBinding, Principal};
use crate::wire::StartIdentity;
use crate::{IpcError, Sha256Hex};
use std::path::{Path, PathBuf};

/// This process's own peer binding, read from `/proc/self`, hashing at most
/// `max_image_bytes` of its executable. Other platforms supply theirs.
pub fn observe_self(max_image_bytes: u64) -> Result<PeerBinding, IpcError> {
    let directory = Path::new("/proc/self");
    let status = read(directory, "status")?;
    Ok(PeerBinding {
        principal: Principal::Unix(
            effective_uid(&status).ok_or_else(|| unreadable(directory, "uid"))?,
        ),
        pid: std::process::id(),
        started: StartIdentity::Linux {
            start_ticks: start_ticks(directory)?,
        },
        image_sha256: image(directory, max_image_bytes)?,
    })
}

/// The `/proc` directory of process `pid`.
pub(crate) fn process(pid: u32) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}"))
}

/// Field 22 of `stat`: when the process started, in clock ticks since boot.
pub(crate) fn start_ticks(directory: &Path) -> Result<u64, IpcError> {
    parse_start_ticks(&read(directory, "stat")?).ok_or_else(|| unreadable(directory, "start time"))
}

/// The SHA-256 of the process's executable, at most `max_bytes` of it.
pub(crate) fn image(directory: &Path, max_bytes: u64) -> Result<Sha256Hex, IpcError> {
    crate::binary::sha256_file(&directory.join("exe"), max_bytes)
}

fn read(directory: &Path, name: &str) -> Result<String, IpcError> {
    std::fs::read_to_string(directory.join(name)).map_err(|_| unreadable(directory, name))
}

fn unreadable(directory: &Path, what: &str) -> IpcError {
    IpcError::PeerMismatch(format!("{} {what} is unreadable", directory.display()))
}

/// The second value of the `Uid:` line: real, effective, saved, filesystem.
fn effective_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// Field 22 of `stat`, counted after the parenthesized command name, which
/// may itself hold spaces or parentheses.
fn parse_start_ticks(stat: &str) -> Option<u64> {
    let (_, after_name) = stat.rsplit_once(')')?;
    after_name.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_fields_parse_and_this_process_observes_itself() -> Result<(), IpcError> {
        assert_eq!(effective_uid("Name:\tx\nUid:\t1\t2\t3\t4\n"), Some(2));
        let stat = format!("9 (a) b) S {} 77 0", "0 ".repeat(18));
        assert_eq!(parse_start_ticks(&stat), Some(77));
        let observed = observe_self(1 << 32)?;
        assert_eq!(observed.pid, std::process::id());
        assert_eq!(
            observed.started,
            StartIdentity::Linux {
                start_ticks: start_ticks(&process(std::process::id()))?
            }
        );
        Ok(())
    }
}

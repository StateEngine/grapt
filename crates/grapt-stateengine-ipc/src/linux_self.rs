// SPDX-License-Identifier: MIT OR Apache-2.0
//! This process's own identity on Linux, read from `/proc/self` without any
//! system call wrapper: the effective uid the kernel reports to a peer, the
//! start time in clock ticks, and the executable's digest.
use crate::peer::{PeerBinding, Principal};
use crate::wire::StartIdentity;
use crate::IpcError;

/// This process's own peer binding, read from `/proc/self`, hashing at most
/// `max_image_bytes` of its executable. Other platforms supply theirs.
pub fn observe_self(max_image_bytes: u64) -> Result<PeerBinding, IpcError> {
    let failed = |what: &str| IpcError::Binary(format!("/proc/self {what} is unreadable"));
    let status = std::fs::read_to_string("/proc/self/status").map_err(|_| failed("status"))?;
    let stat = std::fs::read_to_string("/proc/self/stat").map_err(|_| failed("stat"))?;
    Ok(PeerBinding {
        principal: Principal::Unix(effective_uid(&status).ok_or_else(|| failed("uid"))?),
        pid: std::process::id(),
        started: StartIdentity::Linux {
            start_ticks: start_ticks(&stat).ok_or_else(|| failed("start time"))?,
        },
        image_sha256: crate::binary::sha256_file("/proc/self/exe".as_ref(), max_image_bytes)?,
    })
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
fn start_ticks(stat: &str) -> Option<u64> {
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
        assert_eq!(start_ticks(&stat), Some(77));
        let observed = observe_self(1 << 32)?;
        assert_eq!(observed.pid, std::process::id());
        Ok(())
    }
}

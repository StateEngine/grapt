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

/// What identifies a running process instance to the server check.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Instance {
    /// The effective uid.
    pub(crate) uid: u32,
    pub(crate) start_ticks: u64,
    pub(crate) executable: PathBuf,
}

impl Instance {
    /// Whether this observation is the `expected` instance, or which part differs.
    pub(crate) fn matches(&self, expected: &Self) -> Result<(), IpcError> {
        let differs = if self.uid != expected.uid {
            "the server runs as another user"
        } else if self.start_ticks != expected.start_ticks {
            "the server process started at another time"
        } else if self.executable != expected.executable {
            "the server runs another executable than the endpoint names"
        } else {
            return Ok(());
        };
        Err(IpcError::PeerMismatch(differs.into()))
    }
}

/// The user, start time and executable path of the process at `directory`.
pub(crate) fn observe(directory: &Path) -> Result<Instance, IpcError> {
    let status = read(directory, "status")?;
    Ok(Instance {
        uid: effective_uid(&status).ok_or_else(|| unreadable(directory, "uid"))?,
        start_ticks: start_ticks(directory)?,
        executable: std::fs::read_link(directory.join("exe"))
            .map_err(|_| unreadable(directory, "exe"))?,
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
    hashed_image(directory, max_bytes).map(|(digest, _, _)| digest)
}

/// A file's device and inode: which file it is, whatever its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileId {
    device: u64,
    inode: u64,
}

impl FileId {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// [`image`], with the identity of the very file that was opened and hashed
/// and that file, still open: while it is held, no other file can take its
/// device and inode.
pub(crate) fn hashed_image(
    directory: &Path,
    max_bytes: u64,
) -> Result<(Sha256Hex, FileId, std::fs::File), IpcError> {
    let (digest, file) = crate::binary::sha256_opened(&directory.join("exe"), max_bytes)?;
    let metadata = file.metadata().map_err(|_| unreadable(directory, "exe"))?;
    Ok((digest, FileId::of(&metadata), file))
}

/// The identity of the file the process at `directory` runs now.
pub(crate) fn executable_id(directory: &Path) -> Result<FileId, IpcError> {
    std::fs::metadata(directory.join("exe"))
        .map(|metadata| FileId::of(&metadata))
        .map_err(|_| unreadable(directory, "exe"))
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
        let (_, hashed, _open) = hashed_image(Path::new("/proc/self"), 1 << 32)?;
        assert_eq!(executable_id(Path::new("/proc/self"))?, hashed);
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

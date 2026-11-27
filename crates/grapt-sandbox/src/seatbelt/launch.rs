//! Child-only descriptor inheritance. No descriptor is made inheritable in the
//! multithreaded parent, and the caller keeps process-group lifetime ownership.
use nix::errno::Errno;
use nix::fcntl::{fcntl, FcntlArg, FdFlag};
use nix::spawn::{posix_spawn, PosixSpawnAttr, PosixSpawnFileActions};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(super) fn spawn(
    executable: &Path,
    arguments: &[OsString],
    environment: &[OsString],
    original: &OwnedFd,
    destination: &OwnedFd,
) -> io::Result<(Child, io::PipeReader)> {
    for descriptor in [original, destination] {
        let flags = fcntl(descriptor, FcntlArg::F_GETFD)?;
        if descriptor.as_raw_fd() <= 2 || flags & FdFlag::FD_CLOEXEC.bits() == 0 {
            return Err(io::Error::other(
                "Seatbelt parent stderr must remain private and CLOEXEC",
            ));
        }
    }
    if original.as_raw_fd() == destination.as_raw_fd() {
        return Err(io::Error::other(
            "Seatbelt stderr descriptors must be distinct",
        ));
    }
    let (reader, writer) = io::pipe()?;
    if reader.as_raw_fd() <= 2 || writer.as_raw_fd() <= 2 {
        return Err(io::Error::other(
            "Seatbelt needs the caller's standard descriptors",
        ));
    }
    let mut actions = PosixSpawnFileActions::init()?;
    actions.add_dup2(original.as_raw_fd(), destination.as_raw_fd())?;
    actions.add_dup2(writer.as_raw_fd(), 2)?;
    // The source descriptors and reader retain CLOEXEC and close on exec.
    let attributes = PosixSpawnAttr::init()?;
    let strings = |values: &[OsString]| {
        values
            .iter()
            .map(|value| {
                CString::new(value.as_bytes()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "Seatbelt launch contains NUL")
                })
            })
            .collect::<io::Result<Vec<_>>>()
    };
    let arguments = strings(arguments)?;
    let environment = strings(environment)?;
    let pid = posix_spawn(executable, &actions, &attributes, &arguments, &environment)?;
    drop(writer);
    Ok((
        Child {
            pid: Some(pid),
            status: None,
        },
        reader,
    ))
}

pub(crate) struct Child {
    // A PID is retained only while this owner has not reaped or lost the child.
    pid: Option<Pid>,
    status: Option<i32>,
}

impl Child {
    fn observe(&mut self, flags: Option<WaitPidFlag>) -> io::Result<Option<i32>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let pid = self
            .pid
            .ok_or_else(|| io::Error::other("Seatbelt child ownership was lost"))?;
        let status = match waitpid(pid, flags) {
            Ok(WaitStatus::Exited(_, code)) => code,
            Ok(WaitStatus::Signaled(_, signal, _)) => 128 + signal as i32,
            Ok(WaitStatus::StillAlive) => return Ok(None),
            Ok(_) => {
                return Err(io::Error::other(
                    "Seatbelt child returned an unexpected wait state",
                ))
            }
            Err(error) => {
                if error == Errno::ECHILD {
                    self.pid = None;
                }
                return Err(error.into());
            }
        };
        self.pid = None;
        self.status = Some(status);
        Ok(Some(status))
    }

    pub fn wait(&mut self) -> io::Result<i32> {
        for _ in 0..32 {
            match self.observe(None) {
                Ok(Some(status)) => return Ok(status),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
                Ok(None) => {
                    return Err(io::Error::other(
                        "Seatbelt blocking wait returned no status",
                    ))
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Seatbelt wait interrupted 32 times",
        ))
    }

    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.observe(Some(WaitPidFlag::WNOHANG))
    }

    pub fn kill(&mut self) -> io::Result<()> {
        match self.pid {
            Some(pid) => kill(pid, Signal::SIGKILL).map_err(Into::into),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seatbelt child is no longer owned",
            )),
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.pid.is_some() && !matches!(self.try_wait(), Ok(Some(_))) {
            let _kill = self.kill();
            let _wait = self.wait();
        }
    }
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;

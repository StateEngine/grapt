//! Child-only descriptor inheritance for bubblewrap's --bind-fd. Parent pins
//! stay close-on-exec, including while other threads launch unrelated tools.
use crate::WriteTarget;
use nix::fcntl::OFlag;
use nix::spawn::{posix_spawn, PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags};
use nix::sys::signal::{kill, killpg, SigSet, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{pipe2, Pid};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

pub(super) struct Process {
    pid: Option<Pid>,
    exit: Option<i32>,
}

pub(super) fn start(
    command: &Command,
    targets: &[WriteTarget],
    inherited: &[BorrowedFd<'_>],
) -> io::Result<(Process, File)> {
    let (read, write) = pipe2(OFlag::O_CLOEXEC)?;
    let mut actions = PosixSpawnFileActions::init()?;
    actions.add_dup2(write.as_raw_fd(), 2)?;
    for target in targets {
        let fd = target.native_descriptor().as_raw_fd();
        if fd < 3 {
            return Err(io::Error::other(
                "Sandbox grant descriptor overlaps standard I/O",
            ));
        }
        // POSIX adddup2(fd, fd) clears CLOEXEC in this child only. There is no
        // parent flag mutation and no descriptor renumbering/collision window.
        actions.add_dup2(fd, fd)?;
    }
    for fd in inherited {
        actions.add_dup2(fd.as_raw_fd(), fd.as_raw_fd())?;
    }
    Ok((launch(command, &actions)?, File::from(read)))
}

pub(super) fn inherited(command: &Command) -> io::Result<Process> {
    launch(command, &PosixSpawnFileActions::init()?)
}

fn launch(command: &Command, actions: &PosixSpawnFileActions) -> io::Result<Process> {
    let string = |bytes: &[u8]| {
        CString::new(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Sandbox launch contains NUL"))
    };
    let mut arguments = vec![string(command.get_program().as_bytes())?];
    for argument in command.get_args() {
        arguments.push(string(argument.as_bytes())?);
    }
    // The builder was env_clear'ed: only the already filtered explicit entries
    // are passed. Nothing is merged from the native launcher's environment.
    let mut environment = Vec::new();
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            let mut entry = name.as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            environment.push(string(&entry)?);
        }
    }
    let mut attributes = PosixSpawnAttr::init()?;
    attributes.set_sigmask(&SigSet::empty())?;
    attributes.set_pgroup(Pid::from_raw(0))?;
    attributes.set_flags(
        PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK | PosixSpawnFlags::POSIX_SPAWN_SETPGROUP,
    )?;
    let pid = posix_spawn(
        std::path::Path::new(command.get_program()),
        actions,
        &attributes,
        &arguments,
        &environment,
    )?;
    Ok(Process {
        pid: Some(pid),
        exit: None,
    })
}

impl Process {
    pub fn id(&self) -> Option<Pid> {
        self.pid
    }

    pub fn interrupt(&mut self) -> io::Result<()> {
        if let Some(pid) = self.pid {
            match killpg(pid, Signal::SIGINT) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn observe(&mut self, blocking: bool, init: bool) -> io::Result<Option<i32>> {
        if self.exit.is_some() {
            return Ok(self.exit);
        }
        let pid = self
            .pid
            .ok_or_else(|| io::Error::other("Sandbox child exit status was lost"))?;
        let flags = if blocking {
            None
        } else {
            Some(WaitPidFlag::WNOHANG)
        };
        for _ in 0..128 {
            let status = match waitpid(if init { Pid::from_raw(-1) } else { pid }, flags) {
                Err(nix::errno::Errno::EINTR) => continue,
                Err(nix::errno::Errno::ECHILD) => {
                    // Another reaper removed ownership. Never signal this
                    // numeric PID again after it can have been recycled.
                    self.pid = None;
                    return Err(io::Error::other("Sandbox child exit status was lost"));
                }
                status => status?,
            };
            self.exit = match status {
                WaitStatus::Exited(observed, code) if observed == pid => Some(code),
                WaitStatus::Signaled(observed, signal, _) if observed == pid => {
                    Some(128 + signal as i32)
                }
                WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _) if init => continue,
                WaitStatus::StillAlive => None,
                _ => {
                    return Err(io::Error::other(
                        "Sandbox child returned an unexpected wait status",
                    ))
                }
            };
            if self.exit.is_some() {
                self.pid = None;
            }
            return Ok(self.exit);
        }
        if init {
            // One poll reaps a bounded batch. Remaining orphaned descendants
            // are reaped on the next poll without changing the command status.
            Ok(None)
        } else {
            Err(io::Error::other(
                "Sandbox child wait exceeded its interruption bound",
            ))
        }
    }

    pub fn wait(&mut self) -> io::Result<i32> {
        self.observe(true, false)?
            .ok_or_else(|| io::Error::other("Sandbox child did not exit"))
    }
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.observe(false, false)
    }
    pub fn try_wait_init(&mut self) -> io::Result<Option<i32>> {
        if nix::unistd::getpid() != Pid::from_raw(1) {
            return Err(io::Error::other(
                "Sandbox init reaping requires the isolated PID namespace",
            ));
        }
        self.observe(false, true)
    }

    pub fn kill(&mut self) -> io::Result<()> {
        if let Some(pid) = self.pid {
            kill(pid, Signal::SIGKILL)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaped_child_keeps_its_status_and_cancellation_is_inert() -> io::Result<()> {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 23"]).env_clear();
        let (mut child, _stderr) = start(&command, &[], &[])?;
        assert_eq!(child.wait()?, 23);
        assert_eq!(child.wait()?, 23);
        assert_eq!(child.try_wait()?, Some(23));
        child.kill()?;
        Ok(())
    }
}

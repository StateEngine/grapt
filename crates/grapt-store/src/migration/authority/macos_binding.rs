//! Reject pathname substitution while Darwin's VFS opens a retained file.
//!
//! F_GETPATH supplies the retained vnode's current path. A synchronous kqueue
//! watches that vnode and every real ancestor across SQLite open, so rename,
//! unlink and restore leave a pending event even when identity samples match.
//! Any observed event or failed read refuses the connection before callers write.
//!
//! This is an open-window guard, not a directory lock or process sandbox. A
//! same-user process with directory write access can force refusal and can
//! change paths again after this lease ends. It cannot be excluded by Unix
//! advisory locks. The guard relies on Darwin delivering vnode replacement
//! events synchronously; it does not inspect SQLite's private descriptor or
//! protect against content writes to the same inode. Write events are excluded
//! because SQLite must create sidecars during open. Existing owned-open checks
//! still apply at later write boundaries.
//!
//! At most 128 handles are retained, every kqueue call is nonblocking, and RAII
//! releases the queue and handles on success and on every error path.
use super::*;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

const MAX_ANCESTORS: usize = 128;
const CHANGED: &str = "database binding changed during SQLite open";

#[derive(Debug)]
pub(super) struct SqliteBinding {
    target: PathBuf,
    queue: Kqueue,
    watched: Vec<(PathBuf, same_file::Handle)>,
}

impl SqliteBinding {
    pub(super) fn new(expected: &DatabaseFileIdentity) -> io::Result<Self> {
        let mut target = PathBuf::new();
        fcntl(expected.0.as_file(), FcntlArg::F_GETPATH(&mut target))?;
        let target = target.canonicalize()?;
        let mut paths: Vec<_> = target
            .ancestors()
            .take(MAX_ANCESTORS + 1)
            .map(Path::to_path_buf)
            .collect();
        if paths.len() > MAX_ANCESTORS {
            return Err(io::Error::other("database path exceeds its ancestor bound"));
        }
        paths.reverse();
        let queue = Kqueue::new()?;
        let mut binding = Self {
            target,
            queue,
            watched: Vec::with_capacity(paths.len()),
        };
        // Root to leaf: retain every ancestor before opening its descendant.
        // A replacement during registration is caught either by its pending
        // vnode event or the final retained/path identity comparison.
        for path in paths {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_EVTONLY | (OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC).bits())
                .open(&path)?;
            let held = same_file::Handle::from_file(file)?;
            let change = KEvent::new(
                held.as_file().as_raw_fd() as usize,
                EventFilter::EVFILT_VNODE,
                // Without EV_CLEAR, a change remains pending for this lease.
                EvFlags::EV_ADD | EvFlags::EV_ENABLE,
                FilterFlag::NOTE_RENAME
                    | FilterFlag::NOTE_DELETE
                    | FilterFlag::NOTE_REVOKE
                    | if path == binding.target {
                        FilterFlag::NOTE_LINK
                    } else {
                        FilterFlag::empty()
                    },
                0,
                0,
            );
            binding.queue.kevent(&[change], &mut [], Some(zero()))?;
            binding.watched.push((path, held));
        }
        binding.validate(expected)?;
        Ok(binding)
    }

    pub(super) fn target(&self) -> &Path {
        &self.target
    }

    pub(super) fn validate(&self, expected: &DatabaseFileIdentity) -> io::Result<()> {
        self.unchanged()?;
        for (path, held) in &self.watched {
            if same_file::Handle::from_path(path)? != *held {
                return Err(io::Error::other(CHANGED));
            }
        }
        if self
            .watched
            .last()
            .is_none_or(|(_, held)| *held != expected.0)
        {
            return Err(io::Error::other(CHANGED));
        }
        self.unchanged()
    }

    fn unchanged(&self) -> io::Result<()> {
        let empty = KEvent::new(
            0,
            EventFilter::EVFILT_VNODE,
            EvFlags::empty(),
            FilterFlag::empty(),
            0,
            0,
        );
        let mut events = [empty];
        if self.queue.kevent(&[], &mut events, Some(zero()))? != 0 {
            return Err(io::Error::other(CHANGED));
        }
        Ok(())
    }
}

fn zero() -> nix::libc::timespec {
    nix::libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    }
}

pub(super) fn sqlite_error(source: io::Error) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
        Some(source.to_string()),
    )
}

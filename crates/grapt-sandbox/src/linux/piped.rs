//! Mount and syscall policy for a piped process inside its caller's cgroup.
use super::{mounts::Mounts, seccomp};
use crate::{NetworkAccess, ProcessRequest, SpawnFailure};
use nix::fcntl::{fcntl, FcntlArg, SealFlag};
use nix::sys::memfd::{memfd_create, MFdFlags};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

/// Holds mount leases and the private seccomp program until the caller has
/// stopped its process tree. The caller may prepend a resource supervisor.
pub struct PipedCommand {
    arguments: Vec<OsString>,
    _mounts: Mounts,
    _filter: File,
}

impl std::fmt::Debug for PipedCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PipedCommand")
            .finish_non_exhaustive()
    }
}

impl PipedCommand {
    pub fn prepare(request: &ProcessRequest) -> Result<Self, SpawnFailure> {
        request.validate()?;
        Self::prepare_inner(request)
            .map_err(|reason| SpawnFailure::for_backend("bwrap", reason, false))
    }

    fn prepare_inner(request: &ProcessRequest) -> Result<Self, String> {
        let launcher = std::fs::metadata("/usr/bin/bwrap")
            .map_err(|_| "bubblewrap is unavailable; install bubblewrap before loading plugins")?;
        if !launcher.is_file() || launcher.permissions().mode() & 0o111 == 0 {
            return Err("bubblewrap is not executable; repair the bubblewrap installation".into());
        }
        let mut policy = request.policy.clone();
        let mut aliases = Vec::new();
        // Admit only platform runtime inputs, preserving merged-/usr aliases
        // as symlinks to the same mount. Carveouts must apply through every
        // spelling; a later duplicate bind would expose denied library files.
        for library in [
            "/lib",
            "/lib64",
            "/usr/lib",
            "/usr/lib64",
            "/etc/ld.so.cache",
        ] {
            let canonical = match std::fs::canonicalize(library) {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!("sandbox runtime library is unreadable: {error}"))
                }
            };
            if !policy.readable_roots.contains(&canonical) {
                policy.readable_roots.push(canonical.clone());
            }
            if canonical != Path::new(library) {
                aliases.push((canonical, library.into()));
            }
        }
        let mounts = Mounts::prepare_with_aliases(&policy, &[], &aliases)?;
        let descriptor = memfd_create(
            "grapt-seccomp",
            MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING,
        )
        .map_err(|error| format!("sandbox filter allocation failed: {error}"))?;
        let mut filter = File::from(descriptor);
        for instruction in seccomp::program(request.policy.network == NetworkAccess::On)? {
            filter
                .write_all(&instruction.code.to_ne_bytes())
                .map_err(|error| error.to_string())?;
            filter
                .write_all(&[instruction.jt, instruction.jf])
                .map_err(|error| error.to_string())?;
            filter
                .write_all(&instruction.k.to_ne_bytes())
                .map_err(|error| error.to_string())?;
        }
        filter.flush().map_err(|error| error.to_string())?;
        filter.rewind().map_err(|error| error.to_string())?;
        fcntl(
            &filter,
            FcntlArg::F_ADD_SEALS(
                SealFlag::F_SEAL_WRITE
                    | SealFlag::F_SEAL_GROW
                    | SealFlag::F_SEAL_SHRINK
                    | SealFlag::F_SEAL_SEAL,
            ),
        )
        .map_err(|error| format!("sandbox filter sealing failed: {error}"))?;
        // No writable filesystem grant can modify the filter for another
        // process before its launcher opens it. The manager opens this live
        // descriptor outside the guest's private PID and mount namespaces.
        let filter_path = format!("/proc/{}/fd/{}", std::process::id(), filter.as_raw_fd());
        // Only the trusted pre-launch shell opens this descriptor. Arguments
        // remain positional data; bwrap consumes and closes the filter before
        // executing the guest. No helper executable or guest bootstrap runs
        // before enforcement.
        let mut arguments = vec![
            "-c".into(),
            "exec 3<\"$1\"; shift; exec \"$@\"".into(),
            "grapt-sandbox".into(),
            filter_path.into(),
            "/usr/bin/bwrap".into(),
            "--die-with-parent".into(),
            "--new-session".into(),
            "--unshare-user".into(),
            "--disable-userns".into(),
            "--unshare-pid".into(),
            "--as-pid-1".into(),
            "--unshare-ipc".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--seccomp".into(),
            "3".into(),
        ];
        if request.policy.network == NetworkAccess::Off {
            arguments.push("--unshare-net".into());
        }
        arguments.extend(mounts.arguments.iter().cloned());
        arguments.extend([
            "--chdir".into(),
            request.working_directory.as_os_str().into(),
            "--clearenv".into(),
        ]);
        for entry in &request.environment {
            let (name, value) = entry
                .split_once('=')
                .ok_or("sandbox environment is malformed")?;
            arguments.extend(["--setenv".into(), name.into(), value.into()]);
        }
        arguments.push("--".into());
        arguments.push(request.executable.as_os_str().into());
        arguments.extend(request.arguments.iter().map(OsString::from));
        Ok(Self {
            arguments,
            _mounts: mounts,
            _filter: filter,
        })
    }

    /// Append an absolute launcher and its arguments to a caller-owned resource
    /// wrapper. Stdio and the process tree remain owned by that caller.
    pub fn append_to(&self, command: &mut Command) {
        command.arg("/bin/sh").args(&self.arguments);
    }
}

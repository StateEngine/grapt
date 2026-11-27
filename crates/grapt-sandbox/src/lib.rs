//! Contained shell creation without owning terminal I/O or effect authorization.
//!
//! The caller supplies an already resolved policy and a native command line,
//! owns the inherited console and supervises the process tree. An unavailable
//! backend returns a degraded refusal; this crate never launches a fallback.
use grapt_types::RealizedEnforcement;
use std::path::PathBuf;

mod process;
pub use process::ProcessRequest;
#[cfg(windows)]
pub use process::{spawn_piped, PipedProcess};

mod write_target;
pub use write_target::WriteTarget;

#[cfg(unix)]
mod aliases;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod native;
mod seatbelt;
#[cfg(target_os = "linux")]
pub use linux::{Bwrap, LinuxInterrupts, PipedCommand};
pub use seatbelt::Seatbelt;

/// Network policy for the contained process and its descendants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkAccess {
    Off,
    On,
}

/// Native filesystem paths, resolved by the effect owner before spawning.
/// Protected paths remain readable but cannot be written, even beneath a
/// writable root; denied paths cannot be read either. Missing protected paths
/// are valid so creating a new config file cannot evade the restriction.
#[derive(Debug, Clone)]
pub struct Policy {
    pub readable_roots: Vec<PathBuf>,
    pub writable_roots: Vec<PathBuf>,
    pub protected_paths: Vec<PathBuf>,
    pub denied_paths: Vec<PathBuf>,
    pub network: NetworkAccess,
}

/// A native Windows command line, or POSIX shell source on Linux and macOS.
/// Environment entries are explicit `NAME=VALUE` strings; nothing ambient is
/// merged by the adapter. Debug output deliberately excludes both fields.
#[derive(Clone)]
pub struct ShellRequest {
    pub command_line: String,
    pub working_directory: PathBuf,
    pub environment: Vec<String>,
    pub timeout_ms: u32,
    pub policy: Policy,
}

impl std::fmt::Debug for ShellRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellRequest")
            .field("working_directory", &self.working_directory)
            .field("timeout_ms", &self.timeout_ms)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl ShellRequest {
    fn validate(&self) -> Result<(), &'static str> {
        if self.command_line.trim().is_empty()
            || self.command_line.len() > 64 * 1024
            || self.command_line.contains('\0')
        {
            return Err("sandbox command line must contain 1..65536 bytes without NUL");
        }
        if !self.working_directory.is_absolute()
            || self
                .working_directory
                .to_str()
                .is_none_or(|path| path.len() > 32 * 1024 || path.contains('\0'))
            || self.timeout_ms == 0
            || self.timeout_ms > 86_400_000
        {
            return Err("sandbox needs an absolute workdir and a timeout within one day");
        }
        let paths = self
            .policy
            .readable_roots
            .iter()
            .chain(&self.policy.writable_roots)
            .chain(&self.policy.protected_paths)
            .chain(&self.policy.denied_paths);
        if paths.clone().count() > 128
            || paths.clone().any(|p| {
                !p.is_absolute()
                    || p.to_str()
                        .is_none_or(|s| s.len() > 32 * 1024 || s.contains('\0'))
            })
        {
            return Err("sandbox policy needs at most 128 absolute Unicode paths");
        }
        for path in &self.policy.writable_roots {
            let resolved =
                std::fs::canonicalize(path).map_err(|_| "sandbox writable root is unavailable")?;
            if resolved.parent().is_none() {
                return Err("sandbox cannot make a filesystem root writable");
            }
        }
        if self.environment.len() > 1024
            || self.environment.iter().map(String::len).sum::<usize>() > 256 * 1024
            || self.environment.iter().any(|entry| {
                entry.contains('\0')
                    || !entry
                        .split_once('=')
                        .is_some_and(|(name, _)| !name.is_empty())
            })
        {
            return Err("sandbox environment is malformed or exceeds its bound");
        }
        Ok(())
    }
}

/// No shell is returned on failure. A launch error may occur after process
/// creation, so the caller must retain its existing cleanup owner.
#[derive(Debug, Clone)]
pub struct SpawnFailure {
    pub enforcement: RealizedEnforcement,
    pub effects_possible: bool,
    pub reason: String,
}

impl SpawnFailure {
    fn refused(reason: impl Into<String>, effects_possible: bool) -> Self {
        Self::for_backend("mxc", reason, effects_possible)
    }

    fn for_backend(backend: &str, reason: impl Into<String>, effects_possible: bool) -> Self {
        let mut reason = reason.into();
        while reason.len() > 2048 {
            reason.pop();
        }
        Self {
            enforcement: RealizedEnforcement::Degraded {
                backend: backend.into(),
                reason: reason.clone(),
            },
            effects_possible,
            reason,
        }
    }
}

impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}
impl std::error::Error for SpawnFailure {}

/// The one backend seam: policy in, contained child and realized enforcement out.
pub trait ContainedShellSpawner {
    fn spawn(&self, request: &ShellRequest) -> Result<ContainedShell, SpawnFailure>;

    /// Apply only targets approved by the caller's permission owner. The native
    /// adapter retains their identity through binding and the child's lifetime.
    fn spawn_with_write_targets(
        &self,
        request: &ShellRequest,
        targets: &[WriteTarget],
    ) -> Result<ContainedShell, SpawnFailure>;
}

#[derive(Debug, Default)]
pub struct Mxc;

impl ContainedShellSpawner for Mxc {
    fn spawn(&self, request: &ShellRequest) -> Result<ContainedShell, SpawnFailure> {
        self.spawn_with_write_targets(request, &[])
    }

    fn spawn_with_write_targets(
        &self,
        request: &ShellRequest,
        targets: &[WriteTarget],
    ) -> Result<ContainedShell, SpawnFailure> {
        let mut request = request.clone();
        request.policy = request
            .policy
            .with_write_targets(targets)
            .map_err(|reason| SpawnFailure::refused(reason, false))?;
        request
            .validate()
            .map_err(|reason| SpawnFailure::refused(reason, false))?;
        #[cfg(windows)]
        {
            native::spawn(&request).map(|child| ContainedShell {
                child,
                _write_targets: targets.to_vec(),
            })
        }
        #[cfg(not(windows))]
        {
            Err(SpawnFailure::refused(
                "MXC native process containment requires Windows",
                false,
            ))
        }
    }
}

/// Opaque contained process. Blocking methods run on the terminal supervisor's
/// thread; the adapter does not start another runtime or collect tool output.
pub struct ContainedShell {
    #[cfg(windows)]
    child: native::Child,
    #[cfg(target_os = "linux")]
    child: linux::Child,
    #[cfg(target_os = "macos")]
    child: seatbelt::native::Child,
    // Fields drop in declaration order: native child cleanup precedes pins.
    _write_targets: Vec<WriteTarget>,
}

impl std::fmt::Debug for ContainedShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainedShell").finish_non_exhaustive()
    }
}

impl ContainedShell {
    /// Available only after native process creation has succeeded.
    pub fn enforcement(&self) -> RealizedEnforcement {
        RealizedEnforcement::PlatformSandbox {
            backend: if cfg!(target_os = "linux") {
                "bwrap"
            } else if cfg!(target_os = "macos") {
                "seatbelt"
            } else {
                "mxc"
            }
            .into(),
        }
    }

    pub fn wait(&mut self) -> std::io::Result<i32> {
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        {
            self.child.wait()
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            Err(std::io::Error::other(
                "MXC child is unavailable on this platform",
            ))
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        {
            self.child.try_wait()
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            Err(std::io::Error::other(
                "MXC child is unavailable on this platform",
            ))
        }
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        {
            self.child.kill()
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            Err(std::io::Error::other(
                "MXC child is unavailable on this platform",
            ))
        }
    }
}

#[cfg(test)]
mod tests;

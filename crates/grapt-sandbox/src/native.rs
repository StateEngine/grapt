//! Native Windows process security with inherited or piped standard I/O.
//! Copyright 2025 OpenAI.
//! Apache-2.0. Modified: safe exact-pinned SDK, native-only refusal and bounded
//! inputs; terminal/process-tree lifetime remains owned by the caller.
use super::{NetworkAccess, ShellRequest, SpawnFailure};
use mxc_sdk::mxc_common::{
    filesystem_object::normalize_object_conflicts,
    logger::{Logger, Mode},
    models,
    sandbox_process::{SandboxBackend, SandboxProcess, StdioMode},
};
use mxc_sdk::process_container_common::base_container_runner::BaseContainerRunner;

pub(super) struct Child(Box<dyn SandboxProcess>);

impl Child {
    pub fn id(&self) -> u32 {
        self.0.id()
    }
    pub fn take_stdin(&mut self) -> Option<Box<dyn std::io::Write + Send>> {
        self.0.take_stdin()
    }
    pub fn take_stdout(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.0.take_stdout()
    }
    pub fn take_stderr(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.0.take_stderr()
    }
    pub fn wait(&mut self) -> std::io::Result<i32> {
        self.0.wait()
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        self.0.try_wait()
    }
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.0.kill()
    }
}

fn supported_host() -> Result<(), SpawnFailure> {
    use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};
    let version = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .map_err(|_| SpawnFailure::refused("cannot read the Windows build for MXC", false))?;
    let build: String = version
        .get_value("CurrentBuildNumber")
        .map_err(|_| SpawnFailure::refused("Windows build number is unavailable for MXC", false))?;
    let build: u32 = build
        .parse()
        .map_err(|_| SpawnFailure::refused("Windows build number is invalid for MXC", false))?;
    let revision: u32 = version.get_value("UBR").map_err(|_| {
        SpawnFailure::refused("Windows build revision is unavailable for MXC", false)
    })?;
    if (build, revision) < (26100, 9278) {
        return Err(SpawnFailure::refused(
            "MXC requires Windows 11 build 26100.9278 or newer",
            false,
        ));
    }
    if !BaseContainerRunner::is_base_container_api_present() {
        return Err(SpawnFailure::refused(
            "native MXC process security environment API is unavailable",
            false,
        ));
    }
    let probe = mxc_sdk::v1::probe(None)
        .map_err(|_| SpawnFailure::refused("native MXC enforcement probe failed", false))?;
    if !probe.probes.base_container_supports_deny_paths {
        return Err(SpawnFailure::refused(
            "native MXC cannot enforce protected paths on this Windows build",
            false,
        ));
    }
    Ok(())
}

fn paths(paths: &[std::path::PathBuf]) -> Vec<String> {
    // ShellRequest::validate already rejected non-Unicode paths.
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

// PSEC resolves the parent of each deny entry. Deny the first missing
// component instead of a descendant whose parent cannot yet be resolved.
// This preserves refusal of creation without creating configuration directories.
fn denied_path(path: &std::path::Path) -> Result<String, SpawnFailure> {
    let unreadable = |error| {
        SpawnFailure::refused(
            format!("cannot resolve denied sandbox path: {error}"),
            false,
        )
    };
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(path.to_string_lossy().into_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unreadable(error)),
    }
    let mut absent = path.to_path_buf();
    for _ in 0..64 {
        let parent = absent.parent().ok_or_else(|| {
            SpawnFailure::refused("denied sandbox path has no existing parent", false)
        })?;
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.is_dir() => {
                return Ok(absent.to_string_lossy().into_owned());
            }
            Ok(_) => {
                return Err(SpawnFailure::refused(
                    "denied sandbox path parent is not a directory",
                    false,
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                absent = parent.to_path_buf();
            }
            Err(error) => return Err(unreadable(error)),
        }
    }
    Err(SpawnFailure::refused(
        "denied sandbox path exceeds 64 ancestors",
        false,
    ))
}

pub(super) fn spawn(request: &ShellRequest) -> Result<Child, SpawnFailure> {
    spawn_stdio(request, StdioMode::Inherit)
}

pub(super) fn spawn_piped(request: &ShellRequest) -> Result<Child, SpawnFailure> {
    spawn_stdio(request, StdioMode::Pipes)
}

fn spawn_stdio(request: &ShellRequest, stdio: StdioMode) -> Result<Child, SpawnFailure> {
    supported_host()?;
    let mut readonly_paths = paths(&request.policy.readable_roots);
    let mut denied_paths = request
        .policy
        .denied_paths
        .iter()
        .map(|path| denied_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    for protected in &request.policy.protected_paths {
        // Native read-only entries protect existing objects. For an absent
        // config or hooks path, a native deny must also prevent its creation.
        let path = protected.to_string_lossy().into_owned();
        match std::fs::symlink_metadata(protected) {
            Ok(_) => readonly_paths.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                denied_paths.push(denied_path(protected)?);
            }
            Err(error) => {
                return Err(SpawnFailure::refused(
                    format!("cannot resolve protected sandbox path: {error}"),
                    false,
                ))
            }
        }
    }
    denied_paths.sort();
    denied_paths.dedup();
    let mut policy = models::ContainerPolicy {
        capabilities: vec!["registryRead".into()],
        readonly_paths,
        readwrite_paths: paths(&request.policy.writable_roots),
        denied_paths,
        fallback: models::FallbackPolicy {
            allow_dacl_mutation: false,
        },
        network_egress: Some(models::NetworkEgressPolicy {
            default: match request.policy.network {
                NetworkAccess::Off => models::NetworkAction::Deny,
                NetworkAccess::On => models::NetworkAction::Allow,
            },
            ..Default::default()
        }),
        network_ingress: Some(models::NetworkIngressPolicy {
            default: models::NetworkAction::Deny,
            host_loopback: models::NetworkAction::Deny,
        }),
        network_specified: true,
        network_mode_specified: true,
        ui: models::UiPolicy {
            disable: false,
            clipboard: models::ClipboardPolicy::None,
            injection: false,
        },
        base_process_ui: models::BaseProcessUiConfig {
            isolation: "desktop".into(),
            desktop_system_control: false,
            system_settings: "none".into(),
            ime: false,
        },
        ..Default::default()
    };
    let mut logger = Logger::new(Mode::Buffer);
    if let Some(normalized) = normalize_object_conflicts(&policy, &mut logger)
        .map_err(|reason| SpawnFailure::refused(reason, false))?
    {
        policy = normalized;
    }
    let execution = models::ExecutionRequest {
        script_code: request.command_line.clone(),
        working_directory: request.working_directory.to_string_lossy().into_owned(),
        // The pinned SDK explicitly defines zero as an infinite kernel wait.
        // A piped protocol has a caller-owned lifecycle and resource supervisor;
        // it must neither expire after a day nor wake an idle polling thread.
        script_timeout: if stdio == StdioMode::Pipes {
            0
        } else {
            request.timeout_ms
        },
        env: Some(request.environment.clone()),
        inherit_default_env: false,
        policy,
        ..Default::default()
    };
    let child = BaseContainerRunner::new()
        .spawn(&execution, &mut logger, stdio)
        .map_err(|error| SpawnFailure::refused(error.error_message, true))?;
    // The logger may contain command/environment data; never publish it.
    Ok(Child(child))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_mxc_hidden_inherited_console_api_canary() {
        fn needs_backend<T: SandboxBackend>() {}
        needs_backend::<BaseContainerRunner>();
        let _: StdioMode = StdioMode::Inherit;
        let _: BaseContainerRunner = BaseContainerRunner::new();
    }
}

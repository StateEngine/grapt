//! Per-process OS authority, independent of guest JavaScript permissions.
use super::*;
use crate::PluginAccess;
use grapt_sandbox::{NetworkAccess, Policy, ProcessRequest};

pub(super) struct ProcessAccess {
    pub(super) permissions: PluginAccess,
    pub(super) live: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(any(windows, target_os = "macos"))]
    pub(super) request: ProcessRequest,
    #[cfg(target_os = "linux")]
    pub(super) command: grapt_sandbox::PipedCommand,
    // Kept until the contained child has been reaped. No peer plugin receives
    // this directory's authority, even when their source roots coincide.
    _temporary: tempfile::TempDir,
}

impl ProcessAccess {
    pub(super) fn prepare(
        runtime: &VerifiedRuntime,
        declaration: &PluginDeclaration,
    ) -> Result<Self, PluginError> {
        let snapshot = declaration.executable.snapshot().ok_or_else(|| {
            PluginError::Runtime(
                "plugin containment requires a pinned local source snapshot".into(),
            )
        })?;
        let temporary = tempfile::Builder::new()
            .prefix("secode-plugin-")
            .tempdir()
            .map_err(|error| PluginError::Spawn(error.to_string()))?;
        let temp_path = child_path(temporary.path());
        let permissions = declaration.permissions.clone();
        let mut readable = permissions
            .readable_roots()
            .iter()
            .map(|path| child_path(path))
            .collect::<Vec<_>>();
        readable.extend([
            runtime.executable().to_path_buf(),
            runtime.bridge().to_path_buf(),
        ]);
        let mut writable = permissions
            .writable_roots()
            .iter()
            .map(|path| child_path(path))
            .collect::<Vec<_>>();
        writable.extend([child_path(snapshot.root()), temp_path.clone()]);
        let policy = Policy {
            readable_roots: readable,
            writable_roots: writable,
            protected_paths: vec![
                runtime.executable().to_path_buf(),
                runtime.bridge().to_path_buf(),
            ],
            denied_paths: Vec::new(),
            // Declared HTTP(S) traffic goes through the bounded host broker.
            // Guest sockets never receive host-network authority.
            network: NetworkAccess::Off,
        };
        let mut arguments = super::process::NODE_ARGUMENTS
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect::<Vec<_>>();
        for path in policy.readable_roots.iter().chain(&policy.writable_roots) {
            arguments.push(format!("--allow-fs-read={}", path.display()));
        }
        for path in &policy.writable_roots {
            arguments.push(format!("--allow-fs-write={}", path.display()));
        }
        arguments.push(runtime.bridge().to_string_lossy().into_owned());
        let mut environment = super::validation::required_environment()
            .into_iter()
            .filter(|(name, _)| matches!(*name, "SystemRoot" | "WINDIR" | "LANG" | "LC_ALL"))
            .map(|(name, value)| format!("{name}={}", value.to_string_lossy()))
            .collect::<Vec<_>>();
        for name in ["TEMP", "TMP", "TMPDIR"] {
            environment.push(format!("{name}={}", temp_path.display()));
        }
        let request = ProcessRequest {
            executable: runtime.executable().to_path_buf(),
            arguments,
            working_directory: child_path(snapshot.root()),
            environment,
            policy,
        };
        #[cfg(target_os = "linux")]
        let command = grapt_sandbox::PipedCommand::prepare(&request)
            .map_err(|error| PluginError::Spawn(format!("plugin_sandbox_unavailable: {error}; install bubblewrap and enable unprivileged user namespaces")))?;
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        return Err(PluginError::Runtime("plugin_sandbox_unavailable: filesystem and network containment is currently supported on Linux and Windows".into()));
        #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
        Ok(Self {
            permissions,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            #[cfg(any(windows, target_os = "macos"))]
            request,
            #[cfg(target_os = "linux")]
            command,
            _temporary: temporary,
        })
    }
}

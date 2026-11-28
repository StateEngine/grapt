//! Private policy handoff to the installed POSIX sandbox helper. Terminal
//! streams and process lifetime remain with the existing effect owner.
//! Apache-2.0); bounded private.
//! policy handoff replaces command-line policy and retains Grapt receipts.
use super::*;
use grapt_sandbox::{Bwrap, ContainedShellSpawner, ShellRequest};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod grants;

const FLAG: &str = "__grapt-posix-shell";
const MAX_REQUEST: usize = 1024 * 1024;
const MAX_ACK: usize = 4096;
const START_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
struct Request {
    command_line: String,
    directory: PathBuf,
    environment: Vec<String>,
    timeout_ms: u32,
    plan: Plan,
    write_targets: Vec<PathBuf>,
}

pub(in crate::dogfood::tools) struct Pending {
    listener: tokio::net::UnixListener,
    request: Vec<u8>,
    path: PathBuf,
    identity: (u64, u64),
    plan: Plan,
    targets: Vec<grapt_sandbox::WriteTarget>,
}

impl Pending {
    pub(in crate::dogfood::tools) fn prepare(
        plan: &Plan,
        command_line: String,
        directory: PathBuf,
        environment: &super::super::shell_environment::Environment,
        timeout_ms: u64,
    ) -> Result<Self, String> {
        let request = Request {
            command_line,
            directory,
            timeout_ms: timeout_ms
                .try_into()
                .map_err(|_| "sandbox timeout exceeds native bound")?,
            plan: plan.clone(),
            write_targets: environment
                .write_targets()
                .iter()
                .map(|target| target.path().to_path_buf())
                .collect(),
            environment: environment
                .iter()
                .map(|(name, value)| {
                    Ok(format!(
                        "{}={}",
                        name.to_str()
                            .ok_or("sandbox environment name is not Unicode")?,
                        value
                            .to_str()
                            .ok_or("sandbox environment value is not Unicode")?
                    ))
                })
                .collect::<Result<_, &str>>()?,
        };
        let request =
            serde_json::to_vec(&request).map_err(|_| "sandbox request could not be encoded")?;
        if request.len() > MAX_REQUEST {
            return Err("sandbox request exceeds 1 MiB".into());
        }
        let entropy =
            grapt_store::OsEntropy::draw_256().map_err(|_| "sandbox IPC entropy unavailable")?;
        let suffix = entropy
            .expose()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        // The protected registry keeps this path within sockaddr_un's limit
        // and prevents another contained command from replacing the listener.
        // No policy or environment content enters its name or process arguments.
        let path = Bwrap::ipc_directory()?.join(suffix);
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| "sandbox IPC needs the runtime")?;
        let _entered = runtime.enter();
        let listener =
            tokio::net::UnixListener::bind(&path).map_err(|_| "sandbox IPC unavailable")?;
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| "sandbox IPC identity unavailable")?;
        let result = Self {
            listener,
            request,
            path,
            identity: (metadata.dev(), metadata.ino()),
            plan: plan.clone(),
            targets: environment.write_targets().to_vec(),
        };
        std::fs::set_permissions(&result.path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "sandbox IPC permissions unavailable")?;
        Ok(result)
    }

    pub(in crate::dogfood::tools) fn plan(&self) -> &Plan {
        &self.plan
    }

    pub(in crate::dogfood::tools) fn arguments(&self) -> [OsString; 2] {
        [FLAG.into(), self.path.as_os_str().into()]
    }

    pub(in crate::dogfood::tools) async fn finish(
        mut self,
        expected_pid: u32,
    ) -> Result<RealizedEnforcement, String> {
        match tokio::time::timeout(START_BOUND, self.exchange(expected_pid)).await {
            Ok(result) => result,
            Err(_) => Err(failure(
                &degraded("sandbox helper startup timed out"),
                "sandbox helper startup timed out",
                None,
            )),
        }
    }

    async fn exchange(&mut self, expected_pid: u32) -> Result<RealizedEnforcement, String> {
        let exchange = async {
            let (mut channel, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| "sandbox helper did not connect")?;
            let peer = channel
                .peer_cred()
                .map_err(|_| "sandbox IPC peer identity unavailable")?;
            if peer.pid().and_then(|pid| u32::try_from(pid).ok()) != Some(expected_pid) {
                return Err("sandbox IPC peer is not the launched helper");
            }
            channel
                .write_all(&(self.request.len() as u32).to_le_bytes())
                .await
                .map_err(|_| "sandbox policy handoff failed")?;
            channel
                .write_all(&self.request)
                .await
                .map_err(|_| "sandbox policy handoff failed")?;
            grants::send(&channel, &self.targets).await?;
            let length = channel
                .read_u32_le()
                .await
                .map_err(|_| "sandbox helper exited without a receipt")?
                as usize;
            if length == 0 || length > MAX_ACK {
                return Err("sandbox helper receipt exceeds bound");
            }
            let mut bytes = vec![0; length];
            channel
                .read_exact(&mut bytes)
                .await
                .map_err(|_| "sandbox helper receipt is incomplete")?;
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .map_err(|_| "sandbox helper receipt is malformed")
        }
        .await
        .map_err(|reason| failure(&degraded(reason), reason, None))?;
        match (
            exchange["enforcement"]["kind"].as_str(),
            exchange["enforcement"]["backend"].as_str(),
        ) {
            (Some("platform_sandbox"), Some("bwrap")) if exchange["ran"] == true => {
                Ok(RealizedEnforcement::PlatformSandbox {
                    backend: "bwrap".into(),
                })
            }
            (Some("degraded"), Some("bwrap")) => Err(exchange.to_string()),
            _ => Err(failure(
                &degraded("sandbox helper receipt names no native enforcement"),
                "sandbox helper receipt names no native enforcement",
                None,
            )),
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if metadata.file_type().is_socket() && (metadata.dev(), metadata.ino()) == self.identity
            {
                let _remove = std::fs::remove_file(&self.path);
            }
        }
    }
}

pub(crate) fn helper_main(arguments: &[String]) -> Option<i32> {
    if arguments.first().is_none_or(|arg| arg != FLAG) {
        return None;
    }
    let result = match arguments {
        [_, path] => run(path),
        _ => Err("sandbox helper arguments are malformed".into()),
    };
    Some(result.unwrap_or_else(|reason| {
        eprintln!("{reason}");
        1
    }))
}

pub(super) fn ensure_protected_scratch_anchor(anchor: Option<&Path>) -> Result<(), String> {
    // Only create/verify the exact scratch anchor planned by the host;
    // never scan policy.protected_paths by basename.
    if let Some(parent) = anchor.and_then(Path::parent) {
        super::scratch::ensure_unix_scratch_anchor(parent)?;
    }
    Ok(())
}

fn run(path: &str) -> Result<i32, String> {
    let mut interrupts = grapt_sandbox::LinuxInterrupts::new().map_err(|e| e.to_string())?;
    let mut channel = std::os::unix::net::UnixStream::connect(path)
        .map_err(|_| "sandbox helper could not reach its owner")?;
    channel
        .set_read_timeout(Some(START_BOUND))
        .and_then(|()| channel.set_write_timeout(Some(START_BOUND)))
        .map_err(|_| "sandbox IPC deadline unavailable")?;
    let mut length = [0; 4];
    channel
        .read_exact(&mut length)
        .map_err(|_| "sandbox policy is absent")?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_REQUEST {
        return Err("sandbox policy exceeds bound".into());
    }
    let mut bytes = vec![0; length];
    channel
        .read_exact(&mut bytes)
        .map_err(|_| "sandbox policy is incomplete")?;
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|_| "sandbox policy is malformed")?;
    let mut policy = request.plan.policy();
    let mut environment = request.environment;
    let helper_exe = std::env::current_exe().map_err(|_| "sandbox helper image unavailable")?;
    let scratch =
        super::scratch::PrivateScratch::attach(&request.plan, &mut policy, &mut environment)?;
    // When private_temp_parent is disabled because a write target overlaps the
    // host temp directory, ensure protected Unix 0700 scratch anchors exist at
    // launch time before Linux synthetic mount targets can create them with
    // default umask 0755.
    if scratch.is_none() {
        ensure_protected_scratch_anchor(request.plan.protected_scratch_anchor())?;
    }
    let targets = match grants::receive(&channel, &policy, &request.write_targets) {
        Ok(targets) => targets,
        Err(reason) => {
            let cleanup_err = super::scratch::cleanup_after_stop(scratch, true);
            return Err(super::scratch::combine_errors(reason, [cleanup_err]));
        }
    };
    let request = ShellRequest {
        command_line: request.command_line,
        working_directory: request.directory,
        environment,
        timeout_ms: request.timeout_ms,
        policy,
    };
    let backend = Bwrap::new(helper_exe);
    let mut child = match backend.spawn_with_write_targets(&request, &targets) {
        Ok(child) => child,
        Err(error) => {
            let cleanup_err = super::scratch::cleanup_after_stop(scratch, !error.effects_possible);
            let receipt_err = send(
                &mut channel,
                &error.enforcement,
                (!error.effects_possible).then_some(false),
            )
            .err();
            return Err(super::scratch::combine_errors(
                error.reason,
                [receipt_err, cleanup_err],
            ));
        }
    };
    if let Err(reason) = send(&mut channel, &child.enforcement(), Some(true)) {
        let (kill_err, reap_err, stopped) = stop_and_reap_child(child);
        let cleanup_err = super::scratch::cleanup_after_stop(scratch, stopped);
        return Err(super::scratch::combine_errors(
            reason,
            [kill_err, reap_err, cleanup_err],
        ));
    }
    drop(channel);
    let status = interrupts
        .wait(&mut child)
        .map_err(|error| format!("sandbox process observation failed: {error}"));
    match status {
        Ok(code) => {
            drop(child);
            let cleanup = scratch.map_or(Ok(()), super::scratch::PrivateScratch::cleanup);
            let (exit_code, diagnostic) = super::scratch::finish_with_exit_code(code, cleanup);
            if let Some(reason) = diagnostic {
                if code == 0 {
                    return Err(reason);
                }
                eprintln!("{reason}");
            }
            Ok(exit_code)
        }
        Err(wait_reason) => {
            let (kill_err, reap_err, stopped) = stop_and_reap_child(child);
            let cleanup_err = super::scratch::cleanup_after_stop(scratch, stopped);
            Err(super::scratch::combine_errors(
                wait_reason,
                [kill_err, reap_err, cleanup_err],
            ))
        }
    }
}

fn stop_and_reap_child(
    mut child: grapt_sandbox::ContainedShell,
) -> (Option<String>, Option<String>, bool) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        drop(child);
        return (None, None, true);
    }
    let kill_err = child
        .kill()
        .err()
        .map(|error| format!("sandbox kill failed: {error}"));
    let reap_err = child
        .wait()
        .err()
        .map(|error| format!("sandbox reap failed: {error}"));
    let stopped = reap_err.is_none();
    drop(child);
    (kill_err, reap_err, stopped)
}

fn send(
    channel: &mut std::os::unix::net::UnixStream,
    enforcement: &RealizedEnforcement,
    ran: Option<bool>,
) -> Result<(), String> {
    let bytes = serde_json::json!({"enforcement":value(enforcement),"ran":ran})
        .to_string()
        .into_bytes();
    channel
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|()| channel.write_all(&bytes))
        .and_then(|()| channel.flush())
        .map_err(|_| "sandbox receipt was not delivered".into())
}

pub(in crate::dogfood::tools) fn cleanup(plan: &Plan) -> Result<(), String> {
    Bwrap::cleanup_targets(&plan.protected)
}

pub(in crate::dogfood::tools) fn failed_cleanup(reason: &str, cleanup: &str) -> String {
    let Some(mut receipt) = super::receipt(reason) else {
        return failure(
            &degraded(reason),
            &format!("{reason}; cleanup: {cleanup}"),
            None,
        );
    };
    let detail = receipt["error"]
        .as_str()
        .or_else(|| receipt["enforcement"]["reason"].as_str())
        .unwrap_or("sandbox startup failed");
    receipt["error"] = serde_json::json!(format!("{detail}; cleanup: {cleanup}"));
    receipt.to_string()
}

pub(in crate::dogfood::tools) async fn settle(plan: &Plan) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + super::super::terminal::STOP_SETTLE_BOUND;
    loop {
        match cleanup(plan) {
            Ok(()) => return Ok(()),
            Err(reason) if tokio::time::Instant::now() >= deadline => return Err(reason),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
}

pub(in crate::dogfood::tools) fn command_line(
    program: &std::ffi::OsStr,
    arguments: &[OsString],
) -> Result<String, String> {
    std::iter::once(program)
        .chain(arguments.iter().map(OsString::as_os_str))
        .map(|arg| {
            let arg = arg.to_str().ok_or("sandbox argument is not Unicode")?;
            if arg.contains('\0') {
                return Err("sandbox argument contains NUL");
            }
            Ok(format!("'{}'", arg.replace('\'', "'\\''")))
        })
        .collect::<Result<Vec<_>, &str>>()
        .map(|parts| format!("exec {}", parts.join(" ")))
        .map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unrelated_unix_peer_receives_no_policy_or_environment() -> Result<(), String> {
        let root = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
        let plan = Plan {
            git_metadata_read_only: false,
            readable: vec![root.clone()],
            writable: vec![root.clone()],
            protected: vec![],
            denied: Vec::new(),
            network: false,
            private_temp_parent: None,
            protected_scratch_anchor: None,
        };
        let environment = super::super::super::shell_environment::Environment::parent()?;
        let pending = Pending::prepare(
            &plan,
            "printf SHOULD_NOT_RUN".into(),
            root,
            &environment,
            1000,
        )?;
        let path = pending.path.clone();
        let peer = tokio::spawn(async move {
            let channel = tokio::net::UnixStream::connect(path)
                .await
                .map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            channel
                .take(8)
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(bytes)
        });
        let refusal = pending
            .finish(u32::MAX)
            .await
            .err()
            .ok_or("unrelated peer was trusted")?;
        assert!(refusal.contains("not the launched helper"), "{refusal}");
        assert!(
            peer.await.map_err(|e| e.to_string())??.is_empty(),
            "policy reached an unrelated process"
        );
        Ok(())
    }
}

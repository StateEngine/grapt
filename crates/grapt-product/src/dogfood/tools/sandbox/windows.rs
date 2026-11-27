//! A private, single-use policy/receipt channel to the existing executable.
//! The helper inherits the terminal's streams and cleanup job. MXC receives
//! only the prepared environment and its own stdio handle allowlist.
use super::*;
use grapt_sandbox::{ContainedShellSpawner, Mxc, ShellRequest};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

const FLAG: &str = "__grapt-mxc-shell";
const HANDLE_FLAG: &str = "__grapt-mxc-handle";
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

/// Not Debug: the one-use name is an IPC capability, and the request can
/// contain environment secrets. The request never enters arguments or logs.
pub(in crate::dogfood::tools) struct Pending {
    server: NamedPipeServer,
    request: Vec<u8>,
    name: OsString,
    _write_targets: Vec<grapt_sandbox::WriteTarget>,
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
            environment: environment
                .iter()
                .map(|(name, value)| {
                    let name = name
                        .to_str()
                        .ok_or("sandbox environment name is not Unicode")?;
                    let value = value
                        .to_str()
                        .ok_or("sandbox environment value is not Unicode")?;
                    Ok(format!("{name}={value}"))
                })
                .collect::<Result<_, &str>>()?,
            timeout_ms: timeout_ms
                .try_into()
                .map_err(|_| "sandbox timeout exceeds native bound")?,
            plan: plan.clone(),
            write_targets: environment
                .write_targets()
                .iter()
                .map(|target| target.path().to_path_buf())
                .collect(),
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
        let name = OsString::from(format!(r"\\.\pipe\grapt-mxc-{suffix}"));
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| "sandbox IPC needs the runtime")?;
        let _entered = runtime.enter();
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .create(&name)
            .map_err(|_| "sandbox IPC unavailable")?;
        Ok(Self {
            server,
            request,
            name,
            _write_targets: environment.write_targets().to_vec(),
        })
    }

    pub(in crate::dogfood::tools) fn arguments(&self) -> [OsString; 2] {
        [OsString::from(FLAG), self.name.clone()]
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
            self.server
                .connect()
                .await
                .map_err(|_| "sandbox helper did not connect")?;
            let duplicate = filedescriptor::FileDescriptor::dup(&self.server)
                .and_then(|handle| handle.as_file())
                .map_err(|_| "sandbox IPC peer unavailable")?;
            use interprocess::os::windows::named_pipe::{pipe_mode::Bytes, PipeStream};
            let peer = PipeStream::<Bytes, Bytes>::try_from(
                std::os::windows::io::OwnedHandle::from(duplicate),
            )
            .map_err(|_| "sandbox IPC peer is not a pipe")?;
            // This duplicate only queries identity; it has no writes to linger.
            peer.assume_flushed();
            let pid = peer
                .client_process_id()
                .map_err(|_| "sandbox IPC peer identity unavailable")?;
            if pid != expected_pid {
                return Err("sandbox IPC peer is not the launched helper");
            }
            drop(peer);
            self.server
                .write_all(&(self.request.len() as u32).to_le_bytes())
                .await
                .map_err(|_| "sandbox policy handoff failed")?;
            self.server
                .write_all(&self.request)
                .await
                .map_err(|_| "sandbox policy handoff failed")?;
            let length = self
                .server
                .read_u32_le()
                .await
                .map_err(|_| "sandbox helper exited without a receipt")?
                as usize;
            if length == 0 || length > MAX_ACK {
                return Err("sandbox helper receipt exceeds bound");
            }
            let mut bytes = vec![0; length];
            self.server
                .read_exact(&mut bytes)
                .await
                .map_err(|_| "sandbox helper receipt is incomplete")?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|_| "sandbox helper receipt is malformed")?;
            Ok(value)
        }
        .await
        .map_err(|reason| failure(&degraded(reason), reason, None))?;
        match (
            exchange["enforcement"]["kind"].as_str(),
            exchange["enforcement"]["backend"].as_str(),
        ) {
            (Some("platform_sandbox"), Some("mxc")) if exchange["ran"] == true => {
                Ok(RealizedEnforcement::PlatformSandbox {
                    backend: "mxc".into(),
                })
            }
            (Some("degraded"), Some("mxc")) => Err(exchange.to_string()),
            _ => Err(failure(
                &degraded("sandbox helper receipt names no native enforcement"),
                "sandbox helper receipt names no native enforcement",
                None,
            )),
        }
    }
}

/// The ConPTY root opens the policy channel itself and passes only its
/// private handle to the native helper. The owner authenticates the root PID,
/// so an earlier sandboxed process cannot preempt the channel and forge a receipt.
pub(in crate::dogfood::tools) fn forwarded_command(
    program: &OsStr,
    args: &[OsString],
) -> Result<Option<windows_spawn::Command>, String> {
    let [flag, pipe] = args else { return Ok(None) };
    if flag != FLAG {
        return Ok(None);
    }
    let channel = open(pipe)?;
    let mut command = windows_spawn::Command::new(program);
    command
        .arg(HANDLE_FLAG)
        .arg_handle(&channel)
        .map_err(|_| "sandbox IPC handle unavailable")?;
    Ok(Some(command))
}

pub(in crate::dogfood::tools) fn helper_main(args: &[OsString]) -> Option<i32> {
    let flag = args.first()?;
    if flag != FLAG && flag != HANDLE_FLAG {
        return None;
    }
    let result = match args {
        [_, value] if flag == FLAG => open(value).and_then(run),
        [_, value] => inherited(value).and_then(run),
        _ => Err("sandbox helper arguments are malformed".into()),
    };
    Some(result.unwrap_or_else(|reason| {
        eprintln!("{reason}");
        1
    }))
}

fn open(pipe: &OsStr) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe)
        .map_err(|_| "sandbox helper could not reach its owner".into())
}
fn inherited(value: &OsStr) -> Result<std::fs::File, String> {
    use std::os::windows::io::{AsRawHandle, RawHandle};
    struct Inherited(isize);
    impl AsRawHandle for Inherited {
        fn as_raw_handle(&self) -> RawHandle {
            self.0 as RawHandle
        }
    }
    let handle = value
        .to_str()
        .and_then(|s| s.parse::<isize>().ok())
        .filter(|v| *v > 0)
        .ok_or("sandbox IPC handle is malformed")?;
    // DuplicateHandle validates the numeric inherited handle before safe owned
    // adoption. No borrowed/raw handle is ever closed or adopted by our code.
    filedescriptor::FileDescriptor::dup(&Inherited(handle))
        .and_then(|handle| handle.as_file())
        .map_err(|_| "sandbox IPC handle is unavailable".into())
}
fn run(mut channel: std::fs::File) -> Result<i32, String> {
    let mut length = [0_u8; 4];
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
    let _interrupts = super::super::terminal::windows_shim::hold_interrupts()?;
    if request.write_targets.len() > 128 {
        return Err("Sandbox write target count exceeds bound".into());
    }
    // The authenticated parent retains no-delete-sharing pins until this
    // handoff completes. Resolve while those names cannot be replaced, then
    // retain the helper's own native pins through the contained child lifetime.
    let policy = request.plan.policy();
    let targets = request
        .write_targets
        .iter()
        .map(|path| grapt_sandbox::WriteTarget::resolve(&policy, path))
        .collect::<Result<Vec<_>, _>>()?;
    let request = ShellRequest {
        command_line: request.command_line,
        working_directory: request.directory,
        environment: request.environment,
        timeout_ms: request.timeout_ms,
        policy,
    };
    let mut child = match Mxc.spawn_with_write_targets(&request, &targets) {
        Ok(child) => child,
        Err(failure) => {
            send(
                &mut channel,
                &failure.enforcement,
                (!failure.effects_possible).then_some(false),
            )?;
            return Err(failure.reason);
        }
    };
    if let Err(reason) = send(&mut channel, &child.enforcement(), Some(true)) {
        let _killed = child.kill();
        return Err(reason);
    }
    drop(channel);
    child
        .wait()
        .map_err(|error| format!("sandbox process observation failed: {error}"))
}

fn send(
    channel: &mut std::fs::File,
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

/// Microsoft CRT argument spelling for the native CreateProcess command line.
/// cmd's /S framing is supplied by the existing shell resolver separately.
pub(in crate::dogfood::tools) fn quote(argument: &OsStr) -> Result<String, String> {
    let argument = argument.to_str().ok_or("sandbox argument is not Unicode")?;
    if argument.contains('\0') {
        return Err("sandbox argument contains NUL".into());
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        if character == '"' {
            out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n('\\', slashes));
        }
        out.push(character);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unrelated_pipe_client_receives_no_policy_or_environment() -> Result<(), String> {
        let root = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
        let plan = Plan {
            git_metadata_read_only: false,
            readable: vec![root.clone()],
            writable: vec![root.clone()],
            protected: vec![],
            denied: Vec::new(),
            network: false,
        };
        let environment = super::super::super::shell_environment::Environment::parent()?;
        let pending = Pending::prepare(
            &plan,
            "cmd.exe /D /C echo refused".into(),
            root,
            &environment,
            1000,
        )?;
        let name = pending.name.clone();
        let client = tokio::task::spawn_blocking(move || {
            let file = open(&name)?;
            let mut bytes = Vec::new();
            Read::take(file, 8)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(bytes)
        });
        let refusal = pending
            .finish(u32::MAX)
            .await
            .err()
            .ok_or("unrelated client was trusted")?;
        assert!(refusal.contains("not the launched helper"), "{refusal}");
        assert!(
            client.await.map_err(|e| e.to_string())??.is_empty(),
            "policy reached an unrelated process"
        );
        Ok(())
    }
}

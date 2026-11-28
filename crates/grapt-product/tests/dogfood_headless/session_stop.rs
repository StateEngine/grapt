//! Session stop is a cleanup receipt, including tools retained after a turn.
use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const POLL: Duration = Duration::from_millis(20);

#[test]
fn interrupt_joins_idle_exec_and_busy_shell_trees_without_stopping_another_session() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("session-stop");
    let workspace = root.with_extension("workspace");
    let _ws_cleanup = terminal_coordination::RootCleanup(workspace.clone());
    fs::create_dir_all(&workspace)?;
    prepare(&root, 120_000);
    unattended(&root);
    let _root_cleanup = terminal_coordination::RootCleanup(root.clone());
    fs::create_dir_all(root.join("global-config"))?;
    // On Linux, this fixture reads child PIDs written by `$$` and checks host
    // `/proc/{pid}/stat`, which is incompatible with `bwrap --unshare-pid`.
    // Keep sandbox enabled on Windows so Windows retains sandboxed stop coverage.
    #[cfg(target_os = "linux")]
    let global_config = r#"{"permission":{"shell":"allow"},"sandbox":{"enabled":false}}"#;
    #[cfg(not(target_os = "linux"))]
    let global_config = r#"{"permission":{"shell":"allow"}}"#;
    fs::write(root.join("global-config/grapt.json"), global_config)?;
    let provider = ProviderFixture::start(vec![
        tool("call_stop_other", "shell", "other"),
        tool("call_stop_idle", "exec_command", "idle"),
        Script::Success,
        tool("call_stop_busy", "shell", "busy"),
        Script::SuccessText("A later prompt completed after stop."),
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let mut processes = Processes::new(&workspace)?;
    let other = create_in(address, &workspace);
    let session = create_in(address, &workspace);

    prompt(address, &other, "msg_stop_other");
    let other_tree = processes.observe("other")?;
    let other_execution = active_execution(address, &other)?;
    prompt(address, &session, "msg_stop_idle");
    let idle_tree = processes.observe("idle")?;
    wait_calls(&provider.calls, 3);
    permission_residuals::wait_session_idle(address, &session);
    let retained: serde_json::Value =
        serde_json::from_str(&permission_boundaries::output(&provider, "call_stop_idle"))?;
    assert_eq!(retained["status"], "running", "{retained}");
    assert_eq!(retained["done"], false, "{retained}");
    assert_tree(&idle_tree, true)?;

    // Ordinary turn interrupts, including explicit false, preserve a retained tool.
    stop(address, &session, "{}");
    assert_tree(&idle_tree, true)?;
    assert_idle(address, &session);
    stop(address, &session, r#"{"stopProcesses":false}"#);
    assert_tree(&idle_tree, true)?;
    stop(address, &session, r#"{"stopProcesses":true}"#);
    // No settle polling here: 204 itself must witness process exit and idle state.
    assert_tree(&idle_tree, false)?;
    assert_idle(address, &session);
    assert_other_running(address, &other, &other_execution, &workspace, &other_tree)?;

    prompt(address, &session, "msg_stop_busy");
    let busy_tree = processes.observe("busy")?;
    let execution = active_execution(address, &session)?;
    stop(
        address,
        &session,
        &serde_json::json!({"executionID":execution.as_str(),"resumePending":false,"stopProcesses":true}).to_string(),
    );
    assert_tree(&busy_tree, false)?;
    assert_idle(address, &session);
    assert_other_running(address, &other, &other_execution, &workspace, &other_tree)?;

    prompt(address, &session, "msg_stop_later");
    wait_calls(&provider.calls, 5);
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    assert!(
        history["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row["type"] == "assistant"
                    && row["content"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part["type"] == "text"
                                && part["text"] == "A later prompt completed after stop."
                        })
                    })
            })),
        "later prompt produced no assistant answer: {history}"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
    stop(address, &other, r#"{"stopProcesses":true}"#);
    assert_tree(&other_tree, false)?;
    assert_idle(address, &other);
    product.stop_after_witness(&root);
    Ok(())
}

fn tool(call: &'static str, tool: &'static str, name: &str) -> Script {
    let command = if cfg!(windows) {
        format!("powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File stop-tree.ps1 {name}")
    } else if cfg!(target_os = "linux") && tool == "exec_command" {
        // Job control puts the child in another group within the owned PTY session.
        format!("bash --noprofile --norc -m stop-tree.sh {name}")
    } else {
        format!("sh stop-tree.sh {name}")
    };
    Script::ToolInput {
        call,
        tool,
        arguments: if tool == "shell" {
            serde_json::json!({"command":command,"timeout_ms":60000})
        } else {
            serde_json::json!({"cmd":command,"yield_time_ms":250,"timeout_ms":60000,"tty":cfg!(target_os = "linux")})
        },
    }
}

fn prompt(address: SocketAddr, session: &str, id: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":"exercise session stop","delivery":"queue","resume":true}).to_string(),
    );
}

fn stop(address: SocketAddr, session: &str, body: &str) {
    let (status, response) = request_with_read_timeout(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        body,
        Duration::from_secs(15),
    );
    assert_eq!(
        status,
        204,
        "stop refused: {}",
        String::from_utf8_lossy(&response)
    );
}

fn active_execution(address: SocketAddr, session: &str) -> TestResult<ExecutionId> {
    let active = json(address, "GET", "/api/session/active", "");
    ExecutionId::new(
        active["data"][session]["executionID"]
            .as_str()
            .ok_or_else(|| format!("session is not running: {active}"))?,
    )
    .ok_or_else(|| "active execution ID was empty".into())
}

fn assert_idle(address: SocketAddr, session: &str) {
    let active = json(address, "GET", "/api/session/active", "");
    assert!(
        active["data"]
            .as_object()
            .is_some_and(|rows| !rows.contains_key(session)),
        "204 left the stopped session active: {active}"
    );
}

fn assert_other_running(
    address: SocketAddr,
    session: &str,
    execution: &ExecutionId,
    root: &Path,
    pids: &[u32; 2],
) -> TestResult {
    assert_eq!(&active_execution(address, session)?, execution);
    assert_tree(pids, true)?;
    let path = root.join("other-heartbeat");
    let before = fs::metadata(&path)?.len();
    let wait = PollDeadline::within(Duration::from_secs(3));
    while wait.pending() {
        if fs::metadata(&path)?.len() > before {
            return Ok(());
        }
        thread::sleep(POLL);
    }
    Err("stopping another session halted the control session's heartbeat".into())
}

fn assert_tree(pids: &[u32; 2], running: bool) -> TestResult {
    for pid in pids {
        assert_eq!(process_running(*pid)?, running, "process {pid}");
    }
    Ok(())
}

struct Processes {
    root: PathBuf,
    pids: Vec<u32>,
}

impl Processes {
    fn new(root: &Path) -> TestResult<Self> {
        // Both scripts keep a parent waiting on a child that writes a heartbeat.
        // The release marker also ends them on an assertion failure; loops are bounded.
        if cfg!(windows) {
            fs::write(
                root.join("stop-tree.ps1"),
                r#"param([string]$Name, [switch]$Child)
$ErrorActionPreference = 'Stop'
$role = if ($Child) { 'child' } else { 'parent' }
[IO.File]::WriteAllText((Join-Path $PSScriptRoot "$Name-$role.pid"), [string]$PID)
if (!$Child) {
    & powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $PSCommandPath $Name -Child
    exit $LASTEXITCODE
}
for ($i = 0; $i -lt 1200; $i++) {
    if (!(Test-Path $PSScriptRoot) -or (Test-Path (Join-Path $PSScriptRoot 'release'))) { break }
    [IO.File]::AppendAllText((Join-Path $PSScriptRoot "$Name-heartbeat"), 'x')
    Start-Sleep -Milliseconds 50
}
"#,
            )?;
        } else {
            fs::write(
                root.join("stop-tree.sh"),
                r#"set -eu
name=$1
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
role=${2:-parent}
printf '%s' "$$" > "$root/$name-$role.pid"
if [ "$role" = parent ]; then
    sh "$0" "$name" child &
    wait "$!"
else
    i=0
    while [ "$i" -lt 1200 ] && [ -d "$root" ] && [ ! -e "$root/release" ]; do
        printf x >> "$root/$name-heartbeat"
        sleep 0.05
        i=$((i + 1))
    done
fi
"#,
            )?;
        }
        Ok(Self {
            root: root.to_owned(),
            pids: Vec::new(),
        })
    }

    fn observe(&mut self, name: &str) -> TestResult<[u32; 2]> {
        let heartbeat = self.root.join(format!("{name}-heartbeat"));
        let wait = PollDeadline::within(Duration::from_secs(10));
        loop {
            match fs::metadata(&heartbeat) {
                Ok(metadata) if metadata.len() > 0 => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if !wait.pending() {
                return Err(format!("{name} never started its child heartbeat ({wait})").into());
            }
            thread::sleep(POLL);
        }
        let pids = [
            fs::read_to_string(self.root.join(format!("{name}-parent.pid")))?
                .trim()
                .parse()?,
            fs::read_to_string(self.root.join(format!("{name}-child.pid")))?
                .trim()
                .parse()?,
        ];
        self.pids.extend(pids);
        assert_tree(&pids, true)?;
        #[cfg(target_os = "linux")]
        if name == "idle" {
            let group = |pid| -> TestResult<String> {
                let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
                Ok(stat
                    .rsplit_once(") ")
                    .ok_or("malformed process stat")?
                    .1
                    .split_whitespace()
                    .nth(2)
                    .ok_or("missing process group")?
                    .to_owned())
            };
            assert_ne!(
                group(pids[0])?,
                group(pids[1])?,
                "fixture did not enable job control"
            );
        }
        Ok(pids)
    }
}

impl Drop for Processes {
    fn drop(&mut self) {
        let cleanup = || -> TestResult {
            fs::write(self.root.join("release"), "release")?;
            let wait = PollDeadline::within(Duration::from_secs(3));
            loop {
                let mut running = false;
                for pid in &self.pids {
                    running |= process_running(*pid)?;
                }
                if !running {
                    return Ok(());
                }
                if !wait.pending() {
                    return Err("fixture processes did not honor release".into());
                }
                thread::sleep(POLL);
            }
        };
        if let Err(error) = cleanup() {
            if !thread::panicking() {
                panic!("session-stop fixture cleanup: {error}");
            }
            eprintln!("session-stop fixture cleanup: {error}");
        }
    }
}

#[cfg(target_os = "linux")]
fn process_running(pid: u32) -> TestResult<bool> {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        // A zombie has exited and cannot execute or retain its process tree.
        Ok(stat) => Ok(stat
            .rsplit_once(") ")
            .ok_or("malformed process stat")?
            .1
            .split_whitespace()
            .next()
            .ok_or("missing process state")?
            != "Z"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn process_running(pid: u32) -> TestResult<bool> {
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &format!(
            "$ErrorActionPreference='Stop'; Get-CimInstance Win32_Process -Filter 'ProcessId = {pid}' -ErrorAction Stop | ForEach-Object {{ $_.ProcessId }}"
        )])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "process observation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let observed = std::str::from_utf8(&output.stdout)?.trim();
    if observed.is_empty() {
        return Ok(false);
    }
    assert_eq!(observed.parse::<u32>()?, pid);
    Ok(true)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_running(pid: u32) -> TestResult<bool> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()?;
    if output.status.code() == Some(1) && output.stdout.is_empty() {
        return Ok(false);
    }
    if !output.status.success() {
        return Err("process observation failed".into());
    }
    let state = std::str::from_utf8(&output.stdout)?.trim();
    if state.is_empty() {
        return Err("process observation returned no state".into());
    }
    Ok(!state.starts_with('Z'))
}

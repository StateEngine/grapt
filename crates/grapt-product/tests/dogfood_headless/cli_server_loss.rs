//! WINDOWS-CLI-SERVER-LOSS-01 J01: what the shipped `grapt run` does when its
//! server is killed mid-attempt, observed with its stdout drained to a file and
//! with its stdout deliberately left undrained.
//!
//! This is an instrument, not a verdict. It records elapsed time to exit, the
//! exit code, stdout bytes and stderr lines, and writes them as one JSON record
//! for `scripts/qualification/cli-server-loss`. It is ignored by default and
//! runs only when that harness names it.

use super::cli_continuity::cli;
use super::*;

// The monotonic scheduling clock the workspace admits; never a durable fact.
use tokio::time::Instant;

/// Longer than the client's 30 s request allowance, so a run that waits one
/// allowance out still exits inside the window.
const DRAINED_WINDOW: Duration = Duration::from_secs(75);
/// Undrained phase, then the harness drains and watches for a prompt exit.
const UNDRAINED_WINDOW: Duration = Duration::from_secs(45);
const AFTER_DRAIN_WINDOW: Duration = Duration::from_secs(30);
/// Inherited stdout is drained by the outer harness at a fixed point after
/// launch; this window spans that point.
const INHERITED_AFTER_WINDOW: Duration = Duration::from_secs(60);
const KILL_DELAY: Duration = Duration::from_millis(250);
const EXIT_POLL: Duration = Duration::from_millis(10);
const MAX_CAPTURED_STDOUT: u64 = 16 * 1_024 * 1_024 + 1;
const MAX_CAPTURED_STDERR_LINES: usize = 4_096;
const TAIL_BYTES: usize = 1_024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The same harness on its smallest workload: no kill, the run completes.
    Smoke,
    Drained,
    Undrained,
    /// stdout is the harness's own pipe, which the harness leaves undrained
    /// and then drains, so the pipe is the one a harness of that kind creates.
    Inherited,
    /// stdout undrained; the CLI is sent its termination signal (SIGTERM, or
    /// CTRL_BREAK on Windows) instead of losing its server.
    Signal,
}

impl Mode {
    fn from_environment() -> Self {
        match std::env::var("GRAPT_CLS_OBSERVE_MODE").as_deref() {
            Ok("smoke") => Self::Smoke,
            Ok("drained") => Self::Drained,
            Ok("undrained") => Self::Undrained,
            Ok("inherited") => Self::Inherited,
            Ok("signal") => Self::Signal,
            other => {
                panic!("GRAPT_CLS_OBSERVE_MODE must be smoke, drained, undrained, inherited or signal: {other:?}")
            }
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Drained => "drained",
            Self::Undrained => "undrained",
            Self::Inherited => "inherited",
            Self::Signal => "signal",
        }
    }
}

/// Sends the CLI its termination signal and reports how the sender exited.
fn terminate(child: &Child) -> String {
    let pid = child.id().to_string();
    #[cfg(unix)]
    let sent = Command::new("kill").args(["-TERM", &pid]).status();
    #[cfg(windows)]
    let sent = Command::new("python")
        .args([
            "-c",
            "import os, signal, sys; os.kill(int(sys.argv[1]), signal.CTRL_BREAK_EVENT)",
            &pid,
        ])
        .status();
    format!("{sent:?}")
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(TAIL_BYTES);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Polls the child for exit until `window` passes, and reports when it exited.
fn exit_within(child: &mut Child, window: Duration, clock: Instant) -> Option<(Option<i32>, u64)> {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Some((status.code(), millis(clock))),
            Ok(None) => thread::sleep(EXIT_POLL),
            Err(error) => panic!("observed CLI status failed: {error}"),
        }
    }
    None
}

#[test]
#[ignore = "J01 instrument; run by scripts/qualification/cli-server-loss"]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn observe_cli_server_loss() {
    let mode = Mode::from_environment();
    let out = PathBuf::from(
        std::env::var_os("GRAPT_CLS_OBSERVE_OUT")
            .unwrap_or_else(|| panic!("GRAPT_CLS_OBSERVE_OUT names the observation directory")),
    );
    fs::create_dir_all(&out).unwrap_or_else(|error| panic!("observation dir failed: {error}"));
    let _journey = process_journey_custody();
    let root = root(&format!("cls-observe-{}", mode.label()));
    prepare(&root, 120_000);
    let service = root.to_string_lossy().into_owned();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let second = if mode == Mode::Smoke {
        Script::SuccessText("second answer through the CLI")
    } else {
        Script::HeldOutput {
            bytes: 32,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }
    };
    // Optional larger first answer, so the re-listed page can exceed a small
    // OS pipe buffer before the kill.
    let history_bytes = std::env::var("GRAPT_CLS_HISTORY_BYTES").ok().map(|raw| {
        raw.parse::<usize>()
            .unwrap_or_else(|error| panic!("GRAPT_CLS_HISTORY_BYTES is invalid: {error}"))
    });
    let first = match history_bytes {
        Some(bytes) => Script::Output(bytes),
        None => Script::SuccessText("first answer through the CLI"),
    };
    let provider = ProviderFixture::start(vec![first, second]);

    let mut product = spawn(&root, provider.address);
    await_ready(&root, &mut product);
    // A Session with history, so the in-flight run re-lists a real page.
    let created = cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "--new",
            "--prompt",
            "first task",
            "--jsonl",
        ],
    );
    assert_eq!(
        created.code,
        Some(0),
        "fresh CLI run failed: {}",
        created.stderr
    );
    let session = created.receipt()["session"]
        .as_str()
        .unwrap_or_else(|| panic!("the run receipt named no Session: {}", created.stdout))
        .to_owned();

    let stdout_path = out.join("cli-stdout.jsonl");
    let stdout = match mode {
        Mode::Undrained | Mode::Signal => Stdio::piped(),
        Mode::Inherited => Stdio::inherit(),
        Mode::Smoke | Mode::Drained => Stdio::from(
            fs::File::create(&stdout_path)
                .unwrap_or_else(|error| panic!("stdout file failed: {error}")),
        ),
    };
    let clock = Instant::now();
    let mut command = Command::new(super::cli_continuity::cli_binary());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CTRL_BREAK is delivered to a process group.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    let mut observed = command
        .args([
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "second task",
            "--jsonl",
        ])
        .env("GRAPT_CLS_TRACE", "1")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("observed CLI launch failed: {error}"));
    let stderr_lines = Arc::new(Mutex::new(Vec::<(u64, String)>::new()));
    let stderr_reader = {
        let pipe = observed
            .stderr
            .take()
            .unwrap_or_else(|| panic!("observed CLI has no stderr pipe"));
        let lines = Arc::clone(&stderr_lines);
        thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(pipe).lines() {
                let Ok(line) = line else { break };
                let mut lines = locked(&lines);
                if lines.len() < MAX_CAPTURED_STDERR_LINES {
                    lines.push((millis(clock), line));
                }
            }
        })
    };

    let mut record = serde_json::json!({
        "schema": "grapt.cls.j01-observation.v1",
        "mode": mode.label(),
        "platform": std::env::consts::OS,
        "session": session,
        "history_bytes": history_bytes,
    });
    let mut undrained_pipe = observed.stdout.take();
    let mut drain_started_ms = None;
    let exit = if mode == Mode::Smoke {
        exit_within(&mut observed, DRAINED_WINDOW, clock)
    } else {
        let _ = entered.wait();
        thread::sleep(KILL_DELAY);
        let kill_ms = millis(clock);
        let stdout_at_kill = fs::metadata(&stdout_path).map(|meta| meta.len()).ok();
        if mode == Mode::Signal {
            record["signal_delivery"] = terminate(&observed).into();
        } else {
            product.stop_after_witness(&root);
            let _ = release.wait();
        }
        record["kill_ms"] = kill_ms.into();
        record["stdout_bytes_at_kill"] = stdout_at_kill.into();
        match mode {
            Mode::Drained => exit_within(&mut observed, DRAINED_WINDOW, clock),
            Mode::Inherited => match exit_within(&mut observed, UNDRAINED_WINDOW, clock) {
                Some(exit) => Some(exit),
                None => {
                    record["undrained_window_passed_ms"] = millis(clock).into();
                    exit_within(&mut observed, INHERITED_AFTER_WINDOW, clock)
                }
            },
            _ => match exit_within(&mut observed, UNDRAINED_WINDOW, clock) {
                Some(exit) => Some(exit),
                None => {
                    drain_started_ms = Some(millis(clock));
                    let pipe = undrained_pipe
                        .take()
                        .unwrap_or_else(|| panic!("undrained CLI has no stdout pipe"));
                    let drain = thread::spawn(move || {
                        let mut bytes = Vec::new();
                        let _ = pipe.take(MAX_CAPTURED_STDOUT).read_to_end(&mut bytes);
                        bytes
                    });
                    let exit = exit_within(&mut observed, AFTER_DRAIN_WINDOW, clock);
                    if exit.is_none() {
                        let _ = observed.kill();
                        let _ = observed.wait();
                    }
                    let bytes = drain.join().unwrap_or_default();
                    record["stdout_bytes"] = bytes.len().into();
                    record["stdout_tail"] = tail(&bytes).into();
                    exit
                }
            },
        }
    };
    match exit {
        Some((code, exit_ms)) => {
            record["exited_by_itself"] = true.into();
            record["exit_code"] = code.into();
            record["exit_ms"] = exit_ms.into();
        }
        None => {
            record["exited_by_itself"] = false.into();
            let _ = observed.kill();
            let _ = observed.wait();
        }
    }
    record["drain_started_ms"] = drain_started_ms.into();
    if let Some(pipe) = undrained_pipe.take() {
        let mut bytes = Vec::new();
        let _ = pipe.take(MAX_CAPTURED_STDOUT).read_to_end(&mut bytes);
        record["stdout_bytes"] = bytes.len().into();
        record["stdout_tail"] = tail(&bytes).into();
    }
    if matches!(mode, Mode::Smoke | Mode::Drained) {
        let bytes = fs::read(&stdout_path).unwrap_or_default();
        record["stdout_bytes"] = bytes.len().into();
        record["stdout_tail"] = tail(&bytes).into();
    }
    let _ = stderr_reader.join();
    record["stderr"] = locked(&stderr_lines)
        .iter()
        .map(|(at, line)| serde_json::json!({"ms": at, "line": line}))
        .collect::<Vec<_>>()
        .into();
    fs::write(
        out.join("observation.json"),
        serde_json::to_vec_pretty(&record)
            .unwrap_or_else(|error| panic!("observation encode failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("observation write failed: {error}"));

    if mode == Mode::Smoke {
        product.stop_after_witness(&root);
    }
    drop(product);
    drop(provider);
    process_fixture::cleanup(&root);
}

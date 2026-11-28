//! The one owner of a `grapt acp` process for these journeys: it speaks ACP
//! over the CLI's stdio and, through success, refusal, timeout, panic or an
//! early return, takes the CLI and the product it embedded down with it.
#![cfg(unix)]

use super::*;
use std::time::Instant;

/// This fixture's wall-clock deadlines; journeys driving an `AcpProcess`
/// derive theirs from this one reading rather than a clock of their own.
#[allow(clippy::disallowed_methods)]
pub(super) fn now() -> Instant {
    Instant::now()
}

/// What an ACP client saw for one `session/prompt`.
#[derive(Debug, Default)]
pub(super) struct AcpObserved {
    pub(super) updates: Vec<String>,
    pub(super) update_values: Vec<serde_json::Value>,
    /// Each update's text content, aligned with `updates` (empty for none).
    pub(super) texts: Vec<String>,
    pub(super) permissions: usize,
    pub(super) outcome: String,
    /// Whether every process the turn started (the CLI and the product it
    /// embedded) was gone after cleanup.
    pub(super) cleaned_up: bool,
    /// From the turn's start, once it holds journey custody, to the end of
    /// its cleanup.
    pub(super) elapsed: Duration,
    /// Provider requests the turn's cut left unfinished (SC-TEST-01).
    pub(super) truncated_requests: Vec<String>,
    /// The product's record of why it ended live streams
    /// (diagnostics/event-stream.jsonl), when a witness read it.
    pub(super) stream_ends: String,
}

/// The most ACP frames, and bytes in one frame, a turn may produce here.
pub(super) const ACP_FRAMES: usize = 4096;
const ACP_FRAME_BYTES: u64 = 1024 * 1024;

/// One `grapt acp --embedded` and everything it starts. It runs in its own
/// process group, so cleanup reaches the product it embedded (killing the CLI
/// alone would skip its kill-on-drop); dropping this always cleans up.
/// Its stdin and stdout are served by threads it owns, so neither a CLI that
/// stops reading nor a queue nobody drains can hold a turn or its cleanup.
#[cfg(unix)]
pub(super) struct AcpProcess {
    child: Child,
    /// Rows for the stdin writer; dropping it closes the CLI's stdin.
    input: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    /// The writer's result for each row, in order.
    written: std::sync::mpsc::Receiver<Result<(), String>>,
    writer: Option<thread::JoinHandle<()>>,
    frames: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    reader: Option<thread::JoinHandle<()>>,
    /// Whether the last cleanup found the whole group gone and both threads
    /// joined; never set by a probe that did not run.
    pub(super) cleaned_up: bool,
}

#[cfg(unix)]
impl AcpProcess {
    #[cfg(target_os = "linux")]
    pub(super) fn close_input_and_wait(&mut self) {
        self.input = None;
        let wait = PollDeadline::within(Duration::from_secs(25));
        let status = wait.poll(Duration::from_millis(20), || {
            self.child
                .try_wait()
                .unwrap_or_else(|error| panic!("ACP EOF wait failed: {error}"))
        });
        assert!(
            status.is_some_and(|status| status.success()),
            "ACP protocol EOF did not exit successfully within 25 seconds: {status:?}"
        );
    }

    /// Kill only the ACP parent. Group cleanup remains a failure backstop;
    /// it must run after the child-lifetime witness records its result.
    #[cfg(target_os = "linux")]
    pub(super) fn kill_parent(&mut self) {
        self.child
            .kill()
            .unwrap_or_else(|error| panic!("ACP parent SIGKILL failed: {error}"));
    }

    #[cfg(target_os = "linux")]
    pub(super) fn terminate_parent(&mut self) {
        use std::os::unix::process::ExitStatusExt;
        let sent = Command::new("kill")
            .args(["-TERM", "--", &self.child.id().to_string()])
            .status()
            .unwrap_or_else(|error| panic!("ACP parent SIGTERM failed: {error}"));
        assert!(sent.success(), "ACP parent SIGTERM refused: {sent}");
        let wait = PollDeadline::within(Duration::from_secs(2));
        let status = wait.poll(Duration::from_millis(10), || {
            self.child
                .try_wait()
                .unwrap_or_else(|error| panic!("ACP parent wait failed: {error}"))
        });
        assert!(
            status.is_some_and(|status| status.signal() == Some(15)),
            "ACP parent did not exit from SIGTERM: {status:?}"
        );
    }

    pub(super) fn start(root: &Path, provider: SocketAddr, frame_cap: usize) -> Self {
        let (home, config) = (root.join("home"), root.join("global-config"));
        fs::create_dir_all(&home).unwrap_or_else(|error| panic!("{error}"));
        fs::create_dir_all(&config).unwrap_or_else(|error| panic!("{error}"));
        let mut command = Command::new(super::cli_continuity::cli_binary());
        command
            .args(["acp", "--embedded", "--product"])
            .arg(process_fixture::installed_product_binary())
            .arg("--service")
            .arg(root)
            .env("GRAPT_LIVE_HEADLESS_LOOPBACK", format!("http://{provider}"))
            .env("GRAPT_TEST_HOME", &home)
            .env("GRAPT_CONFIG_DIR", &config);
        Self::spawn(command, frame_cap)
    }

    /// Run `command` in its own process group, speaking ACP over its stdio.
    pub(super) fn spawn(mut command: Command, frame_cap: usize) -> Self {
        use std::io::BufRead;
        use std::os::unix::process::CommandExt;
        let mut child = command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Not read by the turn; discarded so it can never fill and stall.
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("acp spawn failed: {error}"));
        let mut stdin = child.stdin.take().unwrap_or_else(|| panic!("acp stdin"));
        // One row is in flight at a time, so each queue holds one; neither
        // side ever blocks handing over.
        let (input, rows) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let (wrote, written) = std::sync::mpsc::sync_channel(1);
        let writer = thread::spawn(move || {
            // Ends when `input` is dropped, closing stdin, or on a failed write.
            for row in rows {
                let result = stdin
                    .write_all(&row)
                    .and_then(|()| stdin.flush())
                    .map_err(|error| format!("acp write failed: {error}"));
                let failed = result.is_err();
                if wrote.try_send(result).is_err() || failed {
                    return;
                }
            }
        });
        let output = child.stdout.take().unwrap_or_else(|| panic!("acp stdout"));
        let (sender, frames) = std::sync::mpsc::sync_channel(64);
        let reader = thread::spawn(move || {
            let mut output = std::io::BufReader::new(output);
            for _ in 0..frame_cap {
                let mut line = Vec::new();
                let read = (&mut output)
                    .take(ACP_FRAME_BYTES + 1)
                    .read_until(b'\n', &mut line);
                let frame = match read {
                    Ok(0) | Err(_) => return,
                    Ok(_) if line.len() as u64 > ACP_FRAME_BYTES => {
                        Err("frame byte bound exceeded".to_owned())
                    }
                    Ok(_) => Ok(String::from_utf8_lossy(&line).into_owned()),
                };
                let failed = frame.is_err();
                if sender.send(frame).is_err() || failed {
                    return;
                }
            }
            let _ = sender.send(Err("frame count bound exceeded".to_owned()));
        });
        Self {
            child,
            input: Some(input),
            written,
            writer: Some(writer),
            frames: Some(frames),
            reader: Some(reader),
            cleaned_up: false,
        }
    }

    /// Write one row, waiting for the writer no later than `deadline`.
    pub(super) fn send(&mut self, row: serde_json::Value, deadline: Instant) -> Result<(), String> {
        let input = self.input.as_ref().ok_or("acp stdin closed")?;
        input
            .try_send(format!("{row}\n").into_bytes())
            .map_err(|error| match error {
                std::sync::mpsc::TrySendError::Full(_) => "acp stdin: a row is already pending",
                std::sync::mpsc::TrySendError::Disconnected(_) => "acp stdin closed",
            })?;
        match self
            .written
            .recv_timeout(deadline.saturating_duration_since(now()))
        {
            Ok(result) => result,
            Err(_) => {
                // The row may still be pending, so nothing may follow it.
                self.input = None;
                Err("acp write: deadline passed or writer ended".to_owned())
            }
        }
    }

    /// One request, answering permission asks "allow once" as the benchmark
    /// client does, until its response or the absolute `deadline`.
    pub(super) fn request(
        &mut self,
        id: u64,
        method: &str,
        params: serde_json::Value,
        deadline: Instant,
        observed: &mut AcpObserved,
    ) -> Result<serde_json::Value, String> {
        self.request_observing(id, method, params, deadline, observed, |_| {})
    }

    /// Observe each real protocol row before its ordinary handling.
    pub(super) fn request_observing(
        &mut self,
        id: u64,
        method: &str,
        params: serde_json::Value,
        deadline: Instant,
        observed: &mut AcpObserved,
        observe: impl FnMut(&serde_json::Value),
    ) -> Result<serde_json::Value, String> {
        if now() >= deadline {
            return Err(format!("{method}: deadline passed"));
        }
        self.send(
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            deadline,
        )?;
        self.response_observing(id, method, deadline, observed, observe)
    }

    /// Read a submitted request, allowing a witness to arm its deadline only
    /// after the provider confirms that the request reached its held response.
    pub(super) fn response_observing(
        &mut self,
        id: u64,
        method: &str,
        deadline: Instant,
        observed: &mut AcpObserved,
        mut observe: impl FnMut(&serde_json::Value),
    ) -> Result<serde_json::Value, String> {
        let expired = || format!("{method}: deadline passed");
        loop {
            let frames = self.frames.as_ref().ok_or("acp stdout closed")?;
            let line = frames
                .recv_timeout(deadline.saturating_duration_since(now()))
                .map_err(|_| format!("{method}: deadline passed or stream ended"))??;
            // A frame queued in time is still not used once the deadline has
            // passed: a zero timeout returns whatever is already queued.
            if now() >= deadline {
                return Err(expired());
            }
            let row: serde_json::Value = serde_json::from_str(&line)
                .map_err(|error| format!("bad frame {error}: {line}"))?;
            observe(&row);
            if row["method"] == "session/update" {
                observed.update_values.push(row["params"]["update"].clone());
                observed.updates.push(
                    row["params"]["update"]["sessionUpdate"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
                observed.texts.push(
                    row["params"]["update"]["content"]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
            } else if row["method"] == "session/request_permission" {
                observed.permissions += 1;
                self.send(serde_json::json!({"jsonrpc": "2.0", "id": row["id"], "result": {"outcome": {"outcome": "selected", "optionId": "once"}}}), deadline)?;
            } else if row["id"] == id {
                return match row.get("error") {
                    Some(error) => Err(error.to_string()),
                    None => Ok(row["result"].clone()),
                };
            }
        }
    }

    /// Close stdin (the CLI then stops its product), wait briefly, then kill
    /// whatever is left in the group and confirm the group is empty.
    pub(super) fn cleanup(&mut self) {
        if self.reader.is_none() {
            return;
        }
        // Ends the writer, which closes stdin once any pending row resolves.
        self.input = None;
        // Nothing is consumed from here on, so a reader blocked on a full
        // queue must fail its send instead of waiting forever.
        self.frames = None;
        let group = format!("-{}", self.child.id());
        let quit_by = now() + Duration::from_secs(3);
        while matches!(self.child.try_wait(), Ok(None)) && now() < quit_by {
            thread::sleep(Duration::from_millis(20));
        }
        let _ = Command::new("kill")
            .args(["-KILL", "--", &group])
            .stderr(Stdio::null())
            .status();
        // The CLI itself, even if the group kill did not run; it is reaped
        // within a bound rather than waited on.
        let _ = self.child.kill();
        let reaped_by = now() + Duration::from_secs(2);
        let reaped = loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break true,
                Ok(None) if now() < reaped_by => thread::sleep(Duration::from_millis(10)),
                _ => break false,
            }
        };
        let empty_by = now() + Duration::from_secs(5);
        let mut empty = false;
        loop {
            match Command::new("kill")
                .args(["-0", "--", &group])
                .stderr(Stdio::null())
                .status()
            {
                Ok(status) if !status.success() => {
                    empty = true;
                    break;
                }
                Ok(_) => {}
                // A probe that did not run proves nothing about the group.
                Err(_) => break,
            }
            if now() > empty_by {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // With the group gone and the queue dropped, both threads end; they
        // are not waited on past their bound.
        let joined_by = now() + Duration::from_secs(2);
        let reader = joined(self.reader.take(), joined_by);
        let writer = joined(self.writer.take(), joined_by);
        self.cleaned_up = reaped && empty && reader && writer;
    }
}

/// Join `handle` if it finishes by `by`; otherwise leave it detached.
#[cfg(unix)]
fn joined(handle: Option<thread::JoinHandle<()>>, by: Instant) -> bool {
    let Some(handle) = handle else {
        return true;
    };
    while !handle.is_finished() {
        if now() > by {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    handle.join().is_ok()
}

#[cfg(unix)]
impl Drop for AcpProcess {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// A stand-in ACP CLI, run in `root`: writes `frames` to stdout, then runs
/// the shell text `then`.
#[cfg(unix)]
fn scripted_peer(case: &str, frames: &[serde_json::Value], then: &str) -> (PathBuf, AcpProcess) {
    let root = root(case);
    process_fixture::cleanup(&root);
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("{error}"));
    let script = root.join("frames.jsonl");
    let rows: String = frames.iter().map(|frame| format!("{frame}\n")).collect();
    fs::write(&script, rows).unwrap_or_else(|error| panic!("fixture file: {error}"));
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .current_dir(&root)
        .arg(format!("cat \"$1\"; {then}"))
        .arg("sh")
        .arg(&script);
    (root, AcpProcess::spawn(command, ACP_FRAMES))
}

#[cfg(unix)]
#[test]
fn an_acp_peer_that_fills_the_frame_queue_is_still_cleaned_up_promptly() {
    // 128 frames against a 64-slot queue nobody drains: the reader is left
    // blocked handing over a frame when cleanup starts.
    let update = serde_json::json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {"sessionUpdate": "agent_message_chunk"}}});
    let (root, mut acp) =
        scripted_peer("acp-queue-full", &vec![update; 128], "exec cat >/dev/null");
    thread::sleep(Duration::from_millis(500));
    let started = now();
    acp.cleanup();
    let elapsed = started.elapsed();
    assert!(
        acp.cleaned_up,
        "queue-full cleanup left a process or thread"
    );
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    drop(acp);
    process_fixture::cleanup(&root);
}

#[cfg(unix)]
#[test]
fn a_frame_queued_before_the_deadline_is_not_used_after_it() {
    // The response is already queued, but the deadline has passed.
    let response = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {}});
    let (root, mut acp) = scripted_peer("acp-expired", &[response], "exec cat >/dev/null");
    thread::sleep(Duration::from_millis(200));
    let deadline = now();
    let mut observed = AcpObserved::default();
    let outcome = acp.request(
        1,
        "initialize",
        serde_json::json!({}),
        deadline,
        &mut observed,
    );
    assert_eq!(outcome, Err("initialize: deadline passed".to_owned()));
    acp.cleanup();
    assert!(
        acp.cleaned_up,
        "expired-request cleanup left a process or thread"
    );
    drop(acp);
    process_fixture::cleanup(&root);
}

#[cfg(unix)]
#[test]
fn an_acp_peer_that_stops_reading_stdin_cannot_hold_a_reply_past_the_deadline() {
    // A permission ask whose reply (it echoes the 200000-byte id) is larger
    // than the stdin pipe, from a peer that never reads stdin.
    let ask = serde_json::json!({"jsonrpc": "2.0", "id": "x".repeat(200_000), "method": "session/request_permission", "params": {}});
    let (root, mut acp) = scripted_peer("acp-undrained", &[ask], "exec sleep 30");
    // Long enough for a loaded host to deliver the ask, far short of the
    // peer's 30 s: the reply write is what the deadline must cut.
    let deadline = Duration::from_secs(2);
    let started = now();
    let mut observed = AcpObserved::default();
    let outcome = acp.request(
        1,
        "initialize",
        serde_json::json!({}),
        started + deadline,
        &mut observed,
    );
    let answered = started.elapsed();
    assert!(
        outcome
            .as_ref()
            .is_err_and(|error| error.contains("deadline passed")),
        "{outcome:?}"
    );
    assert_eq!(observed.permissions, 1);
    assert!(answered < deadline + Duration::from_secs(2), "{answered:?}");
    acp.cleanup();
    assert!(
        acp.cleaned_up,
        "undrained-stdin cleanup left a process or thread"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    drop(acp);
    process_fixture::cleanup(&root);
}

#[cfg(target_os = "linux")]
#[test]
fn a_cli_that_ignores_stdin_eof_is_killed_with_the_product_it_embedded() {
    // The stand-in never reads stdin, so closing it stops nothing, and it has
    // started a child of its own, as the CLI starts the product it embeds.
    let (root, mut acp) = scripted_peer(
        "acp-eof-ignored",
        &[],
        "sleep 300 & echo $! > embedded.pid; exec sleep 300",
    );
    let started_by = now() + Duration::from_secs(5);
    let embedded = loop {
        match fs::read_to_string(root.join("embedded.pid")) {
            Ok(pid) if pid.ends_with('\n') => break pid.trim().to_owned(),
            _ => {}
        }
        assert!(now() < started_by, "the embedded stand-in never started");
        thread::sleep(Duration::from_millis(10));
    };
    let process = PathBuf::from(format!("/proc/{embedded}"));
    assert!(process.exists(), "embedded child {embedded} is not running");
    let started = now();
    acp.cleanup();
    assert!(
        acp.cleaned_up,
        "EOF-ignoring cleanup left a process or thread"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    // Gone and reaped, not merely signalled: no entry, not even a zombie.
    let reaped_by = now() + Duration::from_secs(5);
    while process.exists() {
        assert!(
            now() < reaped_by,
            "embedded child {embedded} outlived cleanup"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(acp);
    process_fixture::cleanup(&root);
}

//! A real terminal hangup must stop work before silent-loss recovery runs.
use super::*;
use grapt_tui_driver::{session_name, Launch, Session, Tmux};

#[path = "tmux_hangup/blocked_output.rs"]
mod blocked_output;

struct Window(Option<Session>, Option<rustix::fd::OwnedFd>);

impl Window {
    fn open(root: &std::path::Path, watched: &str) -> TestResult<(Self, std::path::PathBuf)> {
        let home = root.join("terminal-home");
        fs::create_dir(&home)?;
        let pid_file = root.join("window.pid");
        let mut launch = Launch::new(vec![
            "sh".into(),
            "-c".into(),
            "printf '%s' \"$$\" > \"$1\"; errors=$2; shift 2; exec \"$@\" 2>\"$errors\"".into(),
            "window".into(),
            pid_file.to_string_lossy().into_owned(),
            root.join("window.stderr").to_string_lossy().into_owned(),
            crate::tui_busy_seat::tui_binary()
                .to_string_lossy()
                .into_owned(),
            "--service".into(),
            root.to_string_lossy().into_owned(),
            "--session".into(),
            watched.to_owned(),
            "--prompt".into(),
            "hold this task until the terminal closes".into(),
        ]);
        launch.directory = Some(root.to_path_buf());
        for key in ["HOME", "GRAPT_TEST_HOME", "GRAPT_CONFIG_DIR"] {
            launch
                .environment
                .push((key.into(), home.to_string_lossy().into_owned()));
        }
        let name = session_name("test", &format!("hangup-{}", std::process::id()))?;
        let tmux = Tmux::default().with_socket(&name)?;
        Ok((Self(Some(tmux.start(&name, &launch)?), None), pid_file))
    }

    fn wait_running(&self, calls: &AtomicUsize) -> TestResult {
        let pane = self.0.as_ref().ok_or("terminal window absent")?;
        let deadline = PollDeadline::within(Duration::from_secs(20));
        while calls.load(Ordering::SeqCst) == 0 {
            if pane.exit_status()?.is_some() || !deadline.pending() {
                return Err(format!(
                    "TUI did not submit its task: {}",
                    pane.capture_with_history()?
                )
                .into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn hangup(&mut self) -> TestResult {
        self.0.take().ok_or("terminal window absent")?.stop()?;
        Ok(())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            let _ = session.stop();
        }
        if let Some(process) = &self.1 {
            let _ = rustix::process::pidfd_send_signal(process, rustix::process::Signal::KILL);
        }
    }
}

#[test]
fn a_tmux_hangup_stops_work_before_the_silent_loss_grace() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("tui-terminal-hangup");
    prepare(&root, 120_000);
    lease::set_close_grace(&root, 60_000);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    let release = Arc::new(terminal_coordination::Phase::new());
    let provider = ProviderFixture::start(vec![Script::ControlledTerminalOutput {
        bytes: 64,
        entered: Arc::new(terminal_coordination::Phase::new()),
        release: Arc::clone(&release),
        disconnect: Arc::new(AtomicBool::new(true)),
    }]);
    let mut product = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let watched = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({
            "agent":"agt_live_headless1", "location":{"directory":root},
            "model":{"providerID":"openai","id":"gpt-live"},
        })
        .to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("Session identity absent")?
        .to_owned();
    let (mut window, pid_file) = Window::open(&root, &watched)?;
    window.wait_running(&provider.calls)?;
    wait_active(address, &watched);
    let pid = fs::read_to_string(pid_file)?.parse::<u32>()?;
    window.1 = Some(rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(i32::try_from(pid)?).ok_or("invalid terminal pid")?,
        rustix::process::PidfdFlags::empty(),
    )?);
    {
        let arguments = fs::read(format!("/proc/{pid}/cmdline"))?;
        let binary = crate::tui_busy_seat::tui_binary();
        assert!(arguments
            .split(|byte| *byte == 0)
            .any(|arg| arg == binary.as_os_str().as_encoded_bytes()));
    }
    prompt(address, &watched, "msg_terminal_close_queued");
    let closed = tokio::time::Instant::now();
    window.hangup()?;
    assert!(
        PollDeadline::within(Duration::from_secs(10))
            .poll(Duration::from_millis(50), || {
                let active = json(address, "GET", "/api/session/active", "");
                active["data"]
                    .as_object()
                    .is_some_and(|rows| !rows.contains_key(&watched))
                    .then_some(())
            })
            .is_some(),
        "work survived hangup: exited={}, stderr={:?}, active={}",
        window_exited(pid),
        fs::read_to_string(root.join("window.stderr"))?,
        json(address, "GET", "/api/session/active", ""),
    );
    assert!(closed.elapsed() < Duration::from_secs(60));
    assert!(
        PollDeadline::within(Duration::from_secs(5))
            .poll(Duration::from_millis(10), || {
                window_exited(pid).then_some(())
            })
            .is_some(),
        "the terminal process kept its event subscription after hangup"
    );
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{watched}/pending"),
        "",
    );
    assert!(pending["data"].as_array().is_some_and(|rows| rows
        .iter()
        .any(|row| row["id"] == "msg_terminal_close_queued")));
    release.release();
    thread::sleep(Duration::from_secs(2));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    product.stop_after_witness(&root);
    Ok(())
}

fn window_exited(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, state)| matches!(state.split_whitespace().next(), Some("Z" | "X"))),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

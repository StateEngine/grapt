//! A killed window cannot send close or run its socket destructor.
use super::*;

fn hold_window(path: &Path) -> TestResult {
    let request: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let address = request["address"].as_str().ok_or("window address absent")?;
    let session = request["session"].as_str().ok_or("window session absent")?;
    let _window = Window::open(address.parse()?, session);
    fs::write(path.with_extension("ready"), b"attached")?;
    // The parent holds stdin open until it kills this exact child. No
    // interrupt request or Window::drop can run after SIGKILL.
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte)?;
    Err("window was released instead of killed".into())
}

#[test]
fn a_killed_window_stops_only_after_the_silent_loss_grace() -> TestResult {
    const WINDOW: &str = "GRAPT_FIXTURE_KILLED_WINDOW";
    if let Some(path) = std::env::var_os(WINDOW) {
        return hold_window(Path::new(&path));
    }

    let _journey = process_journey_custody();
    let root = root("close-killed-window");
    prepare(&root, 120_000);
    let grace = Duration::from_secs(5);
    lease::set_close_grace(&root, 5_000);
    let _root_cleanup = terminal_coordination::RootCleanup(root.clone());
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let provider = ProviderFixture::start(vec![Script::ControlledTerminalOutput {
        bytes: 64,
        entered,
        release: Arc::clone(&release),
        disconnect: Arc::new(AtomicBool::new(true)),
    }]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let watched = session(address);
    let request = root.join("window.json");
    fs::write(
        &request,
        serde_json::to_vec(&serde_json::json!({
            "address": address.to_string(), "session": watched,
        }))?,
    )?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "close_grace::process_loss::a_killed_window_stops_only_after_the_silent_loss_grace",
            "--nocapture",
        ])
        .env(WINDOW, &request)
        .stdin(Stdio::piped());
    let mut window = process_fixture::spawn_captured(&root, &mut command);
    let ready = PollDeadline::new();
    while !request.with_extension("ready").exists() {
        assert!(
            window.try_wait()?.is_none(),
            "window exited before attaching"
        );
        assert!(ready.pending(), "window never established its lease");
        thread::sleep(Duration::from_millis(10));
    }
    prompt(address, &watched, "msg_killed_window_running");
    wait_calls(&provider.calls, 1);
    wait_active(address, &watched);
    prompt(address, &watched, "msg_killed_window_queued");

    let killed_at = tokio::time::Instant::now();
    window.kill()?;
    let status = window.wait()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9), "window did not die by SIGKILL");
    }
    #[cfg(not(unix))]
    assert!(
        !status.success(),
        "window exited normally instead of being killed"
    );
    loop {
        let active = json(address, "GET", "/api/session/active", "");
        if !active["data"]
            .as_object()
            .is_some_and(|rows| rows.contains_key(&watched))
        {
            assert!(
                killed_at.elapsed() >= grace,
                "silent process loss stopped the turn before its grace: {active}"
            );
            break;
        }
        assert!(
            killed_at.elapsed() < grace + Duration::from_secs(15),
            "killed window's turn never stopped: {active}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{watched}/pending"),
        "",
    );
    assert!(
        pending["data"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == "msg_killed_window_queued")),
        "silent loss discarded or promoted queued input: {pending}"
    );
    release.release();
    thread::sleep(Duration::from_secs(2));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    product.stop_after_witness(&root);
    Ok(())
}

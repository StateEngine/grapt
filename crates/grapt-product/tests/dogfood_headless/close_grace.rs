//! F23: close means closed. A turn whose last window went away stops, and no
//! further provider calls arrive after it.
use super::*;

#[path = "close_grace/lease.rs"]
pub(super) mod lease;

#[path = "close_grace/process_loss.rs"]
mod process_loss;

#[cfg(target_os = "linux")]
#[path = "close_grace/tmux_hangup.rs"]
mod tmux_hangup;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// One HTTP window on `/api/event`, named for the Session it watches.
pub(super) struct Window {
    stream: TcpStream,
    drain: Option<thread::JoinHandle<()>>,
}

impl Window {
    /// Open a named window and wait until the service has answered its head, so
    /// the attachment exists before the caller prompts.
    pub(super) fn open(address: SocketAddr, session: &str) -> Self {
        let mut stream =
            TcpStream::connect(address).unwrap_or_else(|error| panic!("window connect: {error}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|error| panic!("window timeout: {error}"));
        write!(
            stream,
            "GET /api/event?session={session} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nConnection: close\r\n\r\n"
        )
        .unwrap_or_else(|error| panic!("window request: {error}"));
        let mut head = Vec::new();
        for _ in 0..16_384 {
            let mut byte = [0_u8; 1];
            stream
                .read_exact(&mut byte)
                .unwrap_or_else(|error| panic!("window handshake: {error}"));
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"),
            "window refused: {}",
            String::from_utf8_lossy(&head)
        );
        let mut reader = stream
            .try_clone()
            .unwrap_or_else(|error| panic!("window reader: {error}"));
        reader
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap_or_else(|error| panic!("window drain timeout: {error}"));
        let drain = thread::spawn(move || {
            let mut buffer = [0_u8; 8_192];
            for _ in 0..36_000 {
                match reader.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) => {}
                    Err(_) => return,
                }
            }
        });
        Self {
            stream,
            drain: Some(drain),
        }
    }

    /// Close the window the way a quit or a hangup does: drop the socket.
    fn close(mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if let Some(drain) = self.drain.take() {
            if drain.join().is_err() && !thread::panicking() {
                panic!("window drain panicked");
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if let Some(drain) = self.drain.take() {
            let _ = drain.join();
        }
    }
}

fn session(address: SocketAddr) -> String {
    json(
        address,
        "POST",
        "/api/session",
        r#"{"agent":"agt_live_headless1"}"#,
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned()
}

fn prompt(address: SocketAddr, session: &str, id: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":"keep working until the window closes","delivery":"queue","resume":true}).to_string(),
    );
}

/// Wait until the turn is registered as active, so the close lands on a turn
/// that is actually running rather than one that has not started.
fn wait_active(address: SocketAddr, session: &str) {
    let wait = PollDeadline::new();
    loop {
        let active = json(address, "GET", "/api/session/active", "");
        if active["data"]
            .as_object()
            .is_some_and(|rows| rows.contains_key(session))
        {
            return;
        }
        assert!(
            wait.pending(),
            "the session never became active; last read {active}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_idle(address: SocketAddr, session: &str) {
    // A silent loss is only stopped once the whole grace has run out. These
    // journeys configure a short grace, so this exceeds it without hiding a
    // turn that never stopped.
    wait_idle_within(address, session, Duration::from_secs(45));
}

#[test]
fn closing_the_last_window_stops_the_turn_and_no_provider_call_follows() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("close-grace");
    prepare(&root, 120_000);
    // This journey must wait out a whole silent-loss grace, so it runs a short
    // one; the product default is covered by the registry's unit tests.
    lease::set_close_grace(&root, 5_000);
    let _root_cleanup = terminal_coordination::RootCleanup(root.clone());
    // The provider holds its answer, so the turn is still in flight when the
    // window closes. It never writes after release: once the client is gone a
    // real provider's write fails and the server simply drops the connection,
    // and `disconnect` models exactly that instead of treating it as a crash.
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let disconnect = Arc::new(AtomicBool::new(true));
    let provider = ProviderFixture::start(vec![Script::ControlledTerminalOutput {
        bytes: 64,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        disconnect: Arc::clone(&disconnect),
    }]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);

    let watched = session(address);
    let window = Window::open(address, &watched);
    prompt(address, &watched, "msg_close_watched");
    wait_calls(&provider.calls, 1);
    wait_active(address, &watched);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "the turn issued its first provider call"
    );
    // Admitted while the turn runs, so it waits behind it. A close that only
    // signals the registered execution leaves this input to be promoted at the
    // next boundary, running a turn with no window left to watch it.
    prompt(address, &watched, "msg_close_behind");

    // The last window goes. Nothing announces it, so the service learns it from
    // the subscription ending, and the turn stops.
    window.close();
    wait_idle(address, &watched);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "a closed window let the turn issue another provider call"
    );
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "input queued behind the closed turn ran with no window watching"
    );
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{watched}/pending"),
        "",
    );
    assert!(
        pending["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == "msg_close_behind")),
        "the close discarded admitted input instead of leaving it queued: {pending}"
    );
    // Let the provider finish its answer after the stop. A turn that had not
    // stopped would read it and call the provider again; the count below would
    // show that.
    release.release();
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "an answer released after the close reached the provider again"
    );

    // The Session is left resumable, not deleted or wedged.
    let history = json(
        address,
        "GET",
        &format!("/api/session/{watched}/message"),
        "",
    );
    assert!(
        history["data"].is_array(),
        "a stopped session must still serve its history: {history}"
    );
    // Let the held provider worker finish before the fixture is torn down; it
    // models a provider whose client has already gone, so it writes nothing.
    // Its phase wait observes the fixture's stop flag before its release flag,
    // so tearing down in the same breath can cancel a worker the release freed.
    release.release();
    thread::sleep(Duration::from_secs(2));
    product.stop_after_witness(&root);
    Ok(())
}

#[test]
fn a_session_with_two_windows_keeps_running_until_the_last_one_goes() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("close-grace-two-windows");
    prepare(&root, 120_000);
    // This journey must wait out a whole silent-loss grace, so it runs a short
    // one. The product default is covered by the registry's unit tests.
    lease::set_close_grace(&root, 5_000);
    let _root_cleanup = terminal_coordination::RootCleanup(root.clone());
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let disconnect = Arc::new(AtomicBool::new(true));
    let provider = ProviderFixture::start(vec![Script::ControlledTerminalOutput {
        bytes: 64,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        disconnect: Arc::clone(&disconnect),
    }]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);

    let watched = session(address);
    let first = Window::open(address, &watched);
    let second = Window::open(address, &watched);
    prompt(address, &watched, "msg_close_two_windows");
    wait_calls(&provider.calls, 1);
    wait_active(address, &watched);

    // One of two windows goes. The other still watches, so the turn must not
    // stop and no grace may start.
    first.close();
    thread::sleep(Duration::from_secs(2));
    let active = json(address, "GET", "/api/session/active", "");
    assert!(
        active["data"]
            .as_object()
            .is_some_and(|rows| rows.contains_key(&watched)),
        "closing one of two windows stopped the running turn: {active}"
    );

    // The last window goes, so the turn stops after the grace.
    second.close();
    wait_idle(address, &watched);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "a turn with no window left issued another provider call"
    );

    let history = json(
        address,
        "GET",
        &format!("/api/session/{watched}/message"),
        "",
    );
    assert!(
        history["data"].is_array(),
        "a stopped session must still serve its history: {history}"
    );
    // Release the held answer so the provider worker finishes before teardown;
    // a stopped turn must not read it, which the call count already witnessed.
    // The worker's phase wait observes the fixture's stop flag before its
    // release flag, so tearing down in the same breath can cancel a worker the
    // release had already freed.
    release.release();
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "a turn with no window left read its held answer and called again"
    );
    product.stop_after_witness(&root);
    Ok(())
}

/// An explicit close is a durable fact sent before the window exits, so it must
/// not wait for the 15 s grace a silent disappearance needs. The window names
/// the Service, stops the turn, and only then drops its socket.
#[test]
fn an_explicit_close_stops_the_turn_before_the_grace_expires() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("close-explicit");
    prepare(&root, 120_000);
    let _root_cleanup = terminal_coordination::RootCleanup(root.clone());
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let disconnect = Arc::new(AtomicBool::new(true));
    let provider = ProviderFixture::start(vec![Script::ControlledTerminalOutput {
        bytes: 64,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        disconnect: Arc::clone(&disconnect),
    }]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);

    let watched = session(address);
    let window = Window::open(address, &watched);
    prompt(address, &watched, "msg_close_explicit");
    wait_calls(&provider.calls, 1);
    wait_active(address, &watched);

    // The window stops the turn itself, then goes. The stop is the fact; the
    // socket ending is what a crashed window leaves behind, and that alone
    // would have taken the whole grace.
    stop(address, &watched);
    window.close();

    // Bounded strictly below DEFAULT_CLOSE_GRACE, so idle here can only come
    // from the explicit stop and never from a grace that has not run out.
    let horizon = grapt_api::u3a::DEFAULT_CLOSE_GRACE
        .checked_sub(Duration::from_secs(5))
        .unwrap_or(Duration::from_secs(1));
    wait_idle_within(address, &watched, horizon);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "an explicit close let the turn issue another provider call"
    );

    release.release();
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "an answer released after an explicit close reached the provider again"
    );

    let history = json(
        address,
        "GET",
        &format!("/api/session/{watched}/message"),
        "",
    );
    assert!(
        history["data"].is_array(),
        "an explicitly closed session must still serve its history: {history}"
    );
    release.release();
    product.stop_after_witness(&root);
    Ok(())
}

fn stop(address: SocketAddr, session: &str) {
    let (status, response) = request_with_read_timeout(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        "{}",
        Duration::from_secs(15),
    );
    assert_eq!(
        status,
        204,
        "explicit close was refused: {}",
        String::from_utf8_lossy(&response)
    );
}

fn wait_idle_within(address: SocketAddr, session: &str, allowance: Duration) {
    let wait = PollDeadline::within(allowance);
    loop {
        let active = json(address, "GET", "/api/session/active", "");
        if active["data"]
            .as_object()
            .is_some_and(|rows| !rows.contains_key(session))
        {
            return;
        }
        assert!(
            wait.pending(),
            "an explicit close waited on the grace instead of stopping at once; last read {active}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// A close must not leave admitted input to run later with nobody watching.
/// The queued prompt stays durable and unpromoted, and no provider call
/// answers it, until a person explicitly resumes the Session.
#[test]
fn an_explicit_close_does_not_promote_queued_input() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("close-queued");
    prepare(&root, 120_000);
    let _root_cleanup = terminal_coordination::RootCleanup(root.clone());
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let disconnect = Arc::new(AtomicBool::new(true));
    let provider = ProviderFixture::start(vec![Script::ControlledTerminalOutput {
        bytes: 64,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        disconnect: Arc::clone(&disconnect),
    }]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);

    let watched = session(address);
    let window = Window::open(address, &watched);
    prompt(address, &watched, "msg_close_running");
    wait_calls(&provider.calls, 1);
    wait_active(address, &watched);
    // Admitted behind the running turn, so it is exactly what a close must not
    // promote once the turn stops.
    prompt(address, &watched, "msg_close_queued");

    stop(address, &watched);
    window.close();
    wait_idle(address, &watched);

    // Nothing may answer the queued input: no further provider call, and the
    // input is still waiting rather than promoted.
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "queued input ran after the window that admitted it closed"
    );
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{watched}/pending"),
        "",
    );
    assert!(
        pending["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == "msg_close_queued")),
        "a close discarded admitted input instead of leaving it queued: {pending}"
    );

    release.release();
    // The worker's phase wait observes the fixture's stop flag before its
    // release flag, so let it finish before teardown cancels it.
    thread::sleep(Duration::from_secs(2));
    product.stop_after_witness(&root);
    Ok(())
}

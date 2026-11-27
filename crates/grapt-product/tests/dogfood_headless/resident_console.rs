//! Real console events reach a resident after its stdout reader has gone away.
use super::*;
use std::os::windows::process::CommandExt;
use windows_spawn::{CreationFlags, SpawnOptions};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const ROLE: &str = "GRAPT_RESIDENT_CONSOLE_EVENT";
const INITIAL: &str = "initial console task";

#[test]
fn ctrl_c_interrupts_a_resident_after_stdout_reader_loss() -> TestResult {
    console_case(
        false,
        "ctrl_c_interrupts_a_resident_after_stdout_reader_loss",
    )
}

#[test]
fn ctrl_break_interrupts_a_resident_after_stdout_reader_loss() -> TestResult {
    console_case(
        true,
        "ctrl_break_interrupts_a_resident_after_stdout_reader_loss",
    )
}

fn console_case(break_event: bool, name: &str) -> TestResult {
    let role = if break_event { "break" } else { "interrupt" };
    in_private_console(role, &format!("resident_console::{name}"), || {
        resident_in_private_console(break_event)
    })
}

pub(super) fn in_private_console(
    role: &str,
    name: &str,
    run: impl FnOnce() -> TestResult,
) -> TestResult {
    let _journey = process_journey_custody();
    if let Some(selected) = std::env::var_os(ROLE) {
        assert_eq!(selected, role, "console fixture role changed");
        return run();
    }
    let root = root(role);
    fs::create_dir_all(&root)?;
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    let log = root.join("console-helper.log");
    let capture = fs::File::create(&log)?;
    let owned = processkit::ProcessGroup::new()?;
    let mut command = windows_spawn::Command::new(std::env::current_exe()?);
    command
        .args(["--exact", name, "--nocapture"])
        .env(ROLE, role)
        .stdin(windows_spawn::Stdio::null())
        .stdout(windows_spawn::Stdio::from_borrowed(&capture)?)
        .stderr(windows_spawn::Stdio::from_borrowed(&capture)?);
    // Assign before any fixture work can run. The census includes the helper,
    // CLI and separately owned service, even on an assertion failure.
    let suspended = command
        .spawn_suspended_with(SpawnOptions::new().creation_flags(CreationFlags::NEW_CONSOLE))?;
    owned.adopt_external(suspended.id())?;
    let mut helper = suspended.resume()?;
    let deadline = PollDeadline::within(Duration::from_secs(60));
    let status = loop {
        if let Some(status) = helper.try_wait()? {
            break status;
        }
        assert!(deadline.pending(), "owned console fixture did not finish");
        thread::sleep(Duration::from_millis(10));
    };
    let mut output = Vec::new();
    fs::File::open(log)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut output)?;
    assert!(
        output.len() <= 64 * 1024,
        "console fixture output exceeded bound"
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&output));
    let deadline = PollDeadline::within(Duration::from_secs(5));
    loop {
        let survivors = owned.members()?;
        if survivors.is_empty() {
            break;
        }
        assert!(deadline.pending(), "owned console survivors: {survivors:?}");
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

struct Resident(Child);
impl Drop for Resident {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _kill = self.0.kill();
            let _join = self.0.wait();
        }
    }
}

struct Release(Arc<terminal_coordination::Phase>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg_attr(windows, expect(clippy::too_many_lines, reason = "legacy"))]
fn resident_in_private_console(break_event: bool) -> TestResult {
    // Catch the broadcast in this helper too; custom handlers are not inherited
    // by the CLI. Neither process uses NEW_PROCESS_GROUP, which disables Ctrl+C.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _entered_runtime = runtime.enter();
    let _interrupt = tokio::signal::windows::ctrl_c()?;
    let _break_signal = tokio::signal::windows::ctrl_break()?;
    let root = root("resident-console-child");
    prepare(&root, 60_000);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("initial task completed"),
        Script::ControlledTerminalOutput {
            bytes: 32,
            entered: entered.clone(),
            release: release.clone(),
            disconnect: Arc::new(AtomicBool::new(true)),
        },
    ]);
    let _release = Release(release);
    let mut command = grapt_service::launcher_environment::command(shipped_cli::product_binary());
    command
        .args(["serve-terminal", "--service"])
        .arg(&root)
        .args(["--port", "0"])
        .stdin(Stdio::piped())
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        )
        .env("GRAPT_TEST_CATALOG_ROOT", &root)
        .env("GRAPT_TEST_HOME", &root)
        .env("GRAPT_CONFIG_DIR", root.join("global-config"));
    // A console-wide event must never reach the separately owned service.
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut product = process_fixture::spawn_captured(&root, &mut command);
    let address = await_ready(&root, &mut product);
    let session = create(address);
    let mut command = grapt_service::launcher_environment::command(shipped_cli::cli_binary());
    command
        .args(["run", "--service"])
        .arg(&root)
        .args(["--session", &session, "--stdin", "--jsonl", "--resident"])
        .env("GRAPT_TEST_HOME", &root)
        .env("GRAPT_CONFIG_DIR", root.join("global-config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(fs::File::create(
            root.join("resident-stderr.log"),
        )?));
    let mut resident = Resident(command.spawn()?);
    let mut input = resident.0.stdin.take().ok_or("resident stdin absent")?;
    input.write_all(INITIAL.as_bytes())?;
    drop(input); // Real initial stdin EOF.
    drop(resident.0.stdout.take().ok_or("resident stdout absent")?);
    wait_calls(&provider.calls, 1);
    permission_residuals::wait_session_idle(address, &session);
    assert!(
        resident.0.try_wait()?.is_none(),
        "stdout loss ended the resident at idle"
    );
    let path = format!("/api/session/{session}/prompt");
    let later = thread::spawn(move || {
        request(
            address,
            "POST",
            &path,
            r#"{"text":"later console task","delivery":"queue","resume":true}"#,
        )
    });
    entered.wait("later running task", &AtomicBool::new(false))?;
    let active = json(address, "GET", "/api/session/active", "");
    let execution = active["data"][&session]["executionID"]
        .as_str()
        .ok_or("later execution absent")?;
    let (events, connected) = event_stream(address);
    assert!(
        resident.0.try_wait()?.is_none(),
        "resident ended before console delivery"
    );
    // Zero means this freshly created console, never an ambient console or a
    // nonzero group for Ctrl+C (whose API success does not imply receipt).
    winconsole::console::generate_ctrl_event(break_event, None)?;
    let observed = collect(events, connected, "session.execution.interrupted");
    let events = observed
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| serde_json::from_str::<serde_json::Value>(line.trim()))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "session.execution.interrupted"
                && event["data"]["sessionID"] == session
                && event["data"]["executionID"] == execution),
        "authenticated readback did not interrupt the selected execution"
    );
    let deadline = PollDeadline::within(Duration::from_secs(5));
    let status = loop {
        if let Some(status) = resident.0.try_wait()? {
            break status;
        }
        assert!(
            deadline.pending(),
            "console event did not join the resident"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        status.code(),
        Some(130),
        "resident did not return cancellation"
    );
    let (status, body) = later.join().map_err(|_| "later prompt worker panicked")?;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(
        product.try_wait()?.is_none(),
        "console event killed the service"
    );
    assert_eq!(request(address, "GET", "/api/health", "").0, 200);
    assert!(json(address, "GET", "/api/session/active", "")["data"][&session].is_null());
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    assert_eq!(
        messages["data"]
            .as_array()
            .ok_or("message page absent")?
            .iter()
            .filter(|message| message["type"] == "user" && message["text"] == INITIAL)
            .count(),
        1,
        "console cancellation readmitted initial work"
    );
    drop(_release);
    provider.settle_terminal_workers()?;
    drop(product.stdin.take());
    let deadline = PollDeadline::within(Duration::from_secs(5));
    loop {
        if let Some(status) = product.try_wait()? {
            assert!(status.success(), "service did not stop gracefully");
            break;
        }
        assert!(
            deadline.pending(),
            "owned service did not join after control EOF"
        );
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

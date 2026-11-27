//! Real signals run in private helper processes, never in the test runner.
use super::*;
use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::sync_channel,
    time::Duration,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CASE: &str = "GRAPT_TEST_UNIX_SIGNAL_CASE";
const CHILD: &str = "listener::unix::signal_tests::signal_child";
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn signal(name: &str, pid: u32) -> TestResult {
    let status = Command::new("kill")
        .args(["-s", name, &pid.to_string()])
        .status()?;
    assert!(status.success(), "signal sender failed: {status}");
    Ok(())
}

#[test]
fn signal_child() -> TestResult {
    let Ok(case) = std::env::var(CASE) else {
        return Ok(());
    };
    let (wake, notice) = sync_channel(1);
    if case == "unavailable" {
        let status = Command::new("prlimit")
            .args(["--pid", &std::process::id().to_string(), "--nofile=3"])
            .status()?;
        assert!(status.success());
    }
    let mut listener = Listener::new(wake)?;
    if case == "unavailable" {
        std::thread::park_timeout(Duration::from_secs(10));
        return Err("default hangup failed to terminate the helper".into());
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let status = fs::read_to_string("/proc/self/status")?;
        let caught = status
            .lines()
            .find_map(|line| line.strip_prefix("SigCgt:"))
            .ok_or("signal mask absent")?;
        let caught = u64::from_str_radix(caught.trim(), 16)?;
        if caught & 0x4003 == 0x4003 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "signal registration did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    if case.starts_with("interrupt-") {
        signal("INT", std::process::id())?;
        notice.recv_timeout(Duration::from_secs(5))?;
    }
    signal(
        if case.ends_with("hangup") {
            "HUP"
        } else {
            "TERM"
        },
        std::process::id(),
    )?;
    notice.recv_timeout(Duration::from_secs(5))?;
    assert!(
        (0..2)
            .filter_map(|_| listener.poll())
            .any(|event| matches!(event, Event::Hangup | Event::Terminate)),
        "an unavoidable close was lost behind the unread interrupt"
    );
    Ok(())
}

fn run(case: &str) -> TestResult {
    let mut child = Owned(
        Command::new(std::env::current_exe()?)
            .args(["--exact", CHILD, "--nocapture"])
            .env(CASE, case)
            .env("RUST_BACKTRACE", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.0.try_wait()? {
            let mut stderr = String::new();
            child
                .0
                .stderr
                .take()
                .ok_or("stderr absent")?
                .take(8193)
                .read_to_string(&mut stderr)?;
            assert!(stderr.len() <= 8192);
            if !status.success() {
                return Err(format!("{case}: {status}: {stderr}").into());
            }
            return Ok(());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "signal child did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn hangup_and_termination_reach_the_synchronous_listener() -> TestResult {
    run("hangup")?;
    run("terminate")
}

#[test]
fn an_unread_interrupt_cannot_discard_a_close() -> TestResult {
    let hangup = run("interrupt-hangup");
    let terminate = run("interrupt-terminate");
    assert!(
        hangup.is_ok() && terminate.is_ok(),
        "hangup={hangup:?}; terminate={terminate:?}"
    );
    Ok(())
}

#[test]
fn an_unavailable_signal_owner_keeps_the_default_hangup_action() -> TestResult {
    use std::os::unix::process::ExitStatusExt;
    let path: PathBuf =
        std::env::temp_dir().join(format!("grapt-signal-failure-{}.log", std::process::id()));
    let capture = fs::File::create(&path)?;
    let mut child = Owned(
        Command::new(std::env::current_exe()?)
            .args(["--exact", CHILD, "--nocapture"])
            .env(CASE, "unavailable")
            .stdin(Stdio::null())
            .stdout(capture.try_clone()?)
            .stderr(capture)
            .spawn()?,
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !fs::read_to_string(&path)?.contains("console control unavailable:") {
        assert!(
            child.0.try_wait()?.is_none(),
            "signal failure helper exited early"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "signal owner did not report failure"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    signal("HUP", child.0.id())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait()? {
            fs::remove_file(path)?;
            assert_eq!(status.signal(), Some(1));
            return Ok(());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "default hangup was swallowed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

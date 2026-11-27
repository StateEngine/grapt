//! A close deadline must not depend on a diagnostic stream becoming writable.
use super::*;
use std::os::unix::fs::OpenOptionsExt;

#[test]
fn a_close_timeout_exits_with_backpressured_stderr() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("terminal-close-blocked-output");
    fs::create_dir_all(&root)?;
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    fs::write(
        root.join("serve-ready.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema":"secode.product.ready.v2", "operation":"serve", "pid":12345,
            "build":"r-restart", "environment":"fixture", "service_instance":"fixture-secret",
            "observed_endpoint":format!("http://{}", listener.local_addr()?),
        }))?,
    )?;
    fs::write(root.join("basic.credential"), "fixture:fixture-secret\n")?;
    let errors = root.join("window.stderr");
    nix::unistd::mkfifo(
        &errors,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )?;
    let flags = nix::fcntl::OFlag::O_NONBLOCK.bits();
    let _reader = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(&errors)?;
    let mut writer = fs::OpenOptions::new()
        .write(true)
        .custom_flags(flags)
        .open(&errors)?;
    let mut full = false;
    for _ in 0..256 {
        match writer.write(&[b'x'; 4096]) {
            Ok(0) => return Err("diagnostic pipe accepted no bytes".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                full = true;
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    assert!(full, "diagnostic pipe did not reach bounded backpressure");
    drop(writer);
    let (mut window, pid_file) = Window::open(&root, "ses_000000000001")?;
    // Keep the health response pending: close must spend its acknowledgement
    // allowance and then exit, regardless of the unavailable diagnostic sink.
    let _pending_health = PollDeadline::within(Duration::from_secs(10))
        .poll(Duration::from_millis(20), || listener.accept().ok())
        .ok_or("terminal did not reach the health request")?;
    let pid = fs::read_to_string(pid_file)?.parse::<u32>()?;
    let process = rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(i32::try_from(pid)?).ok_or("invalid terminal pid")?,
        rustix::process::PidfdFlags::empty(),
    )?;
    let command = fs::read(format!("/proc/{pid}/cmdline"))?;
    let binary = crate::tui_busy_seat::tui_binary();
    assert!(command
        .split(|byte| *byte == 0)
        .any(|argument| argument == binary.as_os_str().as_encoded_bytes()));
    window.1 = Some(process);
    rustix::process::pidfd_send_signal(
        window.1.as_ref().ok_or("terminal process absent")?,
        rustix::process::Signal::HUP,
    )?;
    assert!(
        PollDeadline::within(Duration::from_secs(8))
            .poll(Duration::from_millis(20), || window_exited(pid)
                .then_some(()))
            .is_some(),
        "the terminal remained alive beyond its close acknowledgement allowance"
    );
    Ok(())
}

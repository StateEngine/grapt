//! Kernel closure of the parent's private pipe ends the owned product.
use super::*;

#[cfg(target_os = "linux")]
fn process_exited(pid: u32) -> bool {
    let path = format!("/proc/{pid}/stat");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(error) => panic!("{path} is unreadable: {error}"),
    };
    process_stat_exited(&path, file)
}

#[cfg(target_os = "linux")]
fn process_stat_exited(path: &str, file: impl Read) -> bool {
    let mut stat = String::new();
    match file.take(4097).read_to_string(&mut stat) {
        Ok(_) => {}
        // Linux returns ESRCH if the process exits after stat was opened.
        Err(error) if error.raw_os_error() == Some(3) => return true,
        Err(error) => panic!("{path} read failed: {error}"),
    }
    assert!(stat.len() <= 4096, "process stat exceeded its byte bound");
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, suffix)| suffix.split_whitespace().next())
        .unwrap_or_else(|| panic!("{path} omitted the process state"));
    // A zombie has exited; its external reaper owns removal of the entry.
    matches!(state, "Z" | "X")
}

#[cfg(target_os = "linux")]
struct FailedStatRead(i32);

#[cfg(target_os = "linux")]
impl Read for FailedStatRead {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from_raw_os_error(self.0))
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_process_that_disappears_after_stat_is_opened_has_exited() {
    assert!(process_stat_exited("fixture stat", FailedStatRead(3)));
}

#[cfg(target_os = "linux")]
#[test]
#[should_panic(expected = "fixture stat read failed:")]
fn an_unreadable_stat_is_never_reported_as_an_exited_process() {
    process_stat_exited("fixture stat", FailedStatRead(13));
}

#[cfg(target_os = "linux")]
#[test]
fn protocol_eof_reaps_the_embedded_product_and_releases_the_service() {
    parent_loss_ends_product("acp-protocol-eof", |acp| acp.close_input_and_wait());
}

#[cfg(target_os = "linux")]
#[test]
fn sigkill_of_the_acp_parent_ends_its_embedded_product() {
    parent_loss_ends_product("acp-parent-sigkill", |acp| acp.kill_parent());
}

#[cfg(target_os = "linux")]
#[test]
fn sigterm_of_the_acp_parent_releases_the_service_for_restart() {
    parent_loss_ends_product("acp-parent-sigterm", |acp| acp.terminate_parent());
}

#[cfg(target_os = "linux")]
fn parent_loss_ends_product(case: &str, stop: impl FnOnce(&mut super::acp_process::AcpProcess)) {
    use super::acp_process::{AcpProcess, ACP_FRAMES};
    let _journey = process_journey_custody();
    let root = root(case);
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut acp = AcpProcess::start(&root, provider.address, ACP_FRAMES);
    let ready = root.join("serve-ready.json");
    let wait = PollDeadline::new();
    let pid = wait
        .poll(Duration::from_millis(10), || match fs::File::open(&ready) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(8193)
                    .read_to_end(&mut bytes)
                    .unwrap_or_else(|error| panic!("readiness read failed: {error}"));
                assert!(bytes.len() <= 8192, "readiness exceeded its byte bound");
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|row| row["pid"].as_u64())
                    .and_then(|pid| u32::try_from(pid).ok())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("readiness is unreadable: {error}"),
        })
        .unwrap_or_else(|| panic!("embedded product never published readiness"));
    assert!(
        !process_exited(pid),
        "the product exited before parent loss"
    );
    stop(&mut acp);
    let exited = permission_residuals::wait_until(
        Duration::from_secs(10),
        Duration::from_millis(20),
        || process_exited(pid),
    )
    .is_some();
    // Record the witness before the fixture's group cleanup can kill it.
    acp.cleanup();
    drop(acp);
    assert!(exited, "product {pid} survived {case} for 10 seconds");
    // Readiness belongs to the previous process. Leave all registrations and
    // database bytes intact so restart proves the service is available again.
    match fs::remove_file(&ready) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("old readiness removal failed: {error}"),
    }
    let mut restarted = process_fixture::spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    assert!(
        restarted.try_wait().is_ok_and(|status| status.is_none()),
        "restarted product exited after readiness"
    );
    assert_eq!(
        json(address, "GET", "/api/session/active", "")["data"],
        serde_json::json!({})
    );
    drop(restarted);
    process_fixture::cleanup(&root);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn native_control_pipe_eof_ends_the_product() {
    let _journey = process_journey_custody();
    let root = root("acp-native-control-eof");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    for name in ["home", "global-config"] {
        fs::create_dir_all(root.join(name))
            .unwrap_or_else(|error| panic!("fixture catalog root failed: {error}"));
    }
    let mut command =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
    command
        .args(["serve-terminal", "--service"])
        .arg(&root)
        .args(["--port", "0"])
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        )
        .env("GRAPT_TEST_HOME", root.join("home"))
        .env("GRAPT_CONFIG_DIR", root.join("global-config"))
        .stdin(Stdio::piped());
    let mut child = process_fixture::spawn_captured(&root, &mut command);
    let _address = await_ready(&root, &mut child);
    assert!(child.try_wait().is_ok_and(|status| status.is_none()));
    drop(
        child
            .stdin
            .take()
            .unwrap_or_else(|| panic!("private control pipe absent")),
    );
    let wait = PollDeadline::new();
    let status = wait.poll(Duration::from_millis(20), || {
        child
            .try_wait()
            .unwrap_or_else(|error| panic!("child wait failed: {error}"))
    });
    assert!(
        status.is_some_and(|status| status.success()),
        "control EOF did not end the product successfully: {status:?}"
    );
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

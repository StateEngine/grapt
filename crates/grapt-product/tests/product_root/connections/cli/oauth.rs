//! Installed device-flow JSON progress and cancellation use the shared service owner.
use super::*;
use grapt_types::ClockAuthority;
use std::io::BufRead;
use std::sync::mpsc::{sync_channel, Receiver};

fn installed(root: &Path) -> TestResult<(PathBuf, PathBuf)> {
    let bin = root.join("installed");
    fs::create_dir(&bin)?;
    let binary = bin.join(if cfg!(windows) { "grapt.exe" } else { "grapt" });
    let product = bin.join(if cfg!(windows) {
        "grapt-server.exe"
    } else {
        "grapt-server"
    });
    fs::copy(shipped_cli::cli_binary(), &binary)?;
    fs::copy(shipped_cli::product_binary(), &product)?;
    Ok((binary, product))
}
struct Progress {
    process: RestartProcess,
    output: Receiver<Result<Value, String>>,
    reader: thread::JoinHandle<()>,
}
fn progress(binary: &Path, profile: &Path, endpoint: &str) -> TestResult<Progress> {
    let process = RestartProcess(
        grapt_service::launcher_environment::command(binary)
            .env(
                grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
                grapt_login::profile_security::isolate_test_keystore()?,
            )
            .args([
                "login",
                "chatgpt",
                "--no-browser",
                "--json",
                "--id",
                "work-chatgpt",
                "--profile",
            ])
            .arg(profile)
            .env("GRAPT_CHATGPT_SIGN_IN_LOOPBACK", endpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    read_progress(process)
}
fn read_progress(mut process: RestartProcess) -> TestResult<Progress> {
    let stdout = process.0.stdout.take().ok_or("CLI stdout absent")?;
    let (sender, receiver) = sync_channel(2);
    let reader = thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout.take(65537));
        for _ in 0..2 {
            let mut line = String::new();
            let result = reader
                .read_line(&mut line)
                .map_err(|_| "CLI progress is unreadable".to_owned())
                .and_then(|count| {
                    if count == 0 || line.len() > 32768 {
                        return Err("CLI progress is absent or oversized".into());
                    }
                    if line.contains("private-cli-oauth") {
                        return Err("CLI progress exposed credential material".into());
                    }
                    serde_json::from_str(&line).map_err(|_| "CLI progress is not JSON".into())
                });
            if sender.send(result).is_err() {
                break;
            }
        }
    });
    Ok(Progress {
        process,
        output: receiver,
        reader,
    })
}
fn finished(process: &mut RestartProcess) -> TestResult<std::process::ExitStatus> {
    for _ in 0..18000 {
        if let Some(status) = process.0.try_wait()? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err("CLI OAuth did not exit within its fixture bound".into())
}
/// The first line of a fixture journey must be the pending challenge.
/// One stale-build failure is named instead of left to the status assert:
/// a sibling CLI built without loopback-testkit refuses the fixture URL.
fn pending_line(receiver: &Receiver<Result<Value, String>>) -> TestResult<Value> {
    let pending = receiver.recv_timeout(Duration::from_secs(120))??;
    if pending["status"] == "failed" && pending["reason"].as_str() == Some("sign-in URL is invalid")
    {
        return Err(format!(
            "sibling CLI {} rejected the fixture sign-in URL: it was built \
             without loopback-testkit; run `cargo build -p grapt-cli \
             --features loopback-testkit` with the same target directory \
             and profile",
            shipped_cli::cli_binary().display()
        )
        .into());
    }
    Ok(pending)
}
fn challenge(receiver: &Receiver<Result<Value, String>>, endpoint: &str) -> TestResult {
    let pending = pending_line(receiver)?;
    assert_eq!(pending["status"], "pending");
    assert!(pending["attemptID"]
        .as_str()
        .is_some_and(|id| id.starts_with("oauth_")));
    assert!(pending["url"]
        .as_str()
        .is_some_and(|url| url.starts_with(endpoint)));
    assert_eq!(pending["userCode"], "CLI-DEVICE");
    assert!(pending["expiresAt"]
        .as_f64()
        .is_some_and(|value| value.is_finite() && value > 0.0));
    Ok(())
}
fn next_request(listener: &TcpListener, stage: &str) -> TestResult<TcpStream> {
    // A CLI-owned new profile must initialize before its first issuer request.
    for _ in 0..12000 {
        match listener.accept() {
            Ok((socket, _)) => return Ok(socket),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(format!("CLI issuer did not receive {stage} within its fixture bound").into())
}
fn issuer(
    listener: TcpListener,
    cancel: bool,
    polling: std::sync::mpsc::SyncSender<()>,
    released: Receiver<()>,
) -> Result<(), String> {
    (|| -> TestResult {
        let mut socket = next_request(&listener, "device authorization")?;
        let request = super::super::oauth::request(&mut socket)?;
        assert!(request.starts_with("POST /api/accounts/deviceauth/usercode "), "CLI did not select device flow");
        super::super::oauth::respond(&mut socket, json!({"device_auth_id":"fixture-cli-device","user_code":"CLI-DEVICE","interval":"1"}))?;
        let mut socket = next_request(&listener, "device polling")?;
        let request = super::super::oauth::request(&mut socket)?;
        assert!(request.starts_with("POST /api/accounts/deviceauth/token "));
        polling.send(())?;
        if cancel {
            released.recv_timeout(Duration::from_secs(120))?;
            return Ok(());
        }
        super::super::oauth::respond(&mut socket, json!({"authorization_code":"private-cli-oauth-code","code_verifier":"private-cli-oauth-verifier"}))?;
        let mut socket = next_request(&listener, "token exchange")?;
        let request = super::super::oauth::request(&mut socket)?;
        assert!(request.starts_with("POST /oauth/token "));
        assert!(request.contains("private-cli-oauth-code") && request.contains("private-cli-oauth-verifier"));
        let expires = grapt_store::SystemClock.now().as_millis() / 1000 + 3 * 24 * 3600;
        let token = format!("e30.{}.signature", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({"exp":expires,"https://api.openai.com/auth":{"chatgpt_account_id":"fixture-cli-account"}}).to_string()));
        super::super::oauth::respond(&mut socket, json!({"access_token":token,"refresh_token":"private-cli-oauth-refresh","expires_in":3*24*3600}))?;
        let mut socket = next_request(&listener, "model discovery")?;
        let request = super::super::oauth::request(&mut socket)?;
        assert!(request.starts_with("GET /models?client_version="));
        assert!(request.to_ascii_lowercase().contains("authorization: bearer e30."));
        super::super::oauth::respond(&mut socket, json!({"models":[{"slug":"gpt-5.6","context_window":272000,"supported_reasoning_levels":[]}]}))?;
        Ok(())
    })().map_err(|error| error.to_string())
}
#[test]
fn copied_cli_device_sign_in_reports_json_challenge_and_persists_the_named_connection() -> TestResult
{
    let root = root("cli-oauth-progress");
    reset(&root);
    let (binary, _) = installed(&root)?;
    let profile = root.join("new-profile");
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let (polling, polled) = sync_channel(1);
    let (_release, released) = sync_channel(1);
    let fixture = thread::spawn(move || issuer(listener, false, polling, released));
    let Progress {
        mut process,
        output,
        reader,
    } = progress(&binary, &profile, &endpoint)?;
    challenge(&output, &endpoint)?;
    polled.recv_timeout(Duration::from_secs(120))?;
    let complete = output.recv_timeout(Duration::from_secs(120))??;
    assert_eq!(complete["status"], "signed_in");
    assert!(finished(&mut process)?.success());
    reader.join().map_err(|_| "CLI progress reader stopped")?;
    fixture.join().map_err(|_| "CLI issuer stopped")??;
    let records = Store::read_snapshot(&profile)?.ok_or("connection metadata absent")?;
    assert_eq!(records.connections.len(), 1);
    assert_eq!(records.connections[0].id.as_str(), "work-chatgpt");
    assert_eq!(records.connections[0].catalog_provider.as_str(), "openai");
    let status = cli(
        &binary,
        &profile,
        &endpoint,
        &["auth", "status", "--json"],
        None,
    )?;
    assert!(status.0);
    assert_eq!(status.1["connections"][0]["status"], "ok");
    assert!(status.1["connections"][0]["last4"].is_null());
    fs::remove_dir_all(root)?;
    Ok(())
}
#[cfg(unix)]
#[test]
fn copied_cli_interrupt_cancels_pending_device_sign_in_without_publishing() -> TestResult {
    let root = root("cli-oauth-cancel");
    reset(&root);
    let (binary, _) = installed(&root)?;
    let profile = root.join("new-profile");
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let (polling, polled) = sync_channel(1);
    let (release, released) = sync_channel(1);
    let fixture = thread::spawn(move || issuer(listener, true, polling, released));
    let Progress {
        mut process,
        output,
        reader,
    } = progress(&binary, &profile, &endpoint)?;
    challenge(&output, &endpoint)?;
    polled.recv_timeout(Duration::from_secs(120))?;
    assert!(Command::new("kill")
        .args(["-INT", &process.0.id().to_string()])
        .status()?
        .success());
    let cancelled = output.recv_timeout(Duration::from_secs(120))??;
    assert_eq!(cancelled["status"], "failed");
    assert!(cancelled["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("cancelled")));
    assert_eq!(finished(&mut process)?.code(), Some(130));
    release.send(())?;
    reader.join().map_err(|_| "CLI progress reader stopped")?;
    fixture.join().map_err(|_| "CLI issuer stopped")??;
    assert!(Store::read_snapshot(&profile)?
        .ok_or("connection metadata absent")?
        .connections
        .is_empty());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
#[path = "oauth/browser.rs"]
mod browser;

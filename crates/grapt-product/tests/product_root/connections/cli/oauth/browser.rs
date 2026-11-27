//! Interrupt while the borrowed service owns a pending browser attempt.
use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn copied_cli_interrupt_during_browser_launch_cancels_the_borrowed_services_attempt() -> TestResult
{
    let root = root("cli-browser-cancel");
    reset(&root);
    let (binary, product) = installed(&root)?;
    let profile = root.join("profile");
    assert!(grapt_service::launcher_environment::command(&product)
        .args(["init-profile", "--service"])
        .arg(&profile)
        .output()?
        .status
        .success());
    let issuer = TcpListener::bind("127.0.0.1:0")?;
    issuer.set_nonblocking(true)?;
    let endpoint = format!("http://{}", issuer.local_addr()?);
    let mut service = RestartProcess(
        grapt_service::launcher_environment::command(&product)
            .args(["serve-terminal", "--service"])
            .arg(&profile)
            .args(["--port", "0"])
            .env("GRAPT_CHATGPT_SIGN_IN_LOOPBACK", &endpoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let ready = service.ready(&profile);
    let client = Client {
        address: ready["observed_endpoint"]
            .as_str()
            .and_then(|value| value.strip_prefix("http://"))
            .ok_or("borrowed service endpoint absent")?
            .parse()?,
        authentication: base64::engine::general_purpose::STANDARD
            .encode(fs::read(profile.join("basic.credential"))?),
    };
    let launchers = root.join("launchers");
    fs::create_dir(&launchers)?;
    let entered = root.join("browser-entered");
    let launcher = launchers.join("xdg-open");
    // The controlled child interrupts its parent from inside browser launch,
    // before that launch finishes. No scheduling delay selects the window.
    fs::write(
        &launcher,
        br##"#!/usr/bin/python3
import os, select, signal
with open(os.environ["GRAPT_TEST_BROWSER_ENTERED"], "x") as entered:
    entered.write("browser launch entered")
os.kill(os.getppid(), signal.SIGINT)
select.select([], [], [], 30)
"##,
    )?;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700))?;
    let path = std::env::join_paths(std::iter::once(launchers).chain(std::env::split_paths(
        &std::env::var_os("PATH").ok_or("PATH absent")?,
    )))?;
    let Progress {
        mut process,
        output,
        reader,
    } = read_progress(RestartProcess(
        grapt_service::launcher_environment::command(&binary)
            .args([
                "login",
                "chatgpt",
                "--json",
                "--id",
                "work-browser",
                "--profile",
            ])
            .arg(&profile)
            .env("PATH", path)
            .env("GRAPT_TEST_BROWSER_ENTERED", &entered)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    ))?;
    let pending = pending_line(&output)?;
    assert_eq!(pending["status"], "pending");
    let attempt = pending["attemptID"]
        .as_str()
        .ok_or("browser attempt absent")?;
    let url = grapt_login::chatgpt::loopback_client(&endpoint)?
        .get(pending["url"].as_str().ok_or("browser URL absent")?)
        .build()?
        .url()
        .clone();
    let parameter = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .ok_or("browser parameter absent")
    };
    let callback: SocketAddr = parameter("redirect_uri")?
        .strip_prefix("http://")
        .and_then(|value| value.strip_suffix("/auth/callback"))
        .ok_or("browser callback absent")?
        .parse()?;
    let cancelled = output.recv_timeout(Duration::from_secs(120))??;
    assert_eq!(cancelled["status"], "failed");
    assert!(cancelled["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("cancelled")));
    assert_eq!(finished(&mut process)?.code(), Some(130));
    assert_eq!(fs::read_to_string(entered)?, "browser launch entered");
    reader
        .join()
        .map_err(|_| "browser progress reader stopped")?;
    assert!(
        service.0.try_wait()?.is_none(),
        "CLI stopped the borrowed service"
    );
    let route = format!("/api/integration/openai/connect/oauth/{attempt}");
    assert_eq!(client.response("GET", &route, "", true)?.0, 400);
    assert_eq!(
        client
            .response(
                "POST",
                &format!("{route}/complete"),
                r#"{"code":"private-cli-oauth-late"}"#,
                true
            )?
            .0,
        400
    );
    let mut closed = false;
    for _ in 0..100 {
        if TcpListener::bind(callback).is_ok() {
            closed = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        closed,
        "cancelled CLI attempt retained its callback listener"
    );
    assert!(
        TcpStream::connect_timeout(&callback, Duration::from_secs(1)).is_err(),
        "late callback was admitted"
    );
    assert!(
        matches!(issuer.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "cancelled attempt contacted its issuer"
    );
    assert!(Store::read_snapshot(&profile)?
        .ok_or("connection metadata absent")?
        .connections
        .is_empty());
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"],
        Value::Null
    );
    service.kill_and_reap();
    fs::remove_dir_all(root)?;
    Ok(())
}

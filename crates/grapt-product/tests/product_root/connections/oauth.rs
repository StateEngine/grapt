//! Cancellation and fresh-profile activation through the installed OAuth routes.
use super::*;

#[path = "oauth/browser.rs"]
mod browser;
use grapt_types::ClockAuthority;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

pub(super) fn respond(socket: &mut TcpStream, body: Value) -> TestResult {
    let body = body.to_string();
    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
    Ok(())
}
pub(super) fn request(socket: &mut TcpStream) -> TestResult<String> {
    // Accepted sockets can inherit the listener's nonblocking mode on Windows.
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::new();
    loop {
        let mut block = [0; 1024];
        let count = socket.read(&mut block)?;
        if count == 0 || bytes.len() + count > 64 * 1024 {
            return Err("fixture request exceeded its bound".into());
        }
        bytes.extend_from_slice(&block[..count]);
        let raw = String::from_utf8_lossy(&bytes);
        if let Some((head, body)) = raw.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|length| length.parse::<usize>().ok())
                })
                .unwrap_or(0);
            if body.len() >= length {
                return Ok(raw.into_owned());
            }
        }
    }
}
pub(super) fn accept(listener: &TcpListener) -> TestResult<TcpStream> {
    // Fixture admission includes profile authorization I/O on native Windows.
    // It is separate from the product request and callback deadlines.
    for _ in 0..3000 {
        match listener.accept() {
            Ok((socket, _)) => return Ok(socket),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err("issuer fixture did not receive its next request".into())
}

fn issuer(
    listener: TcpListener,
    polling: SyncSender<()>,
    cancelled: Receiver<()>,
    calls: Arc<AtomicUsize>,
) -> Result<(), String> {
    (|| -> TestResult {
        let jwt = |claims: Value| format!("e30.{}.signature", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string()));
        let expires = grapt_store::SystemClock.now().as_millis() / 1000 + 3 * 24 * 3600;
        for step in 0..7 {
            let mut socket = accept(&listener)?;
            let raw = request(&mut socket)?;
            calls.fetch_add(1, Ordering::SeqCst);
            match step {
                0 => {
                    assert!(raw.starts_with("POST /api/accounts/deviceauth/usercode "));
                    write!(socket, "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}")?;
                }
                1 | 3 => {
                    assert!(raw.starts_with("POST /api/accounts/deviceauth/usercode "));
                    respond(&mut socket, json!({"device_auth_id":"fixture-device","user_code":"ABCD-EFGH","interval":"1"}))?;
                }
                2 | 4 => {
                    assert!(raw.starts_with("POST /api/accounts/deviceauth/token "));
                    if step == 2 { polling.send(())?; cancelled.recv_timeout(Duration::from_secs(10))?; }
                    let _ = respond(&mut socket, json!({"authorization_code":"fixture-code","code_verifier":"fixture-verifier"}));
                }
                5 => {
                    assert!(raw.starts_with("POST /oauth/token "));
                    respond(&mut socket, json!({
                        "access_token":jwt(json!({"exp":expires,"https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account"}})),
                        "refresh_token":"private-fixture-refresh", "expires_in":3*24*3600,
                    }))?;
                }
                6 => {
                    assert!(raw.starts_with("GET /models?client_version="));
                    respond(&mut socket, json!({"models":[{"slug":"gpt-5.6","context_window":272000,"supported_reasoning_levels":[]}]}))?;
                }
                _ => return Err("unexpected fixture step".into()),
            }
        }
        Ok(())
    })().map_err(|error| error.to_string())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn fresh_profile_chatgpt_oauth_cancels_without_installing_then_activates_in_place() -> TestResult {
    let root = root("connection-oauth");
    reset(&root);
    let profile = root.join("profile");
    let initialized = Command::new(shipped_cli::product_binary())
        .args(["init-profile", "--service"])
        .arg(&profile)
        .output()?;
    assert!(
        initialized.status.success(),
        "profile initialization refused"
    );
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    listener.set_nonblocking(true)?;
    let (polling, poll_started) = sync_channel(1);
    let (cancelled, cancellation) = sync_channel(1);

    let approval = grapt_platform::profile_configuration::ProfileConfiguration::capture(&profile)?
        .approval()
        .encoded()?;
    let mut process = RestartProcess(
        grapt_service::launcher_environment::command(shipped_cli::product_binary())
            .args([
                "serve-terminal",
                "--profile-approval",
                &approval,
                "--service",
            ])
            .arg(&profile)
            .args(["--port", "0"])
            .env("GRAPT_CHATGPT_SIGN_IN_LOOPBACK", endpoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let ready = process.ready(&profile);
    let issuer_calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&issuer_calls);
    let provider = thread::spawn(move || issuer(listener, polling, cancellation, observed));
    let client = Client {
        address: ready["observed_endpoint"]
            .as_str()
            .and_then(|endpoint| endpoint.strip_prefix("http://"))
            .ok_or("endpoint absent")?
            .parse()?,
        authentication: base64::engine::general_purpose::STANDARD
            .encode(fs::read(profile.join("basic.credential"))?),
    };
    let route = "/api/integration/openai/connect/oauth";
    let body = r#"{"methodID":"chatgpt-device","inputs":{}}"#;
    assert_eq!(client.response("POST", route, body, false)?.0, 401);
    assert_eq!(
        client
            .response(
                "POST",
                "/api/integration/anthropic/connect/oauth",
                body,
                true
            )?
            .0,
        400
    );
    let (status, refusal) = client.response("POST", route, body, true)?;
    assert_eq!(status, 400);
    assert_eq!(
        refusal,
        json!({"_tag":"InvalidRequestError","kind":"integration_authorization","message":"Authentication failed"})
    );
    assert_eq!(
        issuer_calls.load(Ordering::SeqCst),
        1,
        "authorization refusal did not reach the issuer fixture"
    );
    let start = client.request("POST", route, body)?;
    let typed: grapt_api::u3a::IntegrationOAuthConnectResponseDto =
        serde_json::from_value(start.clone())?;
    assert!(!format!("{typed:?}").contains("private-fixture"));
    let id = start["data"]["attemptID"]
        .as_str()
        .ok_or("attempt absent")?;
    let status = format!("{route}/{id}");
    poll_started.recv_timeout(Duration::from_secs(10))?;
    assert_eq!(
        client.request("GET", &status, "")?["data"]["status"],
        "pending"
    );
    assert_eq!(client.response("DELETE", &status, "", true)?.0, 204);
    cancelled.send(())?;
    assert_eq!(client.response("GET", &status, "", true)?.0, 400);
    assert_eq!(client.response("DELETE", &status, "", true)?.0, 204);
    assert_eq!(
        client
            .response("DELETE", &format!("{route}/missing-attempt"), "", true)?
            .0,
        204
    );
    assert!(grapt_connections::store::Store::read_snapshot(&profile)?
        .ok_or("connections absent")?
        .connections
        .is_empty());
    let start = client.request("POST", route, body)?;
    let id = start["data"]["attemptID"]
        .as_str()
        .ok_or("attempt absent")?;
    let status = format!("{route}/{id}");
    let mut completed = false;
    // Completion includes real private-file publication, independently of
    // the issuer's request allowance. Avoid flooding native profile I/O.
    for _ in 0..600 {
        let response = client.request("GET", &status, "")?;
        assert!(!response.to_string().contains("private-fixture"));
        let current = response["data"]["status"].as_str().ok_or("status absent")?;
        assert!(
            matches!(current, "pending" | "complete"),
            "sign-in refused: {response}"
        );
        if current == "complete" {
            completed = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        completed,
        "sign-in did not complete; observed issuer requests: {}",
        issuer_calls.load(Ordering::SeqCst)
    );
    assert_eq!(client.response("DELETE", &status, "", true)?.0, 204);
    assert_eq!(
        client.request("GET", &status, "")?["data"]["status"],
        "complete"
    );
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"]["providerID"],
        "chatgpt"
    );
    let records =
        grapt_connections::store::Store::read_snapshot(&profile)?.ok_or("connections absent")?;
    assert_eq!(records.connections.len(), 1);
    assert!(matches!(
        records.connections[0].credential_format,
        grapt_connections::store::CredentialFormat::VaultedOAuth { .. }
    ));
    assert!(!profile
        .join(&records.connections[0].credential_file)
        .exists());
    let session = client.request("POST", "/api/session", &json!({"id":null,"location":{"directory":root.to_string_lossy().replace('\\', "/")},"model":{"providerID":"chatgpt","id":"gpt-5.6"}}).to_string())?;
    assert!(session["data"]["id"].is_string());
    assert!(process.0.try_wait()?.is_none());
    provider.join().map_err(|_| "issuer fixture panicked")??;
    process.0.kill()?;
    process.0.wait()?;
    reset(&root);
    Ok(())
}

//! Browser callbacks cannot activate a profile without the initiating state.
use super::*;

fn callback(address: SocketAddr, target: &str, host: &str) -> TestResult<String> {
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    write!(
        socket,
        "GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    socket.take(4097).read_to_string(&mut response)?;
    assert!(response.len() <= 4096);
    Ok(response)
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn browser_callback_checks_state_and_host_before_exchanging_and_activating() -> TestResult {
    let root = root("connection-browser");
    reset(&root);
    let profile = root.join("profile");
    assert!(Command::new(shipped_cli::product_binary())
        .args(["init-profile", "--service"])
        .arg(&profile)
        .output()?
        .status
        .success());
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&requests);
    let issuer = thread::spawn(move || -> Result<(), String> {
        (|| -> TestResult {
            let mut token = accept(&listener)?;
            let raw = request(&mut token)?;
            assert!(raw.starts_with("POST /oauth/token "));
            let body = raw.split_once("\r\n\r\n").ok_or("token body absent")?.1;
            assert!(body.contains("grant_type=authorization_code"));
            assert!(body.contains("code=private-browser-code"));
            assert!(body.contains("code_verifier="));
            observed.fetch_add(1, Ordering::SeqCst);
            let expiry = grapt_store::SystemClock.now().as_millis() / 1000 + 3 * 24 * 3600;
            let access = format!("e30.{}.signature", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({"exp":expiry,"https://api.openai.com/auth":{"chatgpt_account_id":"browser-account"}}).to_string()));
            respond(&mut token, json!({"access_token":access,"refresh_token":"private-browser-refresh","expires_in":3*24*3600}))?;
            let mut models = accept(&listener)?;
            assert!(request(&mut models)?.starts_with("GET /models?client_version="));
            respond(&mut models, json!({"models":[{"slug":"gpt-5.6","context_window":272000,"supported_reasoning_levels":[]}]}))?;
            Ok(())
        })().map_err(|error| error.to_string())
    });
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
            .env("GRAPT_CHATGPT_SIGN_IN_LOOPBACK", &endpoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let ready = process.ready(&profile);
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
    let cancelled = client.request(
        "POST",
        route,
        r#"{"methodID":"chatgpt-browser","inputs":{}}"#,
    )?;
    let cancel_url = grapt_login::chatgpt::loopback_client(&endpoint)?
        .get(
            cancelled["data"]["url"]
                .as_str()
                .ok_or("cancel URL absent")?,
        )
        .build()?
        .url()
        .clone();
    let redirect = cancel_url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .ok_or("cancel callback absent")?;
    let abandoned: SocketAddr = redirect
        .strip_prefix("http://")
        .and_then(|url| url.strip_suffix("/auth/callback"))
        .ok_or("cancel address absent")?
        .parse()?;
    let cancel_id = cancelled["data"]["attemptID"]
        .as_str()
        .ok_or("cancel attempt absent")?;
    assert_eq!(
        client
            .response("DELETE", &format!("{route}/{cancel_id}"), "", true)?
            .0,
        204
    );
    let mut closed = false;
    for _ in 0..100 {
        if TcpListener::bind(abandoned).is_ok() {
            closed = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        closed,
        "cancelled browser attempt retained its callback listener"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    let start = client.request(
        "POST",
        route,
        r#"{"methodID":"chatgpt-browser","inputs":{}}"#,
    )?;
    let _typed: grapt_api::u3a::IntegrationOAuthConnectResponseDto =
        serde_json::from_value(start.clone())?;
    assert_eq!(start["data"]["mode"], "auto");
    let id = start["data"]["attemptID"]
        .as_str()
        .ok_or("attempt absent")?;
    let status = format!("{route}/{id}");
    let url = grapt_login::chatgpt::loopback_client(&endpoint)?
        .get(start["data"]["url"].as_str().ok_or("URL absent")?)
        .build()?
        .url()
        .clone();
    let parameter = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .ok_or("authorization parameter absent")
    };
    assert_eq!(parameter("code_challenge_method")?, "S256");
    assert_eq!(parameter("code_challenge")?.len(), 43);
    let redirect = parameter("redirect_uri")?;
    let address: SocketAddr = redirect
        .strip_prefix("http://")
        .and_then(|url| url.strip_suffix("/auth/callback"))
        .ok_or("callback absent")?
        .parse()?;
    let target = format!(
        "/auth/callback?state={}&code=private-browser-code",
        parameter("state")?
    );
    assert!(callback(address, &target, "untrusted.example")?.starts_with("HTTP/1.1 400 "));
    assert!(callback(
        address,
        "/auth/callback?state=wrong&code=private-browser-code",
        &address.to_string()
    )?
    .starts_with("HTTP/1.1 400 "));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        client.request("GET", &status, "")?["data"]["status"],
        "pending"
    );
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"],
        Value::Null
    );
    let response = callback(address, &target, &address.to_string())?;
    assert!(response.starts_with("HTTP/1.1 200 "));
    assert!(!response.contains("private-browser-code"));
    let mut completed = false;
    for _ in 0..1000 {
        let response = client.request("GET", &status, "")?;
        assert!(!response.to_string().contains("private-browser"));
        match response["data"]["status"].as_str() {
            Some("complete") => {
                completed = true;
                break;
            }
            Some("pending") => thread::sleep(Duration::from_millis(10)),
            _ => return Err(format!("browser sign-in refused: {response}").into()),
        }
    }
    assert!(completed, "browser sign-in did not complete");
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"]["providerID"],
        "chatgpt"
    );
    assert!(process.0.try_wait()?.is_none());
    issuer.join().map_err(|_| "browser fixture panicked")??;
    process.0.kill()?;
    process.0.wait()?;
    reset(&root);
    Ok(())
}

//! Empty local input keeps the same verified, destination-bound credential path.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn explicit_endpoints_verify_before_install_and_only_local_http_accepts_empty_keys() -> TestResult {
    let root = root("explicit-local-endpoints");
    reset(&root);
    let profile = root.join("profile");
    assert!(Command::new(shipped_cli::product_binary())
        .env(
            grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
            grapt_login::profile_security::isolate_test_keystore()?
        )
        .args(["init-profile", "--service"])
        .arg(&profile)
        .output()?
        .status
        .success());
    let approval = grapt_platform::profile_configuration::ProfileConfiguration::capture(&profile)?
        .approval()
        .encoded()?;
    let mut process = RestartProcess(
        grapt_service::launcher_environment::command(shipped_cli::product_binary())
            .env(
                grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
                grapt_login::profile_security::isolate_test_keystore()?,
            )
            .args([
                "serve-terminal",
                "--profile-approval",
                &approval,
                "--service",
            ])
            .arg(&profile)
            .args(["--port", "0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let ready = process.ready_with_polls(&profile, 3000);
    let client = Client {
        address: ready["observed_endpoint"]
            .as_str()
            .and_then(|url| url.strip_prefix("http://"))
            .ok_or("service endpoint absent")?
            .parse()?,
        authentication: base64::engine::general_purpose::STANDARD
            .encode(fs::read(profile.join("basic.credential"))?),
    };
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let before = grapt_connections::store::Store::read_snapshot(&profile)?;
    for (provider, endpoint) in [
        ("lmstudio", base.replacen("http://", "https://", 1)),
        ("lmstudio", "http://example.invalid/v1".into()),
        ("lmstudio", format!("{base}?key=fixture")),
        (
            "lmstudio",
            format!("http://fixture@{}/v1", listener.local_addr()?),
        ),
        ("infomaniak", base.clone()),
        ("neon", base.clone()),
        ("snowflake-cortex", base.clone()),
        ("custom", base.replacen("http://", "https://", 1)),
    ] {
        let body = json!({"key":"","custom":{
            "endpoint":endpoint,"model":"fixture-model","context_window":65536
        }})
        .to_string();
        let route = format!("/api/integration/{provider}/connect/key");
        assert_eq!(
            client.response("POST", &route, &body, true)?.0,
            400,
            "{provider}"
        );
        assert_eq!(
            grapt_connections::store::Store::read_snapshot(&profile)?,
            before
        );
    }
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let fixture = thread::spawn(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let mut expected: std::collections::VecDeque<&str> = [
                "local",
                "synthetic-endpoint-token",
                "synthetic-endpoint-token",
            ]
            .into_iter()
            .collect();
            while let Some(wanted) = expected.pop_front() {
                let mut accepted = None;
                for _ in 0..3000 {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            accepted = Some(socket);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                let mut socket = accepted.ok_or("model verification absent")?;
                socket.set_nonblocking(false)?;
                socket.set_read_timeout(Some(Duration::from_secs(5)))?;
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
                    let mut byte = [0];
                    socket.read_exact(&mut byte)?;
                    head.push(byte[0]);
                }
                let head = String::from_utf8(head)?.to_ascii_lowercase();
                // The connect may ask for the endpoint's optional catalogue on
                // its way to the completion check. That read is best effort and
                // is not what this test witnesses, so it is answered without
                // consuming an expected verification; the POST below is still
                // required once per connect, in order.
                if head.starts_with("get /v1/models http/1.1\r\n") {
                    let body = r#"{"data":[{"id":"fixture-model"}]}"#;
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
                    // Re-queue this connect's verification expectation.
                    expected.push_front(wanted);
                    continue;
                }
                assert!(head.starts_with("post /v1/chat/completions http/1.1\r\n"));
                assert!(head.contains(&format!("authorization: bearer {wanted}\r\n")));
                assert!(!head.contains("x-api-key:"));
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .ok_or("body length absent")?
                    .parse::<usize>()?;
                assert!(length <= 8192);
                let mut bytes = vec![0; length];
                socket.read_exact(&mut bytes)?;
                let body: Value = serde_json::from_slice(&bytes)?;
                assert_eq!(body["model"], "fixture-model");
                assert_eq!(body["stream"], false);
                observed.fetch_add(1, Ordering::SeqCst);
                let body = r#"{"choices":[{"message":{"role":"assistant","content":"ready"},"finish_reason":"stop"}]}"#;
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len())?;
            }
            Ok(())
        },
    );
    for (provider, key) in [
        ("lmstudio", ""),
        ("infomaniak", "synthetic-endpoint-token"),
        ("neon", "synthetic-endpoint-token"),
    ] {
        let body = json!({"key":key,"custom":{
            "endpoint":base,"model":"fixture-model","context_window":65536
        }})
        .to_string();
        let route = format!("/api/integration/{provider}/connect/key");
        assert_eq!(client.response("POST", &route, &body, false)?.0, 401);
        client.request("POST", &route, &body)?;
        let records = grapt_connections::store::Store::read_snapshot(&profile)?
            .ok_or("connections absent")?;
        let connection = records
            .connections
            .iter()
            .find(|record| record.id.as_str() == provider)
            .ok_or("verified connection absent")?;
        assert_eq!(connection.endpoint.as_deref(), Some(base.as_str()));
        assert_eq!(
            connection.family,
            grapt_connections::store::Family::OpenaiCompatible
        );
        assert!(matches!(&connection.credential_format,
            grapt_connections::store::CredentialFormat::Vault { binding }
            if binding.check_api(&base).is_ok()));
        assert_eq!(
            records
                .default_model
                .as_ref()
                .ok_or("default model absent")?
                .model
                .as_str(),
            "fixture-model"
        );
    }
    fixture
        .join()
        .map_err(|_| "endpoint fixture stopped")?
        .map_err(|error| error.to_string())?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(
        !fs::read_to_string(profile.join("connections.json"))?.contains("synthetic-endpoint-token")
    );
    process.kill_and_reap();
    fs::remove_dir_all(root)?;
    Ok(())
}

/// A running product and one connect endpoint, shared by the discovery
/// witnesses below so each test states only the discovery behaviour it proves.
struct ConnectFixture {
    process: RestartProcess,
    client: Client,
    base: String,
    listener: TcpListener,
    profile: std::path::PathBuf,
    root: std::path::PathBuf,
}

fn connect_fixture(label: &str) -> TestResult<ConnectFixture> {
    let root = root(label);
    reset(&root);
    let profile = root.join("profile");
    assert!(Command::new(shipped_cli::product_binary())
        .env(
            grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
            grapt_login::profile_security::isolate_test_keystore()?
        )
        .args(["init-profile", "--service"])
        .arg(&profile)
        .output()?
        .status
        .success());
    let approval = grapt_platform::profile_configuration::ProfileConfiguration::capture(&profile)?
        .approval()
        .encoded()?;
    let mut process = RestartProcess(
        grapt_service::launcher_environment::command(shipped_cli::product_binary())
            .env(
                grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
                grapt_login::profile_security::isolate_test_keystore()?,
            )
            .args([
                "serve-terminal",
                "--profile-approval",
                &approval,
                "--service",
            ])
            .arg(&profile)
            .args(["--port", "0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let ready = process.ready_with_polls(&profile, 3000);
    let client = Client {
        address: ready["observed_endpoint"]
            .as_str()
            .and_then(|url| url.strip_prefix("http://"))
            .ok_or("service endpoint absent")?
            .parse()?,
        authentication: base64::engine::general_purpose::STANDARD
            .encode(fs::read(profile.join("basic.credential"))?),
    };
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    Ok(ConnectFixture {
        process,
        client,
        base,
        listener,
        profile,
        root,
    })
}

/// What the endpoint observed, so a witness can prove both requests happened
/// rather than only that the install finished.
#[derive(Default)]
struct Observed {
    discoveries: AtomicUsize,
    verifications: AtomicUsize,
    release: std::sync::Mutex<bool>,
    released: std::sync::Condvar,
}

impl Observed {
    /// Hold a silent connection until the test says the install is done.
    fn hold(&self) {
        let mut released = self
            .release
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while !*released {
            released = self
                .released
                .wait(released)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn is_released(&self) -> bool {
        *self
            .release
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn signal_release(&self) {
        *self
            .release
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
        self.released.notify_all();
    }
}

struct ReleaseGuard(Arc<Observed>);

impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        self.0.signal_release();
    }
}

fn read_http_request(socket: &mut std::net::TcpStream) -> Option<String> {
    let _ = socket.set_nonblocking(false);
    let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = socket.set_write_timeout(Some(Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
        if socket.read_exact(&mut byte).is_err() {
            return None;
        }
        head.push(byte[0]);
    }
    if !head.ends_with(b"\r\n\r\n") {
        return None;
    }
    let header_text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == "content-length").then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    if content_length > 256 * 1024 {
        return None;
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        if socket.read_exact(&mut body).is_err() {
            return None;
        }
    }
    Some(header_text)
}

impl ConnectFixture {
    /// Connect one explicit endpoint through the real product. The endpoint
    /// records every request; `answer` decides what happens to the catalogue GET
    /// (refuse it, or hold it open), while the verification POST is always
    /// answered. Returns what the endpoint observed, after joining it.
    fn connect(mut self, answer: impl Fn(&Observed) + Send + 'static) -> TestResult<Arc<Observed>> {
        let observed = Arc::new(Observed::default());
        let _release_on_drop = ReleaseGuard(Arc::clone(&observed));
        let recorded = Arc::clone(&observed);
        // The very listener the product was told about, so a request that never
        // arrives here is a request the product never made.
        let listener = self.listener;
        listener.set_nonblocking(true)?;
        let fixture = thread::spawn(move || -> Result<(), String> {
            loop {
                let Some((mut socket, _)) = accept_polling(&listener, &recorded) else {
                    return Ok(());
                };
                let Some(head) = read_http_request(&mut socket) else {
                    continue;
                };
                if head.starts_with("get /v1/models ") {
                    recorded.discoveries.fetch_add(1, Ordering::SeqCst);
                    answer(&recorded);
                    continue;
                }
                if !head.starts_with("post ") {
                    continue;
                }
                recorded.verifications.fetch_add(1, Ordering::SeqCst);
                let body = r#"{"choices":[{"message":{"role":"assistant","content":"ready"},"finish_reason":"stop"}]}"#;
                let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = socket.flush();
            }
        });
        let body = json!({"key":"synthetic-endpoint-token","custom":{
            "endpoint":self.base,"model":"fixture-model","context_window":65536
        }})
        .to_string();
        let route = "/api/integration/infomaniak/connect/key";
        // The install is asserted inside a bound shorter than any stub-side
        // close, so a released wait must be the product's discovery timeout.
        let bound = Duration::from_secs(20);
        let started = tokio::time::Instant::now();
        self.client
            .request_with_timeout("POST", route, &body, bound)?;
        assert!(
            started.elapsed() < bound,
            "the install outlived 20s, so the wait was not released by the product's own bound"
        );
        let records = grapt_connections::store::Store::read_snapshot(&self.profile)?
            .ok_or("connections absent after a best-effort discovery")?;
        assert!(
            records
                .connections
                .iter()
                .any(|row| row.id.as_str() == "infomaniak"),
            "the connection was not installed: {records:?}"
        );
        // The install has completed, so any held discovery and the accept loop
        // are released immediately before joining the fixture thread.
        observed.signal_release();
        fixture.join().map_err(|_| "endpoint fixture panicked")??;
        self.process.kill_and_reap();
        fs::remove_dir_all(self.root)?;
        Ok(observed)
    }
}

fn accept_polling(
    listener: &TcpListener,
    observed: &Observed,
) -> Option<(std::net::TcpStream, SocketAddr)> {
    for _ in 0..4000 {
        if observed.is_released() {
            return None;
        }
        match listener.accept() {
            Ok(pair) => return Some(pair),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(_) => return None,
        }
    }
    None
}

/// A refused catalogue read contributes nothing and must not fail the install.
/// The endpoint still receives the read: the witness asserts the mechanism, not
/// only the outcome.
#[test]
fn a_connect_installs_when_catalogue_discovery_is_refused() -> TestResult {
    let observed = connect_fixture("discovery-refused")?.connect(|_| {
        // Refused: the discovery connection closes without a response.
    })?;
    assert_eq!(
        observed.discoveries.load(Ordering::SeqCst),
        1,
        "the product never asked for the catalogue, so this witness proves nothing"
    );
    assert_eq!(
        observed.verifications.load(Ordering::SeqCst),
        1,
        "verification did not run exactly once"
    );
    Ok(())
}

/// A discovery connection that is accepted and never answered must not hold the
/// install open: the product's own discovery timeout releases it. The socket
/// stays open until the install has completed, so the fixture cannot end the
/// wait for the product, and the completion is asserted inside a bound shorter
/// than any stub-side close.
#[test]
fn a_connect_installs_when_catalogue_discovery_never_answers() -> TestResult {
    let observed = connect_fixture("discovery-silent")?.connect(|observed| observed.hold())?;
    assert_eq!(
        observed.discoveries.load(Ordering::SeqCst),
        1,
        "the product never asked for the catalogue, so this witness proves nothing"
    );
    assert_eq!(
        observed.verifications.load(Ordering::SeqCst),
        1,
        "verification did not run exactly once"
    );
    Ok(())
}

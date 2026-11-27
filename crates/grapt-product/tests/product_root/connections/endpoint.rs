use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn cortex_claude_setup_verifies_bearer_messages_and_keeps_its_canonical_destination() -> TestResult
{
    let root = root("cortex-messages");
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
    listener.set_nonblocking(true)?;
    let base = format!("http://{}/api/v2/cortex/v1", listener.local_addr()?);
    let fixture = thread::spawn(move || -> std::io::Result<()> {
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
                Err(error) => return Err(error),
            }
        }
        let mut socket = accepted.ok_or_else(|| std::io::Error::other("Messages check absent"))?;
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
            let mut byte = [0];
            socket.read_exact(&mut byte)?;
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).map_err(std::io::Error::other)?;
        assert!(head.starts_with("POST /api/v2/cortex/v1/messages HTTP/1.1\r\n"));
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("authorization: bearer fixture-cortex-pat\r\n"));
        assert!(!lower.contains("x-api-key:"));
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .filter(|length| *length <= 4096)
            .ok_or_else(|| std::io::Error::other("completion body exceeds its bound"))?;
        let mut bytes = vec![0; length];
        socket.read_exact(&mut bytes)?;
        let body: Value = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        assert_eq!(body["model"], "claude-sonnet-5");
        assert_eq!(body["stream"], false);
        let body = r#"{"content":[{"type":"text","text":"ready"}],"stop_reason":"end_turn"}"#;
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
        Ok(())
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
    let body = json!({"key":"fixture-cortex-pat","custom":{
        "endpoint":format!("{base}/"),"model":"claude-sonnet-5","context_window":65536
    }})
    .to_string();
    let route = "/api/integration/snowflake-cortex/connect/key";
    assert_eq!(client.response("POST", route, &body, false)?.0, 401);
    client.request("POST", route, &body)?;
    let records =
        grapt_connections::store::Store::read_snapshot(&profile)?.ok_or("connection absent")?;
    let connection = records.connections.first().ok_or("connection absent")?;
    assert_eq!(
        connection.family,
        grapt_connections::store::Family::AnthropicMessages
    );
    assert_eq!(connection.endpoint.as_deref(), Some(base.as_str()));
    assert!(matches!(&connection.credential_format,
        grapt_connections::store::CredentialFormat::MessagesBearer { destination, .. }
        if destination.as_str() == base));
    assert!(!fs::read_to_string(profile.join("connections.json"))?.contains("fixture-cortex-pat"));
    fixture.join().map_err(|_| "Messages fixture stopped")??;
    process.kill_and_reap();
    fs::remove_dir_all(root)?;
    Ok(())
}

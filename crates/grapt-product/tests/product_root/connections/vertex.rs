//! First-time Vertex publication, token renewal and restart at the public seam.
use super::*;

fn provider(listener: TcpListener) -> TestResult {
    let mut exchanges = 0;
    let mut completed = 0;
    let mut expired = 0;
    for _ in 0..12 {
        let mut socket = oauth::accept(&listener)?;
        let raw = oauth::request(&mut socket)?;
        let (head, body) = raw.split_once("\r\n\r\n").ok_or("request boundary")?;
        if head.starts_with("POST /token ") {
            exchanges += 1;
            if exchanges == 1 {
                write!(socket, "HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}")?;
            } else {
                oauth::respond(
                    &mut socket,
                    json!({"access_token":format!("fixture-access-{exchanges}"),"token_type":"Bearer","expires_in":3600}),
                )?;
            }
            continue;
        }
        assert!(head.starts_with("POST /v1/projects/fixture-project/locations/global/publishers/google/models/gemini-2.5-flash:"));
        assert!(!head.to_ascii_lowercase().contains("x-goog-api-key"));
        assert!(!body.contains("PRIVATE KEY"));
        let request: Value = serde_json::from_str(body)?;
        assert!(request["contents"].is_array());
        let original = head
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-access-2");
        if head.contains(":generateContent HTTP/1.1") {
            assert!(original);
            oauth::respond(
                &mut socket,
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"OK"}]},"finishReason":"STOP"}]}),
            )?;
            continue;
        }
        assert!(head.contains(":streamGenerateContent?alt=sse HTTP/1.1"));
        if original {
            expired += 1;
            write!(
                socket,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )?;
            continue;
        }
        assert!(head
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer fixture-access-{exchanges}")));
        let chunk = json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"Vertex fixture completed."}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":10,"totalTokenCount":110}});
        let body = format!("data: {chunk}\n\n");
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
        completed += 1;
        if completed == 2 {
            assert!(expired > 0, "401 recovery was not exercised");
            return Ok(());
        }
    }
    Err("provider request allowance exhausted".into())
}

fn credential_files(profile: &Path) -> TestResult<Vec<PathBuf>> {
    let mut files = fs::read_dir(profile)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    files.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "credential")
    });
    files.sort();
    Ok(files)
}

fn launch(profile: &Path, endpoint: &str) -> TestResult<(RestartProcess, Client)> {
    let approval = grapt_platform::profile_configuration::ProfileConfiguration::capture(profile)?
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
            .arg(profile)
            .args(["--port", "0"])
            .env(
                "GRAPT_VERTEX_TEST_TOKEN_ENDPOINT",
                format!("{endpoint}/token"),
            )
            .env(
                "GRAPT_LIVE_HEADLESS_LOOPBACK",
                format!(
                    "{endpoint}/v1/projects/fixture-project/locations/global/publishers/google"
                ),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let ready = process.ready_with_polls(profile, 3000);
    let client = Client {
        address: ready["observed_endpoint"]
            .as_str()
            .and_then(|url| url.strip_prefix("http://"))
            .ok_or("endpoint absent")?
            .parse()?,
        authentication: base64::engine::general_purpose::STANDARD
            .encode(fs::read(profile.join("basic.credential"))?),
    };
    Ok((process, client))
}

fn complete(client: &Client, session: &str, id: &str) -> TestResult {
    client.request(
        "POST",
        &format!("/api/session/{session}/prompt"),
        &json!({"id":id,"text":"Reply briefly.","delivery":"queue","resume":true}).to_string(),
    )?;
    for _ in 0..600 {
        let messages = client.request("GET", &format!("/api/session/{session}/message"), "")?;
        let rows = messages["data"].as_array().ok_or("messages absent")?;
        // The API returns newest first; a prior assistant cannot satisfy this prompt.
        let newer: Vec<_> = rows.iter().take_while(|row| row["id"] != id).collect();
        if newer.len() < rows.len()
            && newer.iter().any(|row| {
                row["type"] == "assistant"
                    && row["finish"] == "stop"
                    && row.to_string().contains("Vertex fixture completed.")
            })
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("Vertex prompt did not complete".into())
}

#[test]
fn fresh_vertex_connect_refuses_without_publication_then_renews_and_restarts() -> TestResult {
    let root = root("vertex-connection");
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
    let fixture = thread::spawn(move || provider(listener).map_err(|error| error.to_string()));
    let (mut process, client) = launch(&profile, &endpoint)?;
    // This isolated key is generated solely for the loopback fixture.
    let key = json!({"type":"service_account","private_key":include_str!("vertex-test-only.pem"),
        "client_email":"fixture@fixture-project.iam.gserviceaccount.com",
        "token_uri":"https://oauth2.googleapis.com/token"});
    let payload = json!({"key":key.to_string(),"vertex":{"project":"fixture-project","location":"global","model":"gemini-2.5-flash"}}).to_string();
    let route = "/api/integration/google-vertex/connect/key";
    let before = fs::read(profile.join("connections.json"))?;
    let credentials_before = credential_files(&profile)?;
    assert_eq!(client.response("POST", route, &payload, true)?.0, 400);
    assert_eq!(fs::read(profile.join("connections.json"))?, before);
    assert_eq!(credential_files(&profile)?, credentials_before);
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"],
        Value::Null
    );
    // The original defect rejected this fresh connection while capturing staged credentials.
    client.request("POST", route, &payload)?;
    let records =
        grapt_connections::store::Store::read_snapshot(&profile)?.ok_or("connections absent")?;
    assert_eq!(records.connections.len(), 1);
    let vault = profile.join(&records.connections[0].credential_file);
    let sealed = fs::read(&vault)?;
    if cfg!(windows) {
        assert!(grapt_login::profile_security::is_protected(&sealed));
    } else {
        assert!(sealed.starts_with(b"secode.credential-vault.v1\n"));
    }
    let metadata = fs::read(profile.join("connections.json"))?;
    assert!(!String::from_utf8_lossy(&metadata).contains("PRIVATE KEY"));
    assert_eq!(
        client
            .response("POST", route, r#"{"key":"invalid fixture"}"#, true)?
            .0,
        400
    );
    assert_eq!(fs::read(profile.join("connections.json"))?, metadata);
    assert_eq!(fs::read(&vault)?, sealed);
    let session = client.request(
        "POST",
        "/api/session",
        &json!({"id":null,
        "location":{"directory":root.to_string_lossy().replace('\\', "/")},
        "model":{"providerID":"google-vertex","id":"gemini-2.5-flash"}})
        .to_string(),
    )?;
    let id = session["data"]["id"].as_str().ok_or("session absent")?;
    complete(&client, id, "msg_vertex_first")?;
    assert_eq!(fs::read(&vault)?, sealed, "renewal rewrote the stored key");
    process.kill_and_reap();
    let (mut restarted, client) = launch(&profile, &endpoint)?;
    complete(&client, id, "msg_vertex_restarted")?;
    assert_eq!(fs::read(&vault)?, sealed, "restart rewrote the stored key");
    fixture.join().map_err(|_| "provider fixture panicked")??;
    restarted.kill_and_reap();
    fs::remove_dir_all(root)?;
    Ok(())
}

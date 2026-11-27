//! Native IBM authorization and generation through the real connection service.
use super::*;

fn denied(socket: &mut TcpStream) -> TestResult {
    write!(
        socket,
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    )?;
    Ok(())
}
fn provider(listener: TcpListener) -> TestResult {
    let mut exchanges = 0;
    let mut completed = 0;
    let mut rejected = 0;
    for _ in 0..12 {
        let mut socket = oauth::accept(&listener)?;
        let raw = oauth::request(&mut socket)?;
        let (head, body) = raw.split_once("\r\n\r\n").ok_or("request boundary")?;
        if head.starts_with("POST /identity/token ") {
            assert!(body.contains("grant_type=urn%3Aibm%3Aparams%3Aoauth%3Agrant-type%3Aapikey"));
            assert!(body.contains("apikey=fixture-ibm-cloud-api-key"));
            exchanges += 1;
            if exchanges == 1 {
                denied(&mut socket)?;
            } else {
                oauth::respond(
                    &mut socket,
                    json!({"access_token":format!("fixture-access-{exchanges}"),"token_type":"Bearer","expires_in":3600}),
                )?;
            }
            continue;
        }
        assert!(!body.contains("fixture-ibm-cloud-api-key"));
        let request: Value = serde_json::from_str(body)?;
        assert_eq!(request["model_id"], "ibm/granite-4-h-small");
        assert_eq!(request["project_id"], "fixture-project");
        assert!(request["messages"].is_array());
        assert!(request.get("model").is_none());
        assert!(!head.contains("fixture-ibm-cloud-api-key"));
        let original = head
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-access-2");
        if head.starts_with("POST /ml/v1/text/chat?version=2026-04-20 ") {
            assert!(original);
            oauth::respond(
                &mut socket,
                json!({"choices":[{"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}]}),
            )?;
            continue;
        }
        assert!(head.starts_with("POST /ml/v1/text/chat_stream?version=2026-04-20 "));
        assert_eq!(request["parallel_tool_calls"], false);
        assert!(request.get("stream").is_none() && request.get("stream_options").is_none());
        if original {
            rejected += 1;
            denied(&mut socket)?;
            continue;
        }
        assert!(head
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer fixture-access-{exchanges}")));
        let chunk = json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"IBM fixture completed."},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}});
        let body = format!("data: {chunk}\n\n");
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
        completed += 1;
        if completed == 2 {
            assert_eq!(rejected, 2);
            assert_eq!(exchanges, 4);
            return Ok(());
        }
    }
    Err("native IBM request allowance exhausted".into())
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
                "GRAPT_WATSONX_TEST_TOKEN_ENDPOINT",
                format!("{endpoint}/identity/token"),
            )
            .env("GRAPT_LIVE_HEADLESS_LOOPBACK", endpoint)
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
        let newer: Vec<_> = rows.iter().take_while(|row| row["id"] != id).collect();
        if newer.len() < rows.len()
            && newer.iter().any(|row| {
                row["type"] == "assistant"
                    && row["finish"] == "stop"
                    && row.to_string().contains("IBM fixture completed.")
            })
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("native IBM prompt did not complete".into())
}
#[test]
fn fresh_watsonx_connect_refuses_without_publication_then_renews_and_restarts() -> TestResult {
    let root = root("watsonx-connection");
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
    let route = "/api/integration/watsonx/connect/key";
    let before = fs::read(profile.join("connections.json"))?;
    let payload = json!({"key":"fixture-ibm-cloud-api-key","watsonx":{"region":"us-south","project":"fixture-project","model":"ibm/granite-4-h-small"}}).to_string();
    let before_files = credential_files(&profile)?;
    assert_eq!(client.response("POST", route, &payload, true)?.0, 400);
    assert_eq!(fs::read(profile.join("connections.json"))?, before);
    assert_eq!(
        before_files,
        credential_files(&profile)?,
        "refused authorization published a credential"
    );
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"],
        Value::Null
    );
    client.request("POST", route, &payload)?;
    let records =
        grapt_connections::store::Store::read_snapshot(&profile)?.ok_or("connections absent")?;
    assert_eq!(records.connections.len(), 1);
    assert_eq!(
        records.connections[0].family,
        grapt_connections::store::Family::WatsonxChat
    );
    assert_eq!(
        records.connections[0]
            .watsonx_project
            .as_ref()
            .ok_or("project absent")?
            .as_str(),
        "fixture-project"
    );
    let vault = profile.join(&records.connections[0].credential_file);
    let sealed = fs::read(&vault)?;
    if cfg!(windows) {
        assert!(grapt_login::profile_security::is_protected(&sealed));
    } else {
        assert!(sealed.starts_with(b"secode.credential-vault.v1\n"));
    }
    let metadata = fs::read(profile.join("connections.json"))?;
    assert!(!String::from_utf8_lossy(&metadata).contains("fixture-ibm-cloud-api-key"));
    assert_eq!(client.response("POST", route, r#"{"key":"fixture-ibm-cloud-api-key","watsonx":{"region":"us-south.foreign.invalid","project":"fixture-project","model":"ibm/granite-4-h-small"}}"#, true)?.0, 400);
    assert_eq!(fs::read(profile.join("connections.json"))?, metadata);
    assert_eq!(fs::read(&vault)?, sealed);
    let session = client.request("POST", "/api/session", &json!({"id":null,"location":{"directory":root.to_string_lossy().replace('\\', "/")},"model":{"providerID":"watsonx","id":"ibm/granite-4-h-small"}}).to_string())?;
    let id = session["data"]["id"].as_str().ok_or("session absent")?;
    complete(&client, id, "msg_ibm_first")?;
    assert_eq!(fs::read(&vault)?, sealed);
    process.kill_and_reap();
    let (mut restarted, client) = launch(&profile, &endpoint)?;
    complete(&client, id, "msg_ibm_restarted")?;
    assert_eq!(fs::read(&vault)?, sealed);
    fixture
        .join()
        .map_err(|_| "native IBM fixture panicked")??;
    restarted.kill_and_reap();
    fs::remove_dir_all(root)?;
    Ok(())
}

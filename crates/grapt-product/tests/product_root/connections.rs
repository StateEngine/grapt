//! Fresh-profile setup and live key replacement through the public server.
use super::*;
use base64::Engine;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Client {
    address: SocketAddr,
    authentication: String,
}
impl Client {
    fn response(
        &self,
        method: &str,
        path: &str,
        body: &str,
        authenticated: bool,
    ) -> TestResult<(u16, Value)> {
        self.response_with_headers(method, path, body, authenticated, &[])
    }
    fn response_with_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
        authenticated: bool,
        headers: &[(&str, &str)],
    ) -> TestResult<(u16, Value)> {
        self.response_with_headers_and_timeout(
            method,
            path,
            body,
            authenticated,
            headers,
            Duration::from_secs(120),
        )
    }
    fn response_with_headers_and_timeout(
        &self,
        method: &str,
        path: &str,
        body: &str,
        authenticated: bool,
        headers: &[(&str, &str)],
        read_timeout: Duration,
    ) -> TestResult<(u16, Value)> {
        let mut socket = TcpStream::connect_timeout(&self.address, Duration::from_secs(5))?;
        socket.set_read_timeout(Some(read_timeout))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let authentication = if authenticated {
            format!("Authorization: Basic {}\r\n", self.authentication)
        } else {
            String::new()
        };
        let headers: String = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        write!(socket, "{method} {path} HTTP/1.1\r\nHost: {}\r\n{authentication}{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", self.address, body.len())?;
        let mut wire = String::new();
        socket.take(256 * 1024 + 1).read_to_string(&mut wire)?;
        assert!(
            wire.len() <= 256 * 1024,
            "setup response exceeded its bound"
        );
        let (head, body) = wire
            .split_once("\r\n\r\n")
            .ok_or("HTTP response boundary missing")?;
        let status = head
            .split_whitespace()
            .nth(1)
            .ok_or("HTTP status missing")?
            .parse()?;
        Ok((
            status,
            if body.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(body)?
            },
        ))
    }
    fn request(&self, method: &str, path: &str, body: &str) -> TestResult<Value> {
        self.request_with_timeout(method, path, body, Duration::from_secs(120))
    }
    fn request_with_timeout(
        &self,
        method: &str,
        path: &str,
        body: &str,
        read_timeout: Duration,
    ) -> TestResult<Value> {
        let (status, value) =
            self.response_with_headers_and_timeout(method, path, body, true, &[], read_timeout)?;
        assert!(
            matches!(status, 200 | 204),
            "{method} {path} refused: {status}: {value}"
        );
        Ok(value)
    }
}

struct KeyProviderFixture {
    endpoint: String,
    task: thread::JoinHandle<Result<(), String>>,
    calls: Arc<AtomicUsize>,
}

fn key_provider() -> TestResult<KeyProviderFixture> {
    key_provider_with_requests(8, 12000)
}

fn key_provider_with_requests(expected: usize, polls: usize) -> TestResult<KeyProviderFixture> {
    key_provider_script(expected, polls, |request, body| {
        let listing = request.starts_with("GET /models HTTP/1.1\r\n");
        let completion = request.starts_with("POST /responses HTTP/1.1\r\n");
        if !(listing || completion) {
            return Err("unexpected key-verification request".into());
        }
        if completion && body["model"] != "gpt-5.6" {
            return Err("unexpected connection-check model".into());
        }
        let valid = request
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-valid-");
        Ok(
            if completion && request.contains("fixture-valid-forbidden") {
                ("403 Forbidden", "{}")
            } else if !valid {
                ("401 Unauthorized", "{}")
            } else if completion {
                (
                    "200 OK",
                    r#"{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello"}]}]}"#,
                )
            } else {
                ("200 OK", r#"{"data":[{"id":"gpt-5.6"}]}"#)
            },
        )
    })
}

fn key_provider_script(
    expected: usize,
    polls: usize,
    reply: impl Fn(&str, &Value) -> Result<(&'static str, &'static str), String> + Send + 'static,
) -> TestResult<KeyProviderFixture> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    listener.set_nonblocking(true)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let task = thread::spawn(move || {
        for index in 0..expected {
            let mut accepted = None;
            for _ in 0..polls {
                match listener.accept() {
                    Ok((socket, _)) => {
                        accepted = Some(socket);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            let mut socket = accepted.ok_or_else(|| {
                format!(
                    "fixture did not receive key-verification request {}",
                    index + 1
                )
            })?;
            socket
                .set_nonblocking(false)
                .map_err(|error| error.to_string())?;
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| error.to_string())?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") && request.len() <= 8192 {
                let mut byte = [0];
                if socket.read(&mut byte).map_err(|error| error.to_string())? == 0 {
                    break;
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).map_err(|_| "fixture request is not UTF-8")?;
            observed.fetch_add(1, Ordering::SeqCst);
            if request.len() > 8192 {
                return Err("key-verification request headers exceed their fixture bound".into());
            }
            let body = if request.starts_with("POST ") {
                let length = request
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .filter(|length| *length <= 4096)
                    .ok_or("completion request body exceeds its fixture bound")?;
                let mut body = vec![0; length];
                socket
                    .read_exact(&mut body)
                    .map_err(|error| error.to_string())?;
                serde_json::from_slice(&body).map_err(|_| "invalid completion request")?
            } else {
                Value::Null
            };
            let (status, body) = reply(&request, &body)?;
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).map_err(|error| error.to_string())?;
        }
        Ok(())
    });
    Ok(KeyProviderFixture {
        endpoint,
        task,
        calls,
    })
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn fresh_profile_connects_and_replaces_keys_without_restarting_or_losing_a_session() -> TestResult {
    let root = root("connection-setup");
    reset(&root);
    let profile = root.join("profile");
    let initialized = Command::new(shipped_cli::product_binary())
        .args(["init-profile", "--service"])
        .arg(&profile)
        .output()?;
    assert!(
        initialized.status.success(),
        "empty profile initialization refused: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let KeyProviderFixture {
        endpoint: provider,
        task: provider_task,
        calls: provider_calls,
    } = key_provider()?;
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
            .env("MISTRAL_API_KEY", "fixture-catalog-key")
            .env("XAI_API_KEY", "fixture-catalog-key")
            .env("CEREBRAS_API_KEY", "fixture-catalog-key")
            .env("GRAPT_LIVE_HEADLESS_LOOPBACK", provider)
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
            .ok_or("setup endpoint missing")?
            .parse()?,
        authentication: base64::engine::general_purpose::STANDARD
            .encode(fs::read(profile.join("basic.credential"))?),
    };
    let integrations = client.request("GET", "/api/integration", "")?;
    for (id, endpoint, environment) in [
        ("mistral", "https://api.mistral.ai/v1", "MISTRAL_API_KEY"),
        ("xai", "https://api.x.ai/v1", "XAI_API_KEY"),
        ("cerebras", "https://api.cerebras.ai/v1", "CEREBRAS_API_KEY"),
    ] {
        let row = integrations["data"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == id))
            .ok_or("compatible provider is missing from Connect")?;
        let _: grapt_api::u3a::IntegrationInfoDto = serde_json::from_value(row.clone())?;
        let methods = row["methods"].as_array().ok_or("Connect methods missing")?;
        assert!(methods.iter().any(|method| method["type"] == "key"), "{id}");
        assert!(
            methods.iter().any(|method| {
                method["type"] == "env"
                    && method["names"]
                        .as_array()
                        .is_some_and(|names| names.iter().any(|name| name == environment))
            }),
            "{id}"
        );
        let provider = grapt_connections::catalog::KeyProvider::from_id(id)?
            .ok_or("Connect provider has no key verifier")?;
        assert_eq!(provider.api_base(), endpoint);
        assert_eq!(
            provider.family(),
            grapt_connections::store::Family::OpenaiCompatible
        );
        assert_eq!(provider.placement(), grapt_login::key::KeyPlacement::Bearer);
        assert_eq!(provider.route(), "chat-completions");
        assert!(provider.allows_environment(environment)?);
        assert!(!provider.allows_environment("UNRELATED_API_KEY")?);
    }
    assert!(!integrations.to_string().contains("fixture-catalog-key"));
    for id in [
        "github-copilot",
        "github-copilot-enterprise",
        "amazon-bedrock-mantle",
    ] {
        assert!(grapt_connections::catalog::KeyProvider::from_id(id)?.is_none());
    }
    assert!(integrations["data"]
        .as_array()
        .ok_or("integrations")?
        .iter()
        .all(|row| {
            row["methods"]
                .as_array()
                .is_some_and(|methods| methods.iter().all(|method| method["type"] != "command"))
        }));
    let command = "/api/integration/openai/connect/command";
    for body in [
        r#"{"methodID":"key"}"#,
        r#"{"methodID":"login"}"#,
        r#"{"methodID":"login","command":["unregistered-program"]}"#,
    ] {
        assert_eq!(client.response("POST", command, body, true)?.0, 400);
    }
    assert_eq!(
        client
            .response("POST", command, r#"{"methodID":"login"}"#, false)?
            .0,
        401
    );
    let missing = format!("{command}/missing");
    assert_eq!(client.response("GET", &missing, "", true)?.0, 400);
    assert_eq!(client.response("DELETE", &missing, "", true)?.0, 204);
    assert_eq!(client.response("DELETE", &missing, "", true)?.0, 204);
    let key_route = "/api/integration/openai/connect/key";
    let invalid = r#"{"key":"fixture-invalid"}"#;
    assert_eq!(client.response("POST", key_route, invalid, false)?.0, 401);
    let (status, refusal) = client.response("POST", key_route, r#"{"key":""}"#, true)?;
    assert_eq!(status, 400);
    assert_eq!(
        refusal,
        json!({"_tag":"InvalidRequestError","kind":"integration_authorization","message":"Authentication failed"})
    );
    let listed = client.request("GET", "/api/integration", "")?;
    let openai = listed["data"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == "openai"))
        .ok_or("OpenAI integration absent")?;
    assert!(openai["methods"]
        .as_array()
        .is_some_and(|methods| methods.iter().any(|method| method["type"] == "key")));
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"],
        Value::Null
    );
    assert_eq!(
        client.request("GET", "/api/integration/unknown-provider", "")?["data"],
        Value::Null
    );
    let original = fs::read(profile.join("connections.json"))?;
    let (status, refusal) = client.response("POST", key_route, invalid, true)?;
    assert_eq!(status, 400);
    assert_eq!(
        refusal,
        json!({"_tag":"InvalidRequestError","kind":grapt_api::u3a::INTEGRATION_KEY_REJECTED_KIND,"message":"the provider rejected this key"})
    );
    assert_eq!(fs::read(profile.join("connections.json"))?, original);
    client.request(
        "POST",
        key_route,
        r#"{"key":"fixture-valid-first","label":"First key"}"#,
    )?;
    let first = grapt_connections::store::Store::read_snapshot(&profile)?
        .ok_or("installed connection missing")?;
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"]["providerID"],
        "openai"
    );
    let input = json!({"id":null,"location":{"directory":root.to_string_lossy().replace('\\', "/")},"model":{"providerID":"openai","id":"gpt-5.6"}});
    let session = client.request("POST", "/api/session", &input.to_string())?;
    let id = session["data"]["id"]
        .as_str()
        .ok_or("session identity missing")?;
    let calls_before = provider_calls.load(Ordering::SeqCst);
    let mut tampered = first.clone();
    tampered.connections[0].endpoint = Some("https://unapproved.example/v1".into());
    grapt_connections::store::Store::open(&profile)?.save(&tampered)?;
    assert_eq!(client.response("POST", key_route, invalid, true)?.0, 400);
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        calls_before,
        "unapproved endpoint received key verification"
    );
    // Status may recover journals only after the repository profile's source
    // approval has been checked, just like a connection change.
    let staged = profile.join(".connections.json.new");
    let mut staged_options = fs::OpenOptions::new();
    staged_options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut staged_options, 0o600);
    let mut staged_file = staged_options.open(&staged)?;
    grapt_login::profile_security::protect_new_private_file(&staged, &staged_file)?;
    staged_file.write_all(&tampered.to_vec()?)?;
    drop(staged_file);
    assert_eq!(client.response("GET", "/api/auth/status", "", true)?.0, 400);
    assert!(
        staged.exists(),
        "unapproved status recovered connection staging"
    );
    fs::remove_file(staged)?;
    grapt_connections::store::Store::open(&profile)?.save(&first)?;
    client.request(
        "POST",
        key_route,
        r#"{"key":"fixture-valid-replacement","label":"Replacement key"}"#,
    )?;
    let replaced = grapt_connections::store::Store::read_snapshot(&profile)?
        .ok_or("replacement connection missing")?;
    assert_ne!(
        first.connections[0].credential_version,
        replaced.connections[0].credential_version
    );
    let credential = "/api/credential/cred_openai";
    let rename = r#"{"label":"Work API key"}"#;
    let secret_before = fs::read(profile.join(&replaced.connections[0].credential_file))?;
    let calls_before_rename = provider_calls.load(Ordering::SeqCst);
    assert_eq!(client.response("PATCH", credential, rename, false)?.0, 401);
    assert_eq!(
        client
            .response("PATCH", credential, r#"{"label":""}"#, true)?
            .0,
        400
    );
    assert_eq!(client.response("PATCH", credential, rename, true)?.0, 204);
    assert_eq!(
        client.request("GET", "/api/integration/openai", "")?["data"]["connections"][0]["label"],
        "Work API key"
    );
    assert_eq!(provider_calls.load(Ordering::SeqCst), calls_before_rename);
    assert_eq!(
        fs::read(profile.join(&replaced.connections[0].credential_file))?,
        secret_before
    );
    let mut renamed = replaced.clone();
    renamed.connections[0].label = "Work API key".into();
    assert_eq!(
        grapt_connections::store::Store::read_snapshot(&profile)?,
        Some(renamed.clone())
    );
    let resumed = client.request("GET", &format!("/api/session/{id}"), "")?;
    assert_eq!(resumed["data"]["id"], id);
    assert_eq!(resumed["data"]["model"], session["data"]["model"]);
    let (status, refusal) = client.response("POST", key_route, invalid, true)?;
    assert_eq!(status, 400);
    assert!(!refusal.to_string().contains("fixture-invalid"));
    assert_eq!(
        refusal,
        json!({"_tag":"InvalidRequestError","kind":grapt_api::u3a::INTEGRATION_KEY_REJECTED_KIND,"message":"the provider rejected this key"})
    );
    assert_eq!(
        grapt_connections::store::Store::read_snapshot(&profile)?,
        Some(renamed)
    );
    assert_eq!(client.response("DELETE", credential, "", false)?.0, 401);
    assert_eq!(client.response("DELETE", credential, "", true)?.0, 204);
    assert_eq!(client.response("DELETE", credential, "", true)?.0, 204);
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"],
        Value::Null
    );
    assert!(client.request("GET", "/api/provider", "")?["data"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert_eq!(
        client.request("GET", &format!("/api/session/{id}"), "")?["data"]["id"],
        id
    );
    assert!(grapt_connections::store::Store::read_snapshot(&profile)?
        .ok_or("metadata absent after removal")?
        .connections
        .is_empty());
    assert!(!profile.join(&first.connections[0].credential_file).exists());
    assert!(!profile
        .join(&replaced.connections[0].credential_file)
        .exists());
    assert!(profile.join("basic.credential").exists());
    client.request("POST", key_route, r#"{"key":"fixture-valid-reconnect"}"#)?;
    assert_eq!(
        client.request("GET", &format!("/api/session/{id}"), "")?["data"]["id"],
        id
    );
    assert!(
        matches!(process.0.try_wait(), Ok(None)),
        "setup changed server lifetime"
    );
    provider_task.join().map_err(|_| "key fixture stopped")??;
    process.kill_and_reap();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[path = "connections/oauth.rs"]
mod oauth;

#[path = "connections/cli.rs"]
mod cli;

#[cfg(feature = "loopback-provider-testkit")]
#[path = "connections/endpoint.rs"]
mod endpoint;
#[cfg(feature = "loopback-provider-testkit")]
#[path = "connections/vertex.rs"]
mod vertex;
#[cfg(feature = "loopback-provider-testkit")]
#[path = "connections/watsonx.rs"]
mod watsonx;

#[cfg(feature = "loopback-provider-testkit")]
#[path = "connections/local_endpoint.rs"]
mod local_endpoint;

#[path = "connections/legacy_headers.rs"]
mod legacy_headers;

//! Public volatile-server input: user file effects survive, harness state does not.
use super::*;
use base64::Engine;
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader};
use std::sync::mpsc;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    root: PathBuf,
    permissions: Vec<(PathBuf, fs::Permissions)>,
}
impl Fixture {
    fn protect(&mut self, path: PathBuf) -> io::Result<()> {
        let original = fs::metadata(&path)?.permissions();
        let mut protected = original.clone();
        protected.set_readonly(true);
        self.permissions.push((path.clone(), original));
        fs::set_permissions(path, protected)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for (path, permissions) in self.permissions.iter().rev() {
            let _ = fs::set_permissions(path, permissions.clone());
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

// Check throughout readiness/completion polling, not only after cleanup could
// conceal a persisted file. This is not an OS-level trace of every filesystem
// operation; Unix also denies directory writes, Windows protects input files.
fn assert_no_harness_state(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.starts_with("database.sqlite3")
                && !name.starts_with("registration")
                && !matches!(
                    name.as_ref(),
                    "artifacts"
                        | "dogfood-artifacts"
                        | "restore-journal"
                        | "sessions"
                        | "serve-ready.json"
                        | ".serve-ready.lock"
                        | "server.log"
                        | "logs"
                        | "chatgpt.oauth.json"
                        | "chatgpt.lock"
                        | "mcp-oauth.lock"
                ),
            "volatile service generated harness state: {}",
            entry.path().display()
        );
        // git init belongs to fixture setup, not to the session/harness.
        let file_type = match entry.file_type() {
            Ok(kind) => kind,
            // Intentional coding effects may retire their staging file after
            // read_dir. A forbidden harness name was already rejected above.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if name != ".git" && file_type.is_dir() {
            assert_no_harness_state(&entry.path())?;
        }
    }
    Ok(())
}

struct Client {
    address: SocketAddr,
    authorization: String,
}
impl Client {
    fn from_ready(ready: &Value) -> TestResult<Self> {
        let address = ready["endpoint"]
            .as_str()
            .and_then(|endpoint| endpoint.strip_prefix("http://"))
            .ok_or("volatile endpoint absent")?
            .parse()?;
        let credentials = format!(
            "{}:{}",
            ready["username"].as_str().ok_or("username absent")?,
            ready["secret"].as_str().ok_or("secret absent")?
        );
        Ok(Self {
            address,
            authorization: base64::engine::general_purpose::STANDARD.encode(credentials),
        })
    }

    fn request(&self, method: &str, path: &str, body: &str) -> TestResult<Value> {
        let mut stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Basic {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", self.address, self.authorization, body.len())?;
        let mut wire = String::new();
        stream.take(256 * 1024 + 1).read_to_string(&mut wire)?;
        assert!(wire.len() <= 256 * 1024, "volatile response exceeded bound");
        let (head, body) = wire
            .split_once("\r\n\r\n")
            .ok_or("HTTP response boundary absent")?;
        let status = head
            .split_whitespace()
            .nth(1)
            .ok_or("HTTP status absent")?
            .parse::<u16>()?;
        assert!(
            matches!(status, 200 | 204),
            "{method} {path} refused: {status}: {body}"
        );
        Ok(if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(body)?
        })
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn public_ephemeral_server_patches_without_creating_session_or_harness_files() -> TestResult {
    let root = root("ephemeral-absence");
    reset(&root);
    let mut fixture = Fixture {
        root,
        permissions: Vec::new(),
    };
    let service = fixture.root.join("profile");
    let workspace = fixture.root.join("workspace");
    for path in [
        &service,
        &workspace,
        &fixture.root.join("home"),
        &fixture.root.join("config"),
    ] {
        fs::create_dir(path)?;
    }
    write_inputs(&service);
    let arguments = json!({"input":"*** Begin Patch\n*** Add File: user.txt\n+intentional user effect\n*** End Patch\n"});
    let events = [
        json!({"type":"response.output_item.added","item":{"type":"function_call","id":"item_patch","call_id":"call_patch","name":"apply_patch"}}),
        json!({"type":"response.function_call_arguments.delta","item_id":"item_patch","delta":arguments.to_string()}),
        json!({"type":"response.function_call_arguments.done","item_id":"item_patch"}),
        json!({"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    let script = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    let provider = scripted_provider::Server::start(vec![script], |_| false)?;
    provider_profile::configure(&service, provider.address)?;
    let settings = fs::read_to_string(service.join("service.conf"))?.replace(
        "serve_window_ms=1000\n",
        "serve_window_ms=60000\npermission_preset=author\n",
    );
    fs::write(service.join("service.conf"), settings)?;
    let inputs: Vec<_> = [
        "grapt.jsonc",
        "service.conf",
        "account-models.json",
        "provider.credential",
        "basic.credential",
    ]
    .into_iter()
    .map(|name| {
        let path = service.join(name);
        fs::read(&path).map(|bytes| (path, bytes))
    })
    .collect::<io::Result<_>>()?;
    for (path, _) in &inputs {
        fixture.protect(path.clone())?;
    }
    #[cfg(unix)]
    fixture.protect(service.clone())?;
    assert_no_harness_state(&fixture.root)?;
    let mut process = RestartProcess(
        grapt_service::launcher_environment::command(shipped_cli::product_binary())
            .args(["serve-ephemeral", "--service"])
            .arg(&service)
            .current_dir(&workspace)
            .env_remove("GRAPT_CONFIG")
            .env_remove("SECODE_CONFIG") // grapt-rename: keep
            .env_remove("GRAPT_CONFIG_CONTENT")
            .env_remove("SECODE_CONFIG_CONTENT") // grapt-rename: keep
            .env("GRAPT_TEST_HOME", fixture.root.join("home"))
            .env("GRAPT_CONFIG_DIR", fixture.root.join("config"))
            .env("XDG_DATA_HOME", fixture.root.join("data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stdout = process
        .0
        .stdout
        .take()
        .ok_or("volatile readiness pipe absent")?;
    let (send, receive) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout.take(16 * 1024))
            .read_line(&mut line)
            .map(|_| line);
        let _ = send.send(result);
    });
    let mut ready = None;
    for _ in 0..1000 {
        assert_no_harness_state(&fixture.root)?;
        match receive.recv_timeout(Duration::from_millis(10)) {
            Ok(line) => {
                ready = Some(serde_json::from_str::<Value>(&line?)?);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let ready = ready.ok_or("volatile readiness timed out")?;
    reader
        .join()
        .map_err(|_| "volatile readiness reader panicked")?;
    let client = Client::from_ready(&ready)?;
    let session = client.request(
        "POST",
        "/api/session",
        &json!({"location":{"directory":workspace},
        "model":{"providerID":"openai", "id":"gpt-5.6"}})
        .to_string(),
    )?;
    let id = session["data"]["id"]
        .as_str()
        .ok_or("volatile session absent")?;
    client.request("POST", &format!("/api/session/{id}/prompt"),
        r#"{"text":"create user.txt using the patch tool","files":[],"agents":[],"resume":true,"delivery":"queue"}"#)?;
    let mut completed = false;
    for _ in 0..500 {
        assert_no_harness_state(&fixture.root)?;
        let messages = client.request("GET", &format!("/api/session/{id}/message"), "")?;
        let active = client.request("GET", "/api/session/active", "")?;
        let messages = messages["data"].as_array().ok_or("message page absent")?;
        let active = active["data"].as_object().ok_or("active page absent")?;
        if messages
            .iter()
            .any(|message| message["type"] == "assistant" && message["finish"] == "stop")
            && active.get(id).is_none_or(Value::is_null)
        {
            completed = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(completed, "volatile prompt did not complete");
    assert_eq!(
        fs::read(workspace.join("user.txt"))?,
        b"intentional user effect\n"
    );
    assert_no_harness_state(&fixture.root)?;
    drop(process.0.stdin.take());
    let mut stopped = false;
    for _ in 0..500 {
        assert_no_harness_state(&fixture.root)?;
        if let Some(status) = process.0.try_wait()? {
            assert!(status.success(), "volatile service failed to shut down");
            stopped = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        stopped,
        "volatile service did not close its control lifetime"
    );
    assert_no_harness_state(&fixture.root)?;
    for (path, bytes) in inputs {
        assert_eq!(fs::read(path)?, bytes, "configured input changed");
    }
    assert_eq!(
        fs::read(workspace.join("user.txt"))?,
        b"intentional user effect\n"
    );
    Ok(())
}

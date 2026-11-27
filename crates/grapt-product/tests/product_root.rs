#[cfg(feature = "loopback-provider-testkit")]
#[path = "product_root/connections.rs"]
mod connections;
#[path = "product_root/continuity.rs"]
mod continuity;
#[path = "product_root/ephemeral.rs"]
mod ephemeral;
#[path = "product_root/profile_import.rs"]
mod profile_import;
#[path = "support/provider_profile.rs"]
mod provider_profile;
#[path = "product_root/restart_process.rs"]
mod restart_process;
#[path = "support/scripted_provider.rs"]
mod scripted_provider;
#[path = "product_root/service_retirement.rs"]
mod service_retirement;
#[path = "product_root/session_form_recovery.rs"]
mod session_form_recovery;
#[path = "support/shipped_cli.rs"]
mod shipped_cli;
#[cfg(target_os = "linux")]
#[path = "product_root/shutdown_diagnostics.rs"]
mod shutdown_diagnostics;

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use grapt_store::{
    apply_migrations, preflight_read_only, EventStore, MigrationOutcome, ProductionMigrations,
    StoreReadiness,
};

fn root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("grapt-product-{}-{label}", std::process::id()))
}

fn reset(path: &PathBuf) {
    if path.exists() {
        if let Err(error) = fs::remove_dir_all(path) {
            panic!("could not reset {}: {error}", path.display());
        }
    }
    if let Err(error) = fs::create_dir_all(path) {
        panic!("could not create {}: {error}", path.display());
    }
}

fn write_inputs(root: &Path) {
    // The service's default location is a real project too; no ambient
    // ancestor repository may define this isolated fixture's identity.
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .arg(root)
        .output()
        .unwrap_or_else(|error| panic!("service repository initialization: {error}"));
    assert!(
        initialized.status.success(),
        "service repository initialization failed"
    );
    let config = "slot=product\ninstance=instance-a\nenvironment=internal-alpha\nversion=0.1.0\nserve_window_ms=1000\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=4\n";
    if let Err(error) = fs::write(root.join("service.conf"), config) {
        panic!("config write failed: {error}");
    }
    if let Err(error) = fs::write(root.join("basic.credential"), "alpha:zephyr") {
        panic!("credential write failed: {error}");
    }
    let platform = r#"{
      "store": { "authority": "open-ready" },
      "product": { "build": "build-a" },
      "provider": {
        "id": "openai",
        "model": "gpt-5.6",
        "base_model": "gpt-5.6",
        "family": "gpt-5",
        "route": "responses",
        "account": "account-a",
        "credential_version": "credential-v1",
        "policy_version": "df0-policy-v1",
        "variants": []
      }
    }"#;
    if let Err(error) = fs::write(root.join("grapt.jsonc"), platform) {
        panic!("platform config write failed: {error}");
    }
    if let Err(error) = fs::write(root.join("provider.credential"), "provider-zephyr") {
        panic!("provider credential write failed: {error}");
    }
    if let Err(error) = provider_profile::configure(root, SocketAddr::from(([127, 0, 0, 1], 1))) {
        panic!("loopback fixture profile failed: {error}");
    }
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn ready_store_serves_registers_probes_drains_and_releases() {
    let root = root("positive");
    reset(&root);
    let database = root.join("database.sqlite3");
    let store = match EventStore::open(&database, "ia0-product-test") {
        Ok(store) => store,
        Err(error) => panic!("ready database creation failed: {error}"),
    };
    drop(store);
    let migrations = match ProductionMigrations::load() {
        Ok(migrations) => migrations,
        Err(error) => panic!("production migration load failed: {error}"),
    };
    match apply_migrations(&database, &root, "0.1.0", &migrations.as_slice()) {
        Ok(MigrationOutcome::Applied { .. } | MigrationOutcome::AlreadyCurrent) => {}
        Err(error) => panic!("ready database migration failed: {error}"),
    }
    write_inputs(&root);
    let mut child =
        match grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .arg("serve")
            .arg("--service")
            .arg(&root)
            .arg("--port")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => panic!("product launch failed: {error}"),
        };
    let child_pid = child.id();
    let ready_path = root.join("serve-ready.json");
    let mut ready = None;
    // Startup may contend with a second governed Linux build. Its bounded
    // readiness wait is separate from the serve window, which starts later.
    for _ in 0..1000 {
        if let Ok(bytes) = fs::read(&ready_path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["pid"].as_u64() == Some(u64::from(child_pid)) {
                    ready = Some(value);
                    break;
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child
                    .wait_with_output()
                    .unwrap_or_else(|error| panic!("product output collection failed: {error}"));
                panic!(
                    "product exited as {status} before ready publication: stdout={} stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(None) => {}
            Err(error) => panic!("product status could not be read: {error}"),
        }
        thread::sleep(Duration::from_millis(10));
    }
    let ready = match ready {
        Some(ready) => ready,
        None => {
            child
                .kill()
                .unwrap_or_else(|error| panic!("timed-out product stop failed: {error}"));
            let output = child.wait_with_output().unwrap_or_else(|error| {
                panic!("timed-out product output collection failed: {error}")
            });
            panic!(
                "product published no ready receipt at {}: stdout={} stderr={}",
                ready_path.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    assert_eq!(
        ready["schema"], "secode.product.ready.v2",
        "ready receipt schema drifted"
    );
    assert_eq!(ready["operation"], "serve", "ready receipt verb drifted");
    assert_eq!(
        ready["serve_window_ms"], 1000,
        "ready receipt did not echo the bounded serve window"
    );
    assert_eq!(
        ready["build"], "build-a",
        "ready receipt lost product build attribution"
    );
    assert_eq!(ready["environment"], "internal-alpha");
    assert_eq!(ready["service_instance"], "instance-a");
    assert_eq!(ready["service_slot"], "product");
    assert_eq!(ready["service_generation"], 0);
    let ready_text = ready.to_string();
    assert!(
        !ready_text.contains("zephyr"),
        "ready receipt leaked a credential"
    );
    assert!(
        matches!(child.try_wait(), Ok(None)),
        "product was not serving when its ready receipt became visible"
    );
    let address: SocketAddr = match ready["observed_endpoint"]
        .as_str()
        .and_then(|raw| raw.strip_prefix("http://"))
        .and_then(|raw| raw.parse::<SocketAddr>().ok())
    {
        Some(address) if address.port() != 0 => address,
        _ => panic!("ready receipt omitted the OS-observed address: {ready}"),
    };
    let live = TcpStream::connect(address)
        .unwrap_or_else(|error| panic!("ready receipt named no live host: {error}"));
    drop(live);
    let (status, location) = location_request(address, "/api/location", true);
    assert_eq!(
        status, 200,
        "retained frontend startup location was not served"
    );
    let location = location.unwrap_or_else(|| panic!("successful location response had no body"));
    let expected_location = grapt_types::LocationRef::new(root.to_string_lossy().as_ref(), None)
        .unwrap_or_else(|error| panic!("fixture location must canonicalize: {error}"));
    assert_eq!(location["directory"], expected_location.directory());
    assert_eq!(location["project"]["directory"], location["directory"]);
    assert_eq!(
        location["project"]["id"], "global",
        "an empty repository must not invent a configured project"
    );
    let (status, unauthorized_body) = location_request(address, "/api/location", false);
    assert_eq!(
        status, 401,
        "location metadata escaped authenticated ingress"
    );
    assert_eq!(
        unauthorized_body,
        Some(serde_json::json!({"_tag":"UnauthorizedError", "message":"authentication required"})),
        "Basic ingress must return the constant typed refusal without location metadata"
    );
    for path in [
        "/api/location?location%5Bdirectory%5D=%2Fforeign",
        "/api/location?location%5Bworkspace%5D=wrk_foreign",
        "/api/location?surprise=x",
    ] {
        let (status, refusal) = location_request(address, path, true);
        assert_eq!(
            status, 400,
            "unsupported location was silently rebound: {path}"
        );
        let refusal =
            refusal.unwrap_or_else(|| panic!("location refusal had no evidence body: {path}"));
        assert_eq!(refusal["_tag"], "InvalidRequestError");
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => panic!("product output collection failed: {error}"),
    };
    assert!(
        output.status.success(),
        "product refused: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => panic!("launch receipt was not JSON: {error}"),
    };
    assert_eq!(receipt["outcome"], "served");
    assert!(receipt["pid"].is_number(), "launch pid must be numeric");
    assert_eq!(receipt["registration_released"], true);
    assert!(
        !root.join("registration").exists(),
        "registration survived shutdown"
    );
    assert!(
        !ready_path.exists(),
        "ready receipt survived its matching service generation"
    );
    assert!(
        matches!(preflight_read_only(&database), StoreReadiness::Ready),
        "owned product open did not settle back to a ready store"
    );
    let endpoint = match receipt["endpoint"].as_str() {
        Some(endpoint) => endpoint,
        None => panic!("launch receipt omitted endpoint"),
    };
    let exit_address: SocketAddr = match endpoint
        .strip_prefix("http://")
        .and_then(|raw| raw.parse().ok())
    {
        Some(address) => address,
        None => panic!("launch endpoint was not a socket address: {endpoint}"),
    };
    assert_eq!(exit_address, address, "ready and exit receipts disagreed");
    let rebound = match TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(error) => panic!("drained address did not rebind: {error}"),
    };
    drop(rebound);
}

fn location_request(
    address: SocketAddr,
    path: &str,
    authenticated: bool,
) -> (u16, Option<serde_json::Value>) {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .unwrap_or_else(|error| panic!("location endpoint connection failed: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap_or_else(|error| panic!("location read timeout failed: {error}"));
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap_or_else(|error| panic!("location write timeout failed: {error}"));
    let auth = if authenticated {
        "Authorization: Basic YWxwaGE6emVwaHly\r\n"
    } else {
        ""
    };
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\n{auth}Connection: close\r\n\r\n"
    )
    .unwrap_or_else(|error| panic!("location request write failed: {error}"));
    let mut wire = String::new();
    stream
        .take(64 * 1024)
        .read_to_string(&mut wire)
        .unwrap_or_else(|error| panic!("location response read failed: {error}"));
    let (head, body) = wire
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("location response missing HTTP boundary: {wire}"));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_else(|| panic!("location response missing status: {head}"));
    let body = if body.is_empty() {
        None
    } else {
        Some(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("location response malformed JSON evidence: {error}: {body}")
        }))
    };
    (status, body)
}

#[test]
fn absent_store_refuses_before_registration_authority() {
    let root = root("absent");
    reset(&root);
    write_inputs(&root);
    let output =
        match grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .arg("serve")
            .arg("--service")
            .arg(&root)
            .arg("--port")
            .arg("0")
            .output()
        {
            Ok(output) => output,
            Err(error) => panic!("product refusal launch failed: {error}"),
        };
    assert!(
        !output.status.success(),
        "an absent store launched the product"
    );
    assert!(
        !root.join("registration").exists(),
        "absent store created registration"
    );
    assert!(
        !root.join("registration.elected").exists(),
        "absent store acquired controller authority"
    );
    assert!(
        !root.join("registration.lock").exists(),
        "absent store acquired registration lock"
    );
}

struct RestartProcess(std::process::Child);

impl RestartProcess {
    fn launch(root: &Path) -> Self {
        Self(
            grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
                .args(["serve", "--service"])
                .arg(root)
                .args(["--port", "0"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|error| panic!("restart process launch failed: {error}")),
        )
    }

    fn ready(&mut self, root: &Path) -> serde_json::Value {
        self.ready_with_polls(root, 500)
    }

    fn ready_with_polls(&mut self, root: &Path, polls: u16) -> serde_json::Value {
        for _ in 0..polls {
            match fs::read(root.join("serve-ready.json")) {
                Ok(bytes) => {
                    let value: serde_json::Value = serde_json::from_slice(&bytes)
                        .unwrap_or_else(|error| panic!("ready receipt is corrupt: {error}"));
                    if value["pid"].as_u64() == Some(u64::from(self.0.id())) {
                        return value;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("ready receipt is unreadable: {error}"),
            }
            match self.0.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => panic!("restart exited {status}: {}", self.stdout()),
                Err(error) => panic!("restart process observation failed: {error}"),
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "restart did not publish its own ready receipt within {} milliseconds",
            u64::from(polls) * 10
        );
    }

    fn stdout(&mut self) -> String {
        let mut output = String::new();
        self.0
            .stdout
            .take()
            .unwrap_or_else(|| panic!("stdout missing"))
            .take(64 * 1024 + 1)
            .read_to_string(&mut output)
            .unwrap_or_else(|error| panic!("stdout read failed: {error}"));
        assert!(output.len() <= 64 * 1024, "stdout exceeded evidence bound");
        output
    }

    fn kill_and_reap(&mut self) {
        self.0
            .kill()
            .unwrap_or_else(|error| panic!("crash injection failed: {error}"));
        self.0
            .wait()
            .unwrap_or_else(|error| panic!("crash reap failed: {error}"));
    }

    fn exit(&mut self) -> std::process::ExitStatus {
        for _ in 0..500 {
            match self.0.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(error) => panic!("exit observation failed: {error}"),
            }
        }
        panic!("product did not exit within five seconds");
    }
}

impl Drop for RestartProcess {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn restart_request(
    ready: &serde_json::Value,
    method: &str,
    path: &str,
    body: &str,
) -> serde_json::Value {
    let (status, value) = restart_response(ready, method, path, body);
    assert!(
        matches!(status, 200 | 204),
        "request refused: {status}: {value}"
    );
    value
}

fn restart_response(
    ready: &serde_json::Value,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, serde_json::Value) {
    let address = ready["observed_endpoint"]
        .as_str()
        .and_then(|raw| raw.strip_prefix("http://"))
        .and_then(|raw| raw.parse::<SocketAddr>().ok())
        .unwrap_or_else(|| panic!("ready endpoint malformed: {ready}"));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .unwrap_or_else(|error| panic!("restart request connect failed: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap_or_else(|error| panic!("read bound failed: {error}"));
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap_or_else(|error| panic!("write bound failed: {error}"));
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic YWxwaGE6emVwaHly\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        .unwrap_or_else(|error| panic!("restart request write failed: {error}"));
    let mut wire = String::new();
    stream
        .take(64 * 1024 + 1)
        .read_to_string(&mut wire)
        .unwrap_or_else(|error| panic!("restart response read failed: {error}"));
    assert!(wire.len() <= 64 * 1024, "response exceeded evidence bound");
    let (head, body) = wire
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("response boundary absent: {wire}"));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("response status malformed: {head}"));
    if status == 204 {
        assert!(body.is_empty(), "no-content response carried a body");
        return (status, serde_json::Value::Null);
    }
    (
        status,
        serde_json::from_str(body)
            .unwrap_or_else(|error| panic!("response malformed: {error}: {body}")),
    )
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn killed_service_restarts_same_store_and_cannot_resurrect_prior_custody() {
    let root = root("crash-restart");
    reset(&root);
    let database = root.join("database.sqlite3");
    drop(
        EventStore::open(&database, "retained-restart-test")
            .unwrap_or_else(|error| panic!("store initialize failed: {error}")),
    );
    let migrations =
        ProductionMigrations::load().unwrap_or_else(|error| panic!("migrations absent: {error}"));
    apply_migrations(&database, &root, "0.1.0", &migrations.as_slice())
        .unwrap_or_else(|error| panic!("migration failed: {error}"));
    write_inputs(&root);
    let original_config = fs::read_to_string(root.join("service.conf"))
        .unwrap_or_else(|error| panic!("config read failed: {error}"));
    fs::write(
        root.join("service.conf"),
        original_config.replace("serve_window_ms=1000\n", "serve_window_ms=10000\n"),
    )
    .unwrap_or_else(|error| panic!("serve window write failed: {error}"));
    let mut first = RestartProcess::launch(&root);
    let first_ready = first.ready(&root);
    let selected_directory = root.join("selected-project");
    fs::create_dir_all(&selected_directory)
        .unwrap_or_else(|error| panic!("selected directory: {error}"));
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .arg(&selected_directory)
        .output()
        .unwrap_or_else(|error| panic!("selected repository initialization: {error}"));
    assert!(
        initialized.status.success(),
        "selected repository initialization failed"
    );
    let selected_directory = selected_directory.to_string_lossy().replace('\\', "/");
    let input = serde_json::json!({"id":null,"location":{"directory":selected_directory},"model":{"providerID":"openai","id":"gpt-5.6"}}).to_string();
    let session = restart_request(&first_ready, "POST", "/api/session", &input);
    assert_eq!(session["data"]["location"]["directory"], selected_directory);
    assert_eq!(
        session["data"]["model"],
        serde_json::json!({"providerID":"openai","id":"gpt-5.6"})
    );
    assert!(
        session["data"].get("agent").is_none(),
        "an unselected agent was fabricated"
    );
    let session_id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session identity absent: {session}"));
    let path = format!("/api/session/{session_id}");
    restart_request(
        &first_ready,
        "POST",
        &format!("{path}/agent"),
        r#"{"agent":"agt_live_headless1"}"#,
    );
    restart_request(
        &first_ready,
        "POST",
        &format!("{path}/model"),
        r#"{"model":{"providerID":"openai","id":"gpt-5.6","variant":"default"}}"#,
    );
    let original = restart_request(&first_ready, "GET", &path, "");
    assert_eq!(original["data"]["agent"], "agt_live_headless1");
    assert_eq!(
        original["data"]["model"], session["data"]["model"],
        "equivalent default model selection must be idempotent"
    );
    let capability_path = format!("{path}/stateengine/capability");
    restart_request(
        &first_ready,
        "PUT",
        &capability_path,
        r#"{"capability":"process-private-capability-witness"}"#,
    );
    let entry_path = format!("{path}/instructions/entries/stateengine-identity");
    let context = serde_json::json!({"scope":"project", "packet":"c".repeat(32_768)});
    restart_request(
        &first_ready,
        "PUT",
        &entry_path,
        &serde_json::json!({"value":context}).to_string(),
    );
    let entries_path = format!("{path}/instructions/entries");
    let entries = restart_request(&first_ready, "GET", &entries_path, "");
    assert_eq!(
        entries,
        serde_json::json!({"data":[{"key":"stateengine-identity", "value":context}]})
    );
    let second = restart_request(&first_ready, "POST", "/api/session", &input);
    let mut permission_query = url::form_urlencoded::Serializer::new(String::new());
    permission_query.append_pair("location[directory]", &selected_directory);
    let permission_page = restart_request(
        &first_ready,
        "GET",
        &format!("/api/permission/request?{}", permission_query.finish()),
        "",
    );
    assert_eq!(permission_page["location"]["directory"], selected_directory);
    assert_eq!(permission_page["data"], serde_json::json!([]));
    assert_eq!(permission_page["hasMore"], false);
    let mut shell_query = url::form_urlencoded::Serializer::new(String::new());
    shell_query.append_pair("location[directory]", &selected_directory);
    let shell_page = restart_request(
        &first_ready,
        "GET",
        &format!("/api/shell?{}", shell_query.finish()),
        "",
    );
    assert_eq!(shell_page["location"]["directory"], selected_directory);
    assert_eq!(shell_page["data"], serde_json::json!([]));
    let sid = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session id absent"));
    assert_eq!(
        restart_request(&first_ready, "GET", &format!("/api/session/{sid}/form"), "")["data"],
        serde_json::json!([])
    );
    assert_eq!(
        restart_request(
            &first_ready,
            "GET",
            &format!("/api/session/{sid}/pending"),
            ""
        )["data"],
        serde_json::json!([])
    );
    assert_eq!(
        restart_request(&first_ready, "GET", "/api/permission/saved", "")["data"],
        serde_json::json!([])
    );
    let nested = root.join("selected-project/nested");
    fs::create_dir(&nested).unwrap_or_else(|error| panic!("nested location: {error}"));
    let foreign_input =
        serde_json::json!({"location":{"directory":nested.to_string_lossy().replace('\\', "/")}})
            .to_string();
    let foreign = restart_request(&first_ready, "POST", "/api/session", &foreign_input);
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("directory", &selected_directory)
        .append_pair("parentID", "null")
        .append_pair("order", "asc")
        .append_pair("limit", "1");
    let first_page = restart_request(
        &first_ready,
        "GET",
        &format!("/api/session?{}", query.finish()),
        "",
    );
    assert_eq!(first_page["data"].as_array().map(Vec::len), Some(1));
    assert_eq!(first_page["data"][0]["id"], session["data"]["id"]);
    let mut cursor_query = url::form_urlencoded::Serializer::new(String::new());
    cursor_query
        .append_pair(
            "cursor",
            first_page["cursor"]["next"]
                .as_str()
                .unwrap_or_else(|| panic!("next cursor absent")),
        )
        .append_pair("limit", "1");
    let next_page = restart_request(
        &first_ready,
        "GET",
        &format!("/api/session?{}", cursor_query.finish()),
        "",
    );
    assert_eq!(next_page["data"].as_array().map(Vec::len), Some(1));
    assert_eq!(next_page["data"][0]["id"], second["data"]["id"]);
    assert_ne!(next_page["data"][0]["id"], foreign["data"]["id"]);
    let mut previous_query = url::form_urlencoded::Serializer::new(String::new());
    previous_query
        .append_pair(
            "cursor",
            next_page["cursor"]["previous"]
                .as_str()
                .unwrap_or_else(|| panic!("previous cursor absent")),
        )
        .append_pair("limit", "1");
    let previous = restart_request(
        &first_ready,
        "GET",
        &format!("/api/session?{}", previous_query.finish()),
        "",
    );
    assert_eq!(previous["data"], first_page["data"]);
    let first_registration = fs::read(root.join("registration"))
        .unwrap_or_else(|error| panic!("first registration absent: {error}"));
    let pending_path = format!("/api/session/{sid}/pending");
    let pending_text = "queued\tprose\nremains pending across restart";
    let admitted = restart_request(&first_ready, "POST", &format!("/api/session/{sid}/prompt"),
        &serde_json::json!({"text":pending_text,"files":[],"agents":[],"resume":false,"delivery":"queue"}).to_string());
    // Successful human admission now gives an unnamed Session its durable
    // first-prompt title. Restart must preserve that admitted truth.
    let admitted_session = restart_request(&first_ready, "GET", &path, "");
    assert_eq!(admitted_session["data"]["title"], "queued prose");
    assert_eq!(original["data"]["title"], "New session");
    let pending_page = restart_request(&first_ready, "GET", &pending_path, "");
    assert_eq!(pending_page["data"].as_array().map(Vec::len), Some(1));
    assert_eq!(pending_page["data"][0]["id"], admitted["data"]["id"]);
    assert_eq!(pending_page["data"][0]["data"]["text"], pending_text);
    assert_eq!(pending_page["data"][0]["delivery"], "queue");
    assert_eq!(pending_page["data"][0]["sessionID"], sid);
    let first_receipt = fs::read(root.join("serve-ready.json"))
        .unwrap_or_else(|error| panic!("first receipt absent: {error}"));
    // A second process must not alter either of the serving owner's files.
    let inspection = |instance: &str| {
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .args(["startup", "--service"])
            .arg(&root)
            .args([
                "--slot",
                "product",
                "--environment",
                "internal-alpha",
                "--instance",
                instance,
            ])
            .output()
            .unwrap_or_else(|error| panic!("startup inspection failed: {error}"))
    };
    let live_inspection = inspection("instance-a");
    assert!(
        !live_inspection.status.success(),
        "inspection admitted a live predecessor"
    );
    let mut competitor = RestartProcess::launch(&root);
    assert!(!competitor.exit().success(), "live owner was replaced");
    assert_eq!(
        fs::read(root.join("registration")).unwrap_or_else(|error| panic!("{error}")),
        first_registration
    );
    assert_eq!(
        fs::read(root.join("serve-ready.json")).unwrap_or_else(|error| panic!("{error}")),
        first_receipt
    );
    first.kill_and_reap();
    for file in [&database, &root.join("database.sqlite3-wal")] {
        if file.exists() {
            let bytes = fs::read(file)
                .unwrap_or_else(|error| panic!("capability persistence witness: {error}"));
            assert!(
                !bytes
                    .windows(b"process-private-capability-witness".len())
                    .any(|window| window == b"process-private-capability-witness"),
                "capability leaked to durable storage"
            );
        }
    }
    assert!(
        root.join("registration").is_file(),
        "crash did not leave registration evidence"
    );
    assert!(
        root.join("serve-ready.json").is_file(),
        "crash did not leave ready evidence"
    );

    for generation in 1..=3 {
        let prior_registration =
            fs::read(root.join("registration")).unwrap_or_else(|error| panic!("{error}"));
        let prior_receipt =
            fs::read(root.join("serve-ready.json")).unwrap_or_else(|error| panic!("{error}"));
        let mut foreign: serde_json::Value =
            serde_json::from_slice(&prior_receipt).unwrap_or_else(|error| panic!("{error}"));
        foreign["service_generation"] = serde_json::json!(generation + 50);
        fs::write(root.join("serve-ready.json"), foreign.to_string())
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            !inspection("instance-a").status.success(),
            "inspection accepted mismatched predecessor custody"
        );
        fs::write(root.join("serve-ready.json"), &prior_receipt)
            .unwrap_or_else(|error| panic!("{error}"));
        let inspected = inspection("instance-a");
        assert!(
            inspected.status.success(),
            "dead predecessor inspection refused: {}",
            String::from_utf8_lossy(&inspected.stdout)
        );
        let advice: serde_json::Value = serde_json::from_slice(&inspected.stdout)
            .unwrap_or_else(|error| panic!("invalid advice: {error}"));
        assert_eq!(
            advice["generation"], generation,
            "startup advice reset recovery generation"
        );
        assert_eq!(
            advice["instance"],
            format!("instance-a-restart-{generation}"),
            "startup advice lost recovery instance"
        );
        assert_eq!(
            fs::read(root.join("registration")).unwrap_or_else(|error| panic!("{error}")),
            prior_registration,
            "inspection mutated registration"
        );
        assert_eq!(
            fs::read(root.join("serve-ready.json")).unwrap_or_else(|error| panic!("{error}")),
            prior_receipt,
            "inspection retired predecessor custody"
        );
        if generation == 3 {
            fs::write(root.join("service.conf"), &original_config)
                .unwrap_or_else(|error| panic!("final serve window write failed: {error}"));
        }
        let mut successor = RestartProcess::launch(&root);
        let ready = successor.ready(&root);
        assert_eq!(
            restart_request(&ready, "GET", &pending_path, ""),
            pending_page,
            "restart altered durable pending inputs"
        );
        assert_eq!(
            restart_request(&ready, "GET", &entries_path, ""),
            entries,
            "process loss changed durable instruction context"
        );
        assert_eq!(ready["service_generation"], generation);
        assert_eq!(
            ready["service_instance"],
            format!("instance-a-restart-{generation}")
        );
        assert_eq!(
            restart_request(&ready, "GET", &path, ""),
            admitted_session,
            "restart changed durable Session truth"
        );
        if generation < 3 {
            successor.kill_and_reap();
        } else {
            assert!(
                successor.exit().success(),
                "final successor failed clean shutdown: {}",
                successor.stdout()
            );
            assert!(!root.join("registration").exists());
            assert!(!root.join("serve-ready.json").exists());
        }
    }
    drop(competitor);
    drop(first);
    fs::remove_dir_all(root)
        .unwrap_or_else(|error| panic!("restart fixture cleanup failed: {error}"));
}

#[test]
fn shell_route_returns_an_empty_scoped_list_without_shell_tables() {
    let root = root("shell-empty");
    reset(&root);
    let database = root.join("database.sqlite3");
    drop(
        EventStore::open(&database, "shell-route-test")
            .unwrap_or_else(|error| panic!("store creation: {error}")),
    );
    let migrations =
        ProductionMigrations::load().unwrap_or_else(|error| panic!("migration load: {error}"));
    apply_migrations(&database, &root, "0.1.0", &migrations.as_slice())
        .unwrap_or_else(|error| panic!("migration: {error}"));
    write_inputs(&root);
    let config = fs::read_to_string(root.join("service.conf"))
        .unwrap_or_else(|error| panic!("config read: {error}"));
    fs::write(
        root.join("service.conf"),
        config.replace("serve_window_ms=1000\n", "serve_window_ms=10000\n"),
    )
    .unwrap_or_else(|error| panic!("config write: {error}"));
    let mut process = RestartProcess::launch(&root);
    let ready = process.ready(&root);
    let connection = rusqlite::Connection::open(&database)
        .unwrap_or_else(|error| panic!("fixture connection: {error}"));
    connection.execute_batch(
        "DROP TABLE product_shell_binding; DROP TABLE shell_operation; DROP TABLE shell_artifact;",
    ).unwrap_or_else(|error| panic!("remove unused inventory: {error}"));
    let directory = root
        .canonicalize()
        .unwrap_or_else(|error| panic!("directory: {error}"))
        .to_string_lossy()
        .replace('\\', "/");
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("location[directory]", &directory);
    let page = restart_request(&ready, "GET", &format!("/api/shell?{}", query.finish()), "");
    let _: grapt_api::u3a::ShellListResponseDto = serde_json::from_value(page.clone())
        .unwrap_or_else(|error| panic!("shell wire response: {error}"));
    assert_eq!(page["location"]["directory"], directory);
    assert_eq!(page["data"], serde_json::json!([]));
    process.kill_and_reap();
    drop(connection);
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

//! The real DOGFOOD-1 process journey.
//!
//! # What review held on, and what this is
//!
//! > "The DOGFOOD-1 real-process seam is absent. The only `Connection`
//! > implementation is for `ContractDouble` ... Add the byte-preserving real
//! > transport and a real DOGFOOD-1 process witness; do not claim the generic
//! > type parameter alone as the seam."
//!
//! So this spawns the SHIPPED `grapt-server` binary as a separate operating
//! system process, discovers the endpoint it publishes while live, attaches
//! [`grapt_client::HttpConnection`] to it over a real TCP socket with a real HTTP
//! Basic credential, and drives the essential journey through the ORDINARY
//! `Client` — the same `Client` every double-backed witness in this crate
//! drives. Nothing here is a shim: if the client and the real server disagreed
//! about a single byte of the U3a surface, this file is where it surfaces.
//!
//! # Why the client crate can prepare a service root at all
//!
//! `grapt-client` must not depend on `grapt-store`, and its dependency gate
//! covers `dev-dependencies` too, so the obvious approach — create and migrate
//! the database the way `grapt-product`'s own witness does — is closed to this
//! crate by the architecture it exists to protect.
//!
//! The product creates and serves its current store under `initialize` from
//! plain configuration files. Preparation checks the specific served receipt,
//! registration release and database file before selecting `open-ready` for
//! the journey. Configuration faults cannot be mistaken for initialization.
//!
//! # Locating the binary
//!
//! `CARGO_BIN_EXE_*` is only set for tests in the package that declares the
//! binary, so this crate cannot use it. The binary is built on demand through
//! `$CARGO` and then located next to this test executable, which respects a
//! custom `CARGO_TARGET_DIR` instead of guessing at `target/debug`.
//!
//! If it cannot be built or found, this file PANICS naming the reason. It never
//! skips: an evidence-bearing test that quietly passes when its evidence is
//! missing is the exact shape the honest-failure law forbids, and a green
//! "real-process journey" that never started a process would be worse than no
//! witness at all.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[path = "dogfood_journey/bootstrap.rs"]
mod bootstrap;
#[path = "dogfood_journey/terminal_lifetime.rs"]
mod terminal_lifetime;
use bootstrap::bootstrap_store;

use catalog::{isolate_catalog, CATALOG_HOME};
use grapt_client::bounds::Bounds;
use grapt_client::client::{Client, ClientError, Connection};
use grapt_client::contract::Operation;
use grapt_client::cursor::StreamState;
use grapt_client::http::{BasicCredential, HttpConnection};
use grapt_client::projection::SliceKind;
use grapt_client::transport::{Transport, TransportError, WireRequest};
use grapt_client::wire::Spelled;
use offer::select_offer;

/// The credential the service root publishes and the client presents.
const USERNAME: &str = "alpha";
const SECRET: &str = "zephyr";

/// How long the spawned product serves before retiring itself.
///
/// A bound rather than a lifetime: if this witness panics or is killed, the
/// product still exits on its own and leaves no orphan holding a port.
const SERVE_WINDOW_MS: u64 = 30_000;

/// Polling budgets. No ambient clock is read anywhere in this file — every wait
/// is a bounded count of fixed sleeps, which is also what makes the failure
/// message able to say how long it actually waited.
const READY_POLLS: usize = 600;
const READY_SLEEP: Duration = Duration::from_millis(25);
const JOURNEY_POLLS: usize = 400;
const JOURNEY_SLEEP: Duration = Duration::from_millis(25);
const PROVIDER_REQUEST_BYTES: usize = 64 * 1_024;

// ---------------------------------------------------------------------------
// Process lifecycle
// ---------------------------------------------------------------------------

/// A spawned product that kills itself when the witness ends.
///
/// `Drop` rather than an explicit teardown call, because every assertion below
/// can panic and a panic must not leave a live server holding a port and a
/// service root. This is the only reason the struct exists.
struct Product {
    child: Child,
    root: PathBuf,
    /// The repository its journeys are launched in.
    launch: PathBuf,
    _provider: ProviderFixture,
}

impl Product {
    fn diagnose(&self) {
        let mut pressure = Vec::new();
        let observed = fs::File::open("/proc/pressure/io")
            .and_then(|file| file.take(512).read_to_end(&mut pressure));
        eprintln!(
            "owned fixture IO pressure: {observed:?}: {}",
            String::from_utf8_lossy(&pressure)
        );
        eprintln!(
            "TUI provider thread finished={}",
            self._provider
                .thread
                .as_ref()
                .is_some_and(|thread| thread.is_finished())
        );
        let root = PathBuf::from(format!("/proc/{}/task", self.child.id()));
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("TUI product threads unavailable: {error}");
                return;
            }
        };
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for entry in entries.take(256) {
            let observation = match entry {
                Ok(entry) => {
                    let mut bytes = Vec::new();
                    match fs::File::open(entry.path().join("wchan"))
                        .and_then(|file| file.take(256).read_to_end(&mut bytes))
                    {
                        Ok(_) => String::from_utf8_lossy(&bytes).trim().to_owned(),
                        Err(error) => format!("unavailable:{error}"),
                    }
                }
                Err(error) => format!("unavailable:{error}"),
            };
            *counts.entry(observation).or_default() += 1;
        }
        eprintln!("TUI product thread waits, at most 256: {counts:?}");
    }
}

impl Drop for Product {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.diagnose();
        }
        let prior_status = self.child.try_wait();
        let _ = self.child.kill();
        let reaped = self.child.wait();
        if std::thread::panicking() {
            eprintln!("TUI product failure before cleanup={prior_status:?}; reaped={reaped:?}");
        }
        if std::thread::panicking() {
            for name in ["product-stdout.log", "product-stderr.log"] {
                let mut bytes = Vec::new();
                let result = fs::File::open(self.root.join(name))
                    .and_then(|file| file.take(65_536).read_to_end(&mut bytes));
                eprintln!(
                    "TUI product {name} {result:?}: {}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.launch);
    }
}

/// A loopback-only provider that echoes the exact user content carried by the
/// live product request. The client still talks only to the shipped product;
/// this second socket replaces the provider dependency the former
/// deterministic executor kept in-process.
struct ProviderFixture {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ProviderFixture {
    fn start(respond: fn(&mut TcpStream, &str)) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|error| panic!("provider loopback bind failed: {error}"));
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|error| panic!("provider nonblocking mode failed: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("provider address failed: {error}"));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _peer)) => {
                        let body = read_provider_body(&mut stream);
                        let prompt = provider_prompt(&body);
                        respond(&mut stream, &prompt);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("provider accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(handle),
        }
    }
}

impl Drop for ProviderFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let Some(handle) = self.thread.take() else {
            return;
        };
        if handle.join().is_ok() {
            return;
        }
        // The fixture thread panicked, and its own message has already been
        // printed by the default hook. Whether this is allowed to panic depends
        // entirely on whether the test is ALREADY unwinding.
        //
        // Panicking here during an unwind aborts the process: the run died with
        // STATUS_STACK_BUFFER_OVERRUN and no output at all, because libtest's
        // captured buffer -- carrying the ORIGINAL failure and the summary line
        // -- is never flushed on abort. A fixture that destroys the report of
        // the defect it is reporting is worse than useless, so when the test is
        // already failing this says so and lets the first panic stand as the
        // diagnosis it is.
        if std::thread::panicking() {
            eprintln!(
                "TUI provider fixture thread panicked while the test was already failing; \
                 the failure above is the one that matters"
            );
            return;
        }
        panic!("provider fixture panicked");
    }
}

fn read_provider_body(stream: &mut TcpStream) -> Vec<u8> {
    // A stream accepted from a NON-BLOCKING listener inherits that mode on
    // Windows, and a non-blocking socket ignores a read timeout: the read
    // returns WouldBlock (os error 10035) the instant the request head has not
    // arrived yet, which is a race this fixture loses intermittently. The
    // listener must stay non-blocking so the accept loop can observe `stop`,
    // but this stream is read to completion right here, so it is put back into
    // blocking mode and the timeout below becomes the real bound it claims to
    // be. Without this the fixture thread panicked on a perfectly ordinary
    // scheduling outcome.
    stream
        .set_nonblocking(false)
        .unwrap_or_else(|error| panic!("provider blocking mode failed: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap_or_else(|error| panic!("provider request timeout failed: {error}"));
    let mut bytes = Vec::with_capacity(4_096);
    let mut chunk = [0_u8; 4_096];
    loop {
        let count = stream
            .read(&mut chunk)
            .unwrap_or_else(|error| panic!("provider request read failed: {error}"));
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > PROVIDER_REQUEST_BYTES {
            panic!("provider request exceeded {PROVIDER_REQUEST_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk[..count]);
        let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&bytes[..split]);
        let length = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|raw| raw.parse::<usize>().ok())
            })
            .unwrap_or_else(|| panic!("provider request omitted content-length"));
        if length > PROVIDER_REQUEST_BYTES {
            panic!("provider body exceeded {PROVIDER_REQUEST_BYTES} bytes");
        }
        let body_start = split + 4;
        let body_end = body_start
            .checked_add(length)
            .unwrap_or_else(|| panic!("provider body bound overflowed"));
        if bytes.len() >= body_end {
            return bytes[body_start..body_end].to_vec();
        }
    }
    panic!("provider request ended before its bounded body")
}

/// The scripted provider answers the CURRENT user turn: a continuation
/// request carries the whole conversation, and the first user message is the
/// stale history, not the prompt the provider is answering.
fn provider_prompt(body: &[u8]) -> String {
    let value: serde_json::Value = serde_json::from_slice(body)
        .unwrap_or_else(|error| panic!("provider request JSON failed: {error}"));
    value
        .get("input")
        .and_then(serde_json::Value::as_array)
        .and_then(|input| input.iter().rev().find(|message| message["role"] == "user"))
        .and_then(|message| message.get("content"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("provider request carried no user content"))
}

#[test]
fn provider_fixture_selects_user_content_after_system_instructions() {
    assert_eq!(
        provider_prompt(
            br#"{"input":[{"role":"system","content":"instructions"},{"role":"user","content":"submitted prompt"}]}"#,
        ),
        "submitted prompt"
    );
    assert_eq!(
        provider_prompt(
            br#"{"input":[{"role":"system","content":"instructions"},{"role":"user","content":"first turn"},{"role":"assistant","content":"first reply"},{"role":"user","content":"current turn"}]}"#,
        ),
        "current turn",
        "a continuation answers the current user turn, not the first one"
    );
}

#[test]
#[should_panic(expected = "provider request carried no user content")]
fn provider_fixture_refuses_absent_user_content() {
    provider_prompt(br#"{"input":[{"role":"system","content":"instructions"}]}"#);
}

fn write_provider_success(stream: &mut TcpStream, prompt: &str) {
    let delta = serde_json::to_string(prompt)
        .unwrap_or_else(|error| panic!("provider delta encoding failed: {error}"));
    let body = format!(
        "data: {{\"type\":\"response.output_text.delta\",\"delta\":{delta}}}\n\n\
         data: {{\"type\":\"response.output_text.done\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":11,\"output_tokens\":4,\"total_tokens\":15}}}}}}\n\n"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .unwrap_or_else(|error| panic!("provider response failed: {error}"));
}

fn unique_root(label: &str) -> PathBuf {
    // Keyed by process id AND label so two witnesses in one run cannot collide
    // on a service root, which would look like a custody refusal from the
    // product rather than a defect in this file.
    std::env::temp_dir().join(format!("grapt-tui-dogfood-{}-{label}", std::process::id()))
}

/// The platform config, parameterized by store authority.
fn platform_config(authority: &str) -> String {
    format!(
        r#"{{
          "store": {{ "authority": "{authority}" }},
          "product": {{ "build": "tui-dogfood-build" }},
          "provider": {{
            "id": "openai", "model": "dogfood-1", "base_model": "dogfood-1",
            "family": "dogfood", "route": "builtin", "account": "account-a",
            "credential_version": "credential-v1", "policy_version": "dogfood-policy-v1",
            "variants": []
          }},
          "catalog": {{ "source": "models.dev-chatgpt" }}
        }}"#
    )
}

/// Write a complete service root. File writes only — no store dependency.
fn prepare(root: &Path) {
    if root.exists() {
        fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("{} reset failed: {error}", root.display()));
    }
    fs::create_dir_all(root.join(CATALOG_HOME))
        .unwrap_or_else(|error| panic!("{} creation failed: {error}", root.display()));

    // Project discovery must stop at this fixture, never at an ambient ancestor.
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .arg(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("fixture Git boundary failed: {error}"));
    assert!(initialized.success(), "fixture Git boundary was refused");

    let service = format!(
        "slot=product\ninstance=tui-dogfood\nenvironment=internal-alpha\nversion=0.1.0\nserve_window_ms={SERVE_WINDOW_MS}\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=4\n"
    );
    fs::write(root.join("service.conf"), service)
        .unwrap_or_else(|error| panic!("service config failed: {error}"));
    fs::write(
        root.join("basic.credential"),
        format!("{USERNAME}:{SECRET}"),
    )
    .unwrap_or_else(|error| panic!("basic credential failed: {error}"));
    fs::write(root.join("provider.credential"), "provider-secret")
        .unwrap_or_else(|error| panic!("provider credential failed: {error}"));
    // The scripted provider declares capacity through the ordinary inventory;
    // production must not infer a context budget from its fixture model name.
    fs::write(
        root.join("model-catalog.json"),
        r#"{"openai":{"id":"openai","name":"Scripted fixture","env":[],
          "api":"https://127.0.0.1","doc":"fixture","models":{"dogfood-1":{
            "id":"dogfood-1","name":"Scripted fixture","family":"dogfood",
            "attachment":false,"reasoning":false,"tool_call":true,
            "structured_output":false,"temperature":false,
            "modalities":{"input":["text"],"output":["text"]},
            "limit":{"context":131072,"output":16384}}}}}"#,
    )
    .unwrap_or_else(|error| panic!("fixture model catalog failed: {error}"));
    fs::write(
        root.join("account-models.json"),
        r#"{"models":[{"slug":"dogfood-1","context_window":131072,"effective_context_window_percent":95,"supported_reasoning_levels":[]}]}"#,
    )
    .unwrap_or_else(|error| panic!("fixture model discovery failed: {error}"));
    // `initialize`, not `open-ready`: there is deliberately no database here.
    // The product creates its own store on first contact, which is what lets a
    // crate that may not depend on `grapt-store` prepare this root at all.
    fs::write(root.join("grapt.jsonc"), platform_config("initialize"))
        .unwrap_or_else(|error| panic!("platform config failed: {error}"));
}

fn test_profile_dir() -> PathBuf {
    let current = std::env::current_exe()
        .unwrap_or_else(|error| panic!("test executable path must be readable: {error}"));
    current
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("test executable must sit under a profile directory"))
        .to_path_buf()
}

/// Build and locate the shipped product binary.
fn product_binary() -> PathBuf {
    // Cross-compiled native witnesses are staged with the candidate product;
    // ordinary local runs still build it here and refuse an older binary.
    if let Some(staged) = std::env::var_os("GRAPT_TUI_DOGFOOD_PRODUCT") {
        let path = PathBuf::from(staged);
        assert!(
            path.is_file(),
            "staged candidate product is absent: {}",
            path.display()
        );
        return path;
    }
    let name = if cfg!(windows) {
        "grapt-server.exe"
    } else {
        "grapt-server"
    };
    // Keep the journey's build beneath this test executable's profile so a
    // custom target directory also owns its artifacts.
    let target = test_profile_dir().join("tui-journey-product");
    let binary = target.join("debug").join(name);

    // A narrower feature graph can replace dependencies still needed by the
    // parent Cargo run's doctests. Build the current product in its own target.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("Cargo.toml");
    let status = Command::new(&cargo)
        .arg("build")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .env("CARGO_TARGET_DIR", &target)
        .arg("-p")
        .arg("grapt-product")
        .arg("--bin")
        .arg("grapt-server")
        .arg("--features")
        .arg("loopback-provider-testkit")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match status {
        Ok(output) if output.status.success() => {}
        Ok(output) => panic!(
            "`{cargo} build -p grapt-product --bin grapt-server` failed; \
             an older binary cannot witness this candidate:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => panic!(
            "could not run `{cargo} build` ({error}); an older binary cannot witness this candidate"
        ),
    }

    assert!(
        binary.exists(),
        "the product binary is absent at {}; this witness cannot run without a real \
         process and will not pretend it did",
        binary.display()
    );
    binary
}

/// Spawn the product and wait for the endpoint it publishes while live.
fn serve(label: &str) -> (Product, SocketAddr) {
    serve_catalog(label, true)
}

/// `serve` with the product's catalog home present or absent.
fn serve_catalog(label: &str, catalog_home: bool) -> (Product, SocketAddr) {
    serve_with_provider(label, catalog_home, write_provider_success)
}

fn serve_with_provider(
    label: &str,
    catalog_home: bool,
    respond: fn(&mut TcpStream, &str),
) -> (Product, SocketAddr) {
    serve_configured(label, catalog_home, respond, |_| {})
}

fn serve_configured(
    label: &str,
    catalog_home: bool,
    respond: fn(&mut TcpStream, &str),
    configure: fn(&Path),
) -> (Product, SocketAddr) {
    let root = unique_root(label);
    prepare(&root);
    configure(&root);
    let launch = offer::launch_repository(&root);
    let binary = product_binary();
    bootstrap_store(&binary, &root);
    let provider = ProviderFixture::start(respond);
    // Multi-step witnesses must not block the product on an unread pipe.
    // Finite process/request budgets bound these logs; failures print a bounded prefix.
    let child = isolate_catalog(&mut Command::new(&binary), &root, catalog_home)
        .arg("serve")
        .arg("--service")
        .arg(&root)
        .arg("--port")
        .arg("0")
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        )
        .stdout(Stdio::from(
            fs::File::create(root.join("product-stdout.log"))
                .unwrap_or_else(|e| panic!("product stdout capture: {e}")),
        ))
        .stderr(Stdio::from(
            fs::File::create(root.join("product-stderr.log"))
                .unwrap_or_else(|e| panic!("product stderr capture: {e}")),
        ))
        .spawn()
        .unwrap_or_else(|error| panic!("product spawn failed: {error}"));
    let mut product = Product {
        child,
        launch,
        root,
        _provider: provider,
    };

    // The endpoint is discovered from the readiness receipt the product writes
    // while it is LIVE, not from its stdout — the launch receipt is printed
    // when the serve window ENDS, so a client that waited for stdout would
    // learn the address only after the server had gone.
    let receipt_path = product.root.join("serve-ready.json");
    for poll in 0..READY_POLLS {
        if let Ok(bytes) = fs::read(&receipt_path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                let endpoint = value
                    .get("observed_endpoint")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                if let Some(endpoint) = endpoint {
                    let address = grapt_client::http::endpoint_address(&endpoint)
                        .unwrap_or_else(|error| panic!("published endpoint: {error}"));
                    return (product, address);
                }
            }
        }
        match product.child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                // Drop retains cached exit status and both bounded output pipes.
                panic!(
                    "product exited before readiness: {status}; seen at poll {poll} \
                     (after ~{}ms of fixed sleeps)",
                    poll as u128 * READY_SLEEP.as_millis()
                );
            }
            Err(error) => panic!("product status failed: {error}"),
        }
        thread::sleep(READY_SLEEP);
    }
    // Every poll above saw the product still running, or it would have panicked
    // there. Drop then records its pre-cleanup state, thread waits, IO pressure
    // and bounded pipes.
    panic!(
        "the product published no readiness receipt within {} polls \
         (~{}ms of fixed sleeps; the product was running at every poll)",
        READY_POLLS,
        READY_POLLS as u128 * READY_SLEEP.as_millis()
    )
}

fn credential() -> BasicCredential {
    BasicCredential::new(USERNAME, SECRET)
        .unwrap_or_else(|error| panic!("witness credential must construct: {error}"))
}

/// A session selected before its stream proved positions is re-read as a
/// snapshot once that proof arrives. Settle that re-read before a witness
/// counts reloads, so the launch read can't stand in for, or race, the one
/// the witness measures.
fn prove_positions<C: Connection>(client: &mut Client<C>) {
    let unproven = client.diagnostics().reconciliations;
    for _ in 0..JOURNEY_POLLS {
        client
            .pump()
            .unwrap_or_else(|error| panic!("handshake pump: {error}"));
        if client.diagnostics().reconciliations > unproven {
            return;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    panic!("the positions proof must replace the unproven page");
}

fn connect(address: SocketAddr, bounds: Bounds) -> Client<HttpConnection> {
    Client::new(
        HttpConnection::attach(address, credential(), bounds),
        bounds,
    )
}

// ---------------------------------------------------------------------------
// The journey
// ---------------------------------------------------------------------------

#[test]
fn the_real_product_serves_the_project_agent_and_model_selection() {
    // J01's selection half, against a REAL process over a real socket.
    //
    // This is the assertion the contract double cannot make. The double agrees
    // with this client by construction — both were written here — so a client
    // that spelled `location[directory]` wrongly, sent a bare model id where
    // the contract wants a `Model.Ref`, or read `data` where the frozen record
    // says the response IS the array would pass every in-crate witness and fail
    // the first time it met the product. Each of those is a 400 or a decode
    // refusal here.
    let (mut product, address) = serve("selection");
    let mut client = connect(address, Bounds::PRODUCTION);

    let (directory, agent, model) = select_offer(&mut client, &product);
    assert_eq!(
        Path::new(&directory),
        product.launch,
        "the gateway leads with the repository it was launched in"
    );
    assert!(
        !directory.is_empty(),
        "the real gateway names a worktree the catalog routes can be scoped by"
    );
    assert!(
        !agent.is_empty() && !model.is_empty(),
        "the real roster and picker name identities the domain parses"
    );
    assert!(
        client.ready_to_chat(),
        "all three selections are made against the real product"
    );

    // The catalog was read AT the chosen location. A client that ignored the
    // location query would still get rows — the product has a current
    // directory — so the assertion that matters is that the rows and the
    // location agree.
    let Some(chosen) = client.project() else {
        panic!("a project is selected")
    };
    assert_eq!(chosen.worktree, directory);

    // The default is READ and reported, and it is not silently adopted as the
    // user's choice. A client that adopted it would satisfy `ready_to_chat`
    // without the user ever opening the picker.
    if let Some(default) = client.default_model() {
        assert!(
            !default.name.is_empty(),
            "a declared default names a real model"
        );
    }

    // And the selection survives into a real session: the create request
    // carries agent, model and location, and the product accepts them.
    let id = client
        .create_session()
        .unwrap_or_else(|error| panic!("a selected session must be creatable: {error}"));
    assert!(id.spelled().starts_with("ses_"));

    let _ = &mut product;
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn the_real_process_serves_the_whole_essential_journey_over_a_real_socket() {
    let (mut product, address) = serve("journey");
    let mut client = connect(address, Bounds::PRODUCTION);

    // 1. AUTHENTICATED LISTING. The first real assertion is that the client's
    //    frozen route table and the server's registered routes agree — a
    //    mismatch here is a 404, and a 404 is what a whole lane of invented
    //    vocabulary looks like from the outside.
    client
        .refresh_sessions()
        .unwrap_or_else(|error| panic!("authenticated session list must succeed: {error}"));
    let before = client.sessions().len();

    // 1b. SELECT, because a session cannot be created without one. The
    //     selection journey itself is asserted in
    //     `the_real_product_serves_the_project_agent_and_model_selection`.
    let _ = select_offer(&mut client, &product);

    // 2. CREATE, through the real request DTO and the real response DTO.
    let id = client
        .create_session()
        .unwrap_or_else(|error| panic!("session creation must succeed: {error}"));
    // THE IDENTITY CAME BACK FROM A REAL SERVER AND PARSED. This is the
    // assertion the previous revision could not have made: it minted `ses1`,
    // which no `SessionId` parses, and nothing in the crate ever tried. The
    // real server mints `ses_` ids, and the client's edge parse accepts them.
    assert!(
        id.spelled().starts_with("ses_"),
        "the real server's identity must be a real SessionId: {}",
        id.spelled()
    );
    assert_eq!(
        client.sessions().len(),
        before + 1,
        "the created session must appear in the real listing"
    );
    assert_eq!(client.selected(), Some(&id));

    // 3. A COMPLETE, READABLE DURABLE PAGE EARNS LIVENESS. `create_session`
    //    selects, and selecting reconciles, so the client has already read the
    //    real `v2.message.list` by this point.
    assert_eq!(
        client.stream_state(),
        &StreamState::Live,
        "a complete readable page from the real server must earn liveness"
    );

    // 4. PROMPT. The bytes `wire::encode_prompt` produced go on the socket
    //    unchanged; the server either accepts them as a real `PromptRequestDto`
    //    or answers 400. There is no third outcome, which is what makes this a
    //    byte-preservation assertion rather than a hopeful one.
    client
        .submit_prompt("hello from the native client")
        .unwrap_or_else(|error| panic!("prompt submission must succeed: {error}"));

    // 5. THE DURABLE TRANSCRIPT CONVERGES. Execution is asynchronous on the
    //    server, so this reconciles on a bounded budget until the assistant's
    //    durable reply lands — and PANICS rather than passing if it never does.
    let mut assistant_text = None;
    for _ in 0..JOURNEY_POLLS {
        client.reconcile(&id).unwrap_or_else(|error| {
            let mut raw = HttpConnection::attach(address, credential(), Bounds::PRODUCTION);
            let request = WireRequest::new(Operation::MessageList, Some(&id), None)
                .unwrap_or_else(|wire_error| panic!("message-list route: {wire_error}"));
            let response = raw.call(&request).unwrap_or_else(|transport_error| {
                panic!("message-list transport: {transport_error}")
            });
            panic!(
                "reconciliation failed: {error}; raw status={}; bounded body={}",
                response.status,
                String::from_utf8_lossy(&response.body)
            )
        });
        if let Some(view) = client.view() {
            let text: String = view
                .slices
                .iter()
                .filter(|slice| slice.kind == SliceKind::Text)
                .map(|slice| slice.text.as_str())
                .collect();
            if !text.is_empty() {
                assistant_text = Some(text);
                break;
            }
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    let assistant_text = assistant_text.unwrap_or_else(|| {
        panic!("the real server produced no durable assistant text within the budget")
    });
    assert!(
        !assistant_text.trim().is_empty(),
        "the durable transcript must carry real content"
    );

    // Provider usage is deliberately partial in this fixture: reasoning and
    // cache coordinates are absent. The assistant DTO makes the outer `tokens`
    // field optional, so the route must stay readable and omit it rather than
    // invent observed zero or turn an honest absence into an HTTP 500.
    let mut raw = HttpConnection::attach(address, credential(), Bounds::PRODUCTION);
    let request = WireRequest::new(Operation::MessageList, Some(&id), None)
        .unwrap_or_else(|error| panic!("message-list route must construct: {error}"));
    let response = raw
        .call(&request)
        .unwrap_or_else(|error| panic!("message-list request must complete: {error}"));
    assert_eq!(
        response.status,
        200,
        "incomplete provider usage must keep message.list readable: {}",
        String::from_utf8_lossy(&response.body)
    );
    let page: serde_json::Value = serde_json::from_slice(&response.body)
        .unwrap_or_else(|error| panic!("message-list response must be JSON: {error}"));
    let assistant = page
        .get("data")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("message-list must carry the durable assistant row"));
    assert!(
        !assistant.contains_key("tokens"),
        "partial provider usage must omit optional assistant tokens"
    );

    // 6. THE PROMPT TEXT ROUND-TRIPPED, END TO END. The bounded loopback
    //    provider echoes what it was given, so the exact bytes this client
    //    encoded reappear inside DURABLE assistant content decoded back through the real
    //    `SessionMessagesResponseDto`. Encode here, accept there, execute there,
    //    re-list there, decode here — that is the byte-preservation claim
    //    closed, and it is closed by observation rather than by assertion about
    //    the transport's internals.
    assert!(
        assistant_text.contains("hello from the native client"),
        "the prompt this client encoded must reappear in the server's durable \
         assistant content, found {assistant_text:?}"
    );

    // 7. D1 restart projection now serves the admitted user row alongside the
    //    assistant row in durable order. Pin the real client path so this is
    //    evidence of the production wire contract, not merely the double.
    let view = client
        .view()
        .unwrap_or_else(|| panic!("the selected session must have a view"));
    let user_slices = view
        .slices
        .iter()
        .filter(|slice| slice.kind == SliceKind::User)
        .count();
    assert_eq!(
        user_slices, 1,
        "DOGFOOD-1 must render the one admitted user row from durable history"
    );

    // 8. THE VIEW IS OWNED BY THE TYPED IDENTITY the server minted.
    assert_eq!(view.session_id, id);

    // 9. THE NEGATIVE CONTROL FOR BYTE PRESERVATION. Acceptance only means
    //    something if refusal is possible: if the server took any body at all,
    //    step 4 would prove nothing about the bytes. So a body this client's
    //    encoder would never produce — the right shape, the wrong key — goes to
    //    the same route on the same socket and must be REFUSED.
    let mut wrong = HttpConnection::attach(address, credential(), Bounds::PRODUCTION);
    let request = WireRequest::new(Operation::SessionPrompt, Some(&id), None)
        .unwrap_or_else(|error| panic!("the prompt route must address: {error}"))
        .with_body(br#"{"prompt":"the wrong key"}"#.to_vec());
    let response = wrong
        .call(&request)
        .unwrap_or_else(|error| panic!("the malformed request must reach the server: {error}"));
    assert_eq!(
        response.status, 400,
        "a body the real encoder would never produce must be refused, or the \
         acceptance above proves nothing about the bytes"
    );

    let _ = &mut product;
}

#[test]
fn the_shipped_client_and_the_real_server_agree_on_every_journey_route() {
    // Route closure, measured against a LIVE server rather than against the
    // frozen record. `contract/tests.rs` already proves the client's table
    // matches the frozen document; this proves the SERVER serves what the
    // document says, which is a different claim and the one that was never
    // made before.
    //
    // Every route here is exercised against the live product. Permission
    // create uses the same byte contract as the TUI's list/reply path.
    let (mut product, address) = serve("routes");
    let mut client = connect(address, Bounds::PRODUCTION);
    let _ = select_offer(&mut client, &product);

    let id = client
        .create_session()
        .unwrap_or_else(|error| panic!("session creation must succeed: {error}"));

    client
        .refresh_sessions()
        .unwrap_or_else(|error| panic!("v2.session.list must be served: {error}"));
    client
        .reconcile(&id)
        .unwrap_or_else(|error| panic!("v2.message.list must be served: {error}"));
    client
        .interrupt()
        .unwrap_or_else(|error| panic!("v2.session.interrupt must be served: {error}"));

    // Exercise an explicit Ask rule; ordinary Maintainer actions run unasked.
    let body =
        grapt_client::wire::encode_permission_create("external_directory", &["/work/dogfood.txt"])
            .unwrap_or_else(|error| panic!("permission create must encode: {error}"));
    let request = WireRequest::new(Operation::SessionPermissionCreate, Some(&id), None)
        .unwrap_or_else(|error| panic!("permission create must address: {error}"))
        .with_body(body);
    let response = client
        .peer_mut()
        .call(&request)
        .unwrap_or_else(|error| panic!("permission create transport failed: {error}"));
    assert_eq!(response.status, 200, "permission create must be served");
    let permission = grapt_client::wire::decode_permission_created(&response.body)
        .unwrap_or_else(|error| panic!("permission create response must decode: {error}"));

    client
        .refresh_permissions()
        .unwrap_or_else(|error| panic!("permission list must be served: {error}"));
    assert!(client
        .permissions()
        .iter()
        .any(|ask| ask.request_id == permission));

    // A fresh HTTP connection rebuilds its view from durable truth; the ask
    // cannot depend on the original client's process-local projection.
    let mut reconnected = connect(address, Bounds::PRODUCTION);
    reconnected
        .refresh_permissions()
        .unwrap_or_else(|error| panic!("reconnected permission list must converge: {error}"));
    assert!(reconnected
        .permissions()
        .iter()
        .any(|ask| ask.request_id == permission));
    reconnected
        .reply_permission(
            &id,
            &permission,
            grapt_client::wire::PermissionReplyChoice::Once,
        )
        .unwrap_or_else(|error| panic!("permission reply must settle: {error}"));
    assert!(
        reconnected.permissions().is_empty(),
        "settled ask must leave the live list"
    );

    let _ = &mut product;
}

#[path = "dogfood_journey/catalog.rs"]
mod catalog;
#[path = "dogfood_journey/live_stream.rs"]
mod live_stream;
#[path = "dogfood_journey/offer.rs"]
mod offer;
#[path = "dogfood_journey/permission_reload.rs"]
mod permission_reload;

#[path = "dogfood_journey/compaction_label.rs"]
mod compaction_label;
#[path = "dogfood_journey/refusals.rs"]
mod refusals;

#[path = "dogfood_journey/tool_events.rs"]
mod tool_events;

#[path = "dogfood_journey/directory_placement.rs"]
mod directory_placement;

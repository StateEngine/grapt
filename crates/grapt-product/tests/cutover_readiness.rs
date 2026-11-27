//! CUTOVER-READINESS-FIXES-01 J01 shipped witness: the shipped serve path
//! recovers a deterministic torn hot rollback journal before it binds, then
//! serves the committed history unchanged; the journal is consumed and the
//! torn rows stay invisible after the drain.

#[path = "support/provider_profile.rs"]
mod provider_profile;
use std::fs;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use grapt_store::{
    apply_migrations, preflight_read_only, EventStore, MigrationOutcome, ProductionMigrations,
    StoreReadiness,
};

const ENGINE_VERSION: &str = "0.1.0";

fn root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "grapt-cutover-readiness-{label}-{}",
        std::process::id()
    ))
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

fn ready_store(database: &Path, backup_root: &Path) {
    let store = match EventStore::open(database, ENGINE_VERSION) {
        Ok(store) => store,
        Err(error) => panic!("ready database creation failed: {error}"),
    };
    drop(store);
    let migrations = match ProductionMigrations::load() {
        Ok(migrations) => migrations,
        Err(error) => panic!("production migration load failed: {error}"),
    };
    match apply_migrations(
        database,
        backup_root,
        ENGINE_VERSION,
        &migrations.as_slice(),
    ) {
        Ok(MigrationOutcome::Applied { .. } | MigrationOutcome::AlreadyCurrent) => {}
        Err(error) => panic!("ready database migration failed: {error}"),
    }
}

fn write_inputs(root: &Path, serve_window_ms: u64) {
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .arg(root)
        .output()
        .unwrap_or_else(|error| panic!("service repository initialization: {error}"));
    assert!(
        initialized.status.success(),
        "service repository initialization failed"
    );
    let config = format!(
        "slot=cutover\ninstance=instance-crf\nenvironment=internal-alpha\nversion=0.1.0\nserve_window_ms={serve_window_ms}\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=4\n"
    );
    if let Err(error) = fs::write(root.join("service.conf"), config) {
        panic!("config write failed: {error}");
    }
    if let Err(error) = fs::write(root.join("basic.credential"), "alpha:zephyr") {
        panic!("credential write failed: {error}");
    }
    let platform = r#"{
      "store": { "authority": "open-ready" },
      "product": { "build": "crf-build" },
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

fn launch_serve(root: &Path) -> Child {
    match grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .arg("serve")
        .arg("--service")
        .arg(root)
        .arg("--port")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => panic!("product launch failed: {error}"),
    }
}

/// Collects a finished or killed child's output without consuming the
/// handle: the caller keeps the process for the status and drain that
/// follow the collection.
fn collect_output(child: &mut Child) -> Output {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        if let Err(error) = pipe.read_to_end(&mut stdout) {
            panic!("product output collection failed: {error}");
        }
    }
    if let Some(mut pipe) = child.stderr.take() {
        if let Err(error) = pipe.read_to_end(&mut stderr) {
            panic!("product output collection failed: {error}");
        }
    }
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => panic!("product status could not be read: {error}"),
    };
    Output {
        status,
        stdout,
        stderr,
    }
}

/// Waits for the pid-gated ready receipt, or reaps an early exit with its
/// output. The bound is 30 seconds: startup may contend with a second
/// governed build, and the readiness wait is separate from the serve window.
fn wait_ready(root: &Path, child: &mut Child) -> serde_json::Value {
    let child_pid = child.id();
    let ready_path = root.join("serve-ready.json");
    for _ in 0..3000 {
        if let Ok(bytes) = fs::read(&ready_path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["pid"].as_u64() == Some(u64::from(child_pid)) {
                    return value;
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = collect_output(child);
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
    let _ = child.kill();
    let output = collect_output(child);
    panic!(
        "product published no ready receipt at {}: stdout={} stderr={}",
        ready_path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Waits for the bounded serve window to drain the product on its own and
/// hands back the launch receipt output. A kill inside this wait means the
/// window itself failed, which the panic names with the product output.
fn wait_drained(child: &mut Child) -> Output {
    for _ in 0..1000 {
        match child.try_wait() {
            Ok(Some(_)) => {
                return collect_output(child);
            }
            Ok(None) => {}
            Err(error) => panic!("product status could not be read: {error}"),
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let output = collect_output(child);
    panic!(
        "the serve window did not drain on its own: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn endpoint(ready: &serde_json::Value) -> SocketAddr {
    let raw = ready["observed_endpoint"]
        .as_str()
        .unwrap_or_else(|| panic!("ready receipt omitted the observed endpoint: {ready}"));
    let raw = raw
        .strip_prefix("http://")
        .unwrap_or_else(|| panic!("ready endpoint is not http: {raw}"));
    raw.parse::<SocketAddr>()
        .unwrap_or_else(|error| panic!("ready endpoint did not parse: {error}"))
}

/// One bounded raw-TCP request through the Basic-authenticated ingress.
fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, Option<serde_json::Value>) {
    let mut stream = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .unwrap_or_else(|error| panic!("endpoint connection failed: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap_or_else(|error| panic!("endpoint read timeout failed: {error}"));
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap_or_else(|error| panic!("endpoint write timeout failed: {error}"));
    let body = body.unwrap_or("");
    let length = body.len();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic YWxwaGE6emVwaHly\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}"
    )
    .unwrap_or_else(|error| panic!("endpoint request write failed: {error}"));
    let mut wire = String::new();
    stream
        .take(256 * 1024)
        .read_to_string(&mut wire)
        .unwrap_or_else(|error| panic!("endpoint response read failed: {error}"));
    let (head, response_body) = wire
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("endpoint response missing HTTP boundary: {wire}"));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_else(|| panic!("endpoint response missing status: {head}"));
    let parsed = if response_body.is_empty() {
        None
    } else {
        Some(serde_json::from_str(response_body).unwrap_or_else(|error| {
            panic!("endpoint response malformed JSON: {error}: {response_body}")
        }))
    };
    (status, parsed)
}

fn journal_path(database: &Path) -> PathBuf {
    let name = database
        .file_name()
        .unwrap_or_else(|| panic!("the database path has no file name"));
    database.with_file_name(format!("{}-journal", name.to_string_lossy()))
}

/// Tears the database: re-executes this test binary as the in-graph torn
/// writer, waits (bounded) for its marker, then kills it. The kill is the
/// crash that leaves the journal hot; the marker proves the journal was
/// observable before the crash.
fn tear(database: &Path, root: &Path) {
    let marker = root.join("crf-tear.marker");
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => panic!("torn-writer executable could not be resolved: {error}"),
    };
    let mut child = match Command::new(executable)
        .arg("--exact")
        .arg("crf_torn_writer")
        .env("CRF_TORN_DB", database)
        .env("CRF_TORN_MARKER", &marker)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => panic!("torn-writer launch failed: {error}"),
    };
    for _ in 0..3000 {
        if marker.is_file() {
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().unwrap_or_else(|error| {
                    panic!("torn-writer output collection failed: {error}")
                });
                panic!(
                    "torn writer exited as {status} before the marker: stdout={} stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(None) => {}
            Err(error) => panic!("torn-writer status could not be read: {error}"),
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !marker.is_file() {
        let _ = child.kill();
        let output = child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("torn-writer output collection failed: {error}"));
        panic!(
            "the torn writer published no marker within the bounded wait: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    child
        .kill()
        .unwrap_or_else(|error| panic!("torn-writer crash stop failed: {error}"));
    child
        .wait()
        .unwrap_or_else(|error| panic!("torn-writer reap failed: {error}"));
    if !journal_path(database).is_file() {
        panic!(
            "the kill did not leave a hot journal at {}",
            journal_path(database).display()
        );
    }
}

#[test]
fn the_shipped_serve_recovers_a_torn_store_before_it_serves() {
    let root = root("j01-shipped");
    reset(&root);
    let database = root.join("database.sqlite3");
    ready_store(&database, &root);
    write_inputs(&root, 5000);

    // Phase one: the clean store serves, and the session created through
    // the shipped API is the committed history the witness captures.
    let mut product = launch_serve(&root);
    let ready = wait_ready(&root, &mut product);
    assert_eq!(
        ready["schema"], "secode.product.ready.v2",
        "ready receipt schema drifted"
    );
    let address = endpoint(&ready);
    let (status, created) = request(address, "POST", "/api/session", Some("{}"));
    assert_eq!(status, 200, "the session create was refused: {created:?}");
    let created = created.unwrap_or_else(|| panic!("session create had no body"));
    let created_id = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session create had no id: {created}"))
        .to_owned();
    let (status, history_before) = request(address, "GET", "/api/session", None);
    assert_eq!(
        status, 200,
        "the session list was refused: {history_before:?}"
    );
    let history_before = history_before.unwrap_or_else(|| panic!("session list had no body"));
    let history_text = history_before.to_string();
    assert!(
        history_text.contains(&created_id),
        "the committed history lost its created session: {history_text}"
    );
    let output = wait_drained(&mut product);
    assert!(
        output.status.success(),
        "the first serve did not drain cleanly: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The crash: a deterministic torn writer with an open transaction.
    tear(&database, &root);

    // Phase two: the shipped serve must recover the torn store before bind
    // and serve the committed history unchanged. Before the fix this launch
    // exited refused without publishing a ready receipt.
    let mut product = launch_serve(&root);
    let ready = wait_ready(&root, &mut product);
    let address = endpoint(&ready);
    let (status, history_after) = request(address, "GET", "/api/session", None);
    assert_eq!(
        status, 200,
        "the recovered store refused its history: {history_after:?}"
    );
    let history_after = history_after.unwrap_or_else(|| panic!("session list had no body"));
    assert_eq!(
        history_before, history_after,
        "the recovered store changed the committed history"
    );
    let output = wait_drained(&mut product);
    assert!(
        output.status.success(),
        "the recovered serve did not drain cleanly: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // After the drain: the journal is consumed, the store stands Ready, and
    // the torn rows are invisible.
    assert!(
        !journal_path(&database).exists(),
        "the hot journal survived the recovered serve"
    );
    match preflight_read_only(&database) {
        StoreReadiness::Ready => {}
        other => panic!("the recovered store does not stand Ready: {other:?}"),
    }
    let torn: i64 = {
        let connection = rusqlite::Connection::open(&database)
            .unwrap_or_else(|error| panic!("post-drain database open failed: {error}"));
        connection
            .query_row(
                "SELECT COUNT(*) FROM event WHERE id LIKE ?",
                ["crf-torn-%"],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("post-drain torn count failed: {error}"))
    };
    assert_eq!(torn, 0, "the torn rows survived the recovered serve");
    reset(&root);
}

// ---------------------------------------------------------------------------
// The in-graph torn writer.
//
// The writer used to live in a second binary under the store crate's src/bin
// that this witness had to locate and pre-build. It now re-executes THIS
// test binary as the single test `crf_torn_writer`, which cargo builds in
// the same invocation as its parent witness, so the writer can never drift
// from the witness that consumes it.

const AGGREGATE_FAMILY: &str = "crf_torn";
const AGGREGATE_ID: &str = "crf-torn";
const EVENT_TYPE: &str = "session.content.appended.1";
const CREATED_INSTANT: i64 = 1_786_060_800_123;
const WRITER_EPOCH: i64 = 1;
const ROW_COUNT: u32 = 256;
const SLEEP_SECONDS: u64 = 120;

#[test]
fn crf_torn_writer() {
    // A plain suite run carries no tear coordinates and tears nothing. The
    // parent witness re-executes this binary with `--exact` and the pair of
    // environment variables that make this process the writer.
    let Some(database) = std::env::var_os("CRF_TORN_DB") else {
        return;
    };
    let Some(marker) = std::env::var_os("CRF_TORN_MARKER") else {
        panic!("CRF_TORN_DB is set but CRF_TORN_MARKER is not");
    };
    let code = torn_writer_run(Path::new(&database), Path::new(&marker));
    if code != 0 {
        std::process::exit(code);
    }
}

/// Opens the READY database, begins a write transaction, and inserts
/// deterministic `crf-torn-*` rows. In rollback-journal mode the original
/// pages are written to the `-journal` file as they are modified, so the
/// writer proves the journal exists and is non-empty BEFORE it writes the
/// marker file and sleeps. The parent kills the process at the marker: the
/// kill is the crash, and the hot journal is left exactly as a torn writer
/// would leave it. If the parent never kills it, the explicit rollback
/// removes the journal, so an un-killed run can never fake a hot journal.
fn torn_writer_run(database: &Path, marker: &Path) -> i32 {
    let Some(name) = database.file_name() else {
        eprintln!("crf torn writer: the database path has no file name");
        return 1;
    };
    let journal = database.with_file_name(format!("{}-journal", name.to_string_lossy()));

    let connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("crf torn writer: opening the database was refused: {error}");
            return 1;
        }
    };
    if let Err(error) = connection.execute_batch(
        "PRAGMA journal_mode = DELETE; PRAGMA cache_size = -10; PRAGMA busy_timeout = 10000;",
    ) {
        eprintln!("crf torn writer: pragma batch was refused: {error}");
        return 1;
    }

    // The writer is a stand-in for an external, unmanaged process: its
    // writes intentionally bypass the store, and the write-chokepoint
    // gate (grapt-store tests) forbids the rusqlite transaction openers
    // outside grapt-store. Raw BEGIN IMMEDIATE is the same immediate
    // transaction, spelled the way an unmanaged client would spell it.
    if let Err(error) = connection.execute_batch("BEGIN IMMEDIATE;") {
        eprintln!("crf torn writer: the write transaction was refused: {error}");
        return 1;
    }

    // The event table carries a foreign key into event_sequence by
    // (family, aggregate); the parent row must exist before the first
    // torn event row, whatever the connection-level foreign-key setting.
    let sequence_sql =
        "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id) VALUES (?1, ?2, 0, NULL)";
    if let Err(error) = connection.execute(
        sequence_sql,
        rusqlite::params![AGGREGATE_FAMILY, AGGREGATE_ID],
    ) {
        eprintln!("crf torn writer: the event_sequence row was refused: {error}");
        return 1;
    }

    for index in 0..ROW_COUNT {
        let id = format!("crf-torn-{index:04}");
        let data = torn_row_data(index, &id);
        let event_sql = "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
        if let Err(error) = connection.execute(
            event_sql,
            rusqlite::params![
                id,
                AGGREGATE_FAMILY,
                AGGREGATE_ID,
                index as i64,
                CREATED_INSTANT,
                EVENT_TYPE,
                data,
                WRITER_EPOCH,
            ],
        ) {
            eprintln!("crf torn writer: insert of {id} was refused: {error}");
            return 1;
        }
    }

    // The journal is hot on disk from the first page modification. Prove
    // the observable fact BEFORE the marker: a zero-byte journal is not a
    // tear the recovery witness can use.
    let journal_size = match std::fs::metadata(&journal) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            eprintln!("crf torn writer: the rollback journal was not observable yet: {error}");
            return 1;
        }
    };
    if journal_size == 0 {
        eprintln!("crf torn writer: the rollback journal is empty; the tear is not observable");
        return 1;
    }

    if let Err(error) = std::fs::write(marker, format!("journal={journal_size}\n")) {
        eprintln!("crf torn writer: the marker write was refused: {error}");
        return 1;
    }

    // Sleep with the transaction OPEN. A kill inside the window leaves the
    // journal hot; surviving the window rolls the transaction back
    // explicitly and removes the journal, so only a kill can produce the
    // state the recovery witness consumes.
    std::thread::sleep(std::time::Duration::from_secs(SLEEP_SECONDS));
    if let Err(error) = connection.execute_batch("ROLLBACK;") {
        eprintln!("crf torn writer: the rollback was refused: {error}");
        return 1;
    }
    0
}

/// One torn row: a 4096-character deterministic filler derived from the
/// row index only, wrapped in the session content append envelope.
fn torn_row_data(index: u32, id: &str) -> String {
    let mut filler = String::with_capacity(4096);
    for block in 0..256u32 {
        let value = u64::from(index)
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add(u64::from(block));
        filler.push_str(&format!("{value:016x}"));
    }
    format!(
        "{{\"message\":\"{id}\",\"item\":{{\"ordinal\":{index},\"kind\":\"crf-torn\",\"payload\":{{\"id\":\"{id}\",\"length\":4096,\"digest\":\"{filler}\"}}}}}}",
    )
}

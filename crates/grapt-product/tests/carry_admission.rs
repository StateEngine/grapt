//! Saved content is verified when its Session is opened, independently of
//! unrelated history. Missing content refuses the Session, not service startup.

#[path = "carry_admission/flip.rs"]
mod flip;
#[path = "support/poll_deadline.rs"]
mod poll_deadline;
#[path = "support/provider_profile.rs"]
mod provider_profile;
#[path = "support/scripted_provider.rs"]
mod scripted_provider;
use poll_deadline::PollDeadline;
use std::fs;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use grapt_store::content_refs::{CARRY_ENUMERATION_QUERY, CARRY_EVENT_TYPE};
use grapt_store::{
    apply_migrations, carried_content_refs, CarryEnumerationBounds, EventStore, MigrationOutcome,
    ProductionMigrations,
};

const ENGINE_VERSION: &str = "0.1.0";

fn root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "grapt-carry-admission-{label}-{}",
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
/// The exact journal id of the sixth production migration - the one that
/// installs the type-leading carry index. The migrations module remains the
/// single owner of the migration itself; the pre-fix state below names the
/// id to remove it.
const CARRY_INDEX_MIGRATION_ID: &str = "20260920_create_event_type_id_index";

/// The exact pre-fix state of an existing database: every production
/// migration applied, then the type-leading carry index and its journal row
/// removed, with the grown unrelated history and a carried content
/// reference installed. This is the state the migration invariant exists to
/// repair; the 25,000 unrelated rows are the same population the store's
/// own plan witnesses assert against, so the plans below are asserted under
/// the volume a live database actually carries.
fn pre_fix_state(database: &Path, backup_root: &Path) {
    ready_store(database, backup_root);
    let connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => panic!("pre-fix database open failed: {error}"),
    };
    let removed = match connection.execute(
        &format!("DELETE FROM rust_migration WHERE id = '{CARRY_INDEX_MIGRATION_ID}'"),
        [],
    ) {
        Ok(removed) => removed,
        Err(error) => panic!("the pre-fix journal reduction failed: {error}"),
    };
    assert_eq!(
        removed, 1,
        "the index migration journal row must be the single row removed"
    );
    if let Err(error) = connection.execute("DROP INDEX event_type_id_idx", []) {
        panic!("the pre-fix index removal failed: {error}");
    }
    drop(connection);
    populate_unrelated_rows(database, 25_000);
    insert_carry_row(database, "evt-crf-prefix-1", 0, &"a".repeat(64), 7);
    insert_carry_row(database, "evt-crf-prefix-2", 1, &"c".repeat(64), 9);
    insert_carry_row(database, "evt-crf-prefix-3", 2, &"e".repeat(64), 11);
}

/// The grown unrelated event history: a real, non-carry kind under its own
/// aggregate, so it can never be confused with the carried content.
fn populate_unrelated_rows(database: &Path, rows: i64) {
    let connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => panic!("pre-fix population open failed: {error}"),
    };
    if let Err(error) = connection.execute(
        "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id) VALUES ('crf-prefix', 'crf-prefix', 0, NULL)",
        [],
    ) {
        panic!("pre-fix population sequence insert failed: {error}");
    }
    if rows == 0 {
        return;
    }
    // The grown history lands as one autocommit statement, not a batch the
    // process opens around it: outside grapt-store, a raw batch opener is
    // a write-chokepoint violation (crates/grapt-store/tests/
    // write_chokepoint.rs), and this one statement is the population's
    // atomic unit - the same 25,000 rows, ids, and types the plan
    // witnesses assert on.
    if let Err(error) = connection.execute(
        &format!(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             SELECT 'evt-crf-prefix-noise-' || value, 'crf-prefix', 'crf-prefix', value, 0,
                    'session.message.appended.1', '{{}}', 1
             FROM (WITH RECURSIVE gen(value) AS (
                     SELECT 0 UNION ALL SELECT value + 1 FROM gen WHERE value < {last})
                  SELECT value FROM gen)",
            last = rows - 1,
        ),
        [],
    ) {
        panic!("pre-fix unrelated population insert failed: {error}");
    }
}

/// One durable content event naming a carried artifact reference, in the
/// exact versioned payload shape the admission enumeration decodes. The
/// artifact object itself is never written: the witness carries the
/// reference without the content, which is what makes the launch refuse
/// during explicit offline inspection.
fn insert_carry_row(database: &Path, event_id: &str, seq: i64, reference_id: &str, length: u64) {
    let connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => panic!("pre-fix carry row open failed: {error}"),
    };
    if let Err(error) = connection.execute(
        "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id) VALUES ('crf-prefix-carry', 'crf-prefix-carry', ?1, NULL)",
        rusqlite::params![seq],
    ) {
        panic!("pre-fix carry sequence insert failed: {error}");
    }
    let data = format!(
        "{{\"message\":\"msg_crfprefix\",\"item\":{{\"ordinal\":0,\"kind\":\"text\",\"payload\":{{\"id\":\"{reference_id}\",\"length\":{length},\"digest\":\"{digest}\"}},\"disposition\":\"complete\"}}}}",
        digest = "b".repeat(64),
    );
    if let Err(error) = connection.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES (?1, 'crf-prefix-carry', 'crf-prefix-carry', ?2, 0, ?3, ?4, 1)",
        rusqlite::params![event_id, seq, CARRY_EVENT_TYPE, data],
    ) {
        panic!("pre-fix carry row insert failed: {error}");
    }
}

/// The exact plan the shipped admission enumeration gets on this database:
/// the production's own query text, with a limit one row past an admitted
/// row ceiling, exactly as the SQL seam bounds it.
fn carry_query_plan(database: &Path, limit: i64) -> Vec<String> {
    let connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => panic!("plan database open failed: {error}"),
    };
    let mut statement =
        match connection.prepare(&format!("EXPLAIN QUERY PLAN {CARRY_ENUMERATION_QUERY}")) {
            Ok(statement) => statement,
            Err(error) => panic!("the carry query plan must prepare: {error}"),
        };
    let rows = match statement.query_map(rusqlite::params![CARRY_EVENT_TYPE, limit], |row| {
        row.get::<_, String>(3)
    }) {
        Ok(rows) => rows,
        Err(error) => panic!("the carry query plan must run: {error}"),
    };
    let mut lines = Vec::new();
    for line in rows {
        match line {
            Ok(detail) => lines.push(detail),
            Err(error) => panic!("the carry query plan must read: {error}"),
        }
    }
    lines
}

/// The index columns exactly as SQLite reports them, for the shape
/// assertion.
fn index_columns(database: &Path, index: &str) -> Vec<String> {
    let connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => panic!("index database open failed: {error}"),
    };
    let mut statement = match connection.prepare(&format!("PRAGMA index_info(\"{index}\")")) {
        Ok(statement) => statement,
        Err(error) => panic!("the index must inspect: {error}"),
    };
    let rows = match statement.query_map([], |row| row.get::<_, Option<String>>(2)) {
        Ok(rows) => rows,
        Err(error) => panic!("the index must inspect: {error}"),
    };
    let mut columns = Vec::new();
    for column in rows {
        match column {
            Ok(Some(name)) => columns.push(name),
            Ok(None) => panic!("the index must carry named columns, not expressions"),
            Err(error) => panic!("the index columns must read: {error}"),
        }
    }
    columns
}

fn index_present(database: &Path, index: &str) -> bool {
    let connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => panic!("index list database open failed: {error}"),
    };
    let mut statement = match connection.prepare("PRAGMA index_list(\"event\")") {
        Ok(statement) => statement,
        Err(error) => panic!("the event index list must prepare: {error}"),
    };
    let names = match statement.query_map([], |row| row.get::<_, String>(1)) {
        Ok(names) => names,
        Err(error) => panic!("the event index list must run: {error}"),
    };
    let mut present = false;
    for name in names {
        match name {
            Ok(name) => present = present || name == index,
            Err(error) => panic!("the event index list must read: {error}"),
        }
    }
    present
}

/// The journal rows for the carry index migration: zero is pre-fix, one is
/// migrated.
fn index_migration_rows(database: &Path) -> i64 {
    let connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => panic!("journal database open failed: {error}"),
    };
    match connection.query_row(
        "SELECT count(*) FROM rust_migration WHERE id = ?1",
        rusqlite::params![CARRY_INDEX_MIGRATION_ID],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(count) => count,
        Err(error) => panic!("the migration journal must count: {error}"),
    }
}

fn write_inputs(root: &Path, serve_window_ms: u64, store_authority: &str) {
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
      "store": { "authority": "__crf-store-authority__" },
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
    }"#
    .replace("__crf-store-authority__", store_authority);
    assert!(
        !platform.contains("__crf-store-authority__"),
        "the store authority placeholder survived the substitution"
    );
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

struct ServeChild {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

fn launch_serve(root: &Path) -> ServeChild {
    static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(0);
    let generation = NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed);
    let stdout = root.join(format!("serve-stdout-{generation}.log"));
    let stderr = root.join(format!("serve-stderr-{generation}.log"));
    let capture = |path: &Path| {
        fs::File::create(path)
            .unwrap_or_else(|error| panic!("{} capture failed: {error}", path.display()))
    };
    // Regular files cannot fill an unread pipe and block service shutdown.
    let child =
        match grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .arg("serve-terminal")
            .arg("--service")
            .arg(root)
            .arg("--port")
            .arg("0")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(capture(&stdout)))
            .stderr(Stdio::from(capture(&stderr)))
            .spawn()
        {
            Ok(child) => child,
            Err(error) => panic!("product launch failed: {error}"),
        };
    ServeChild {
        child,
        stdout,
        stderr,
    }
}

fn captured_output(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    let read = fs::File::open(path).and_then(|file| file.take(65_537).read_to_end(&mut bytes));
    if let Err(error) = read {
        panic!("{} output collection failed: {error}", path.display());
    }
    if bytes.len() > 65_536 {
        bytes.truncate(65_536);
        bytes.extend_from_slice(
            format!("\n[output truncated; full log: {}]\n", path.display()).as_bytes(),
        );
    }
    bytes
}

fn collect_output(child: &mut ServeChild) -> Output {
    let status = match child.child.wait() {
        Ok(status) => status,
        Err(error) => panic!("product status could not be read: {error}"),
    };
    Output {
        status,
        stdout: captured_output(&child.stdout),
        stderr: captured_output(&child.stderr),
    }
}

fn wait_ready(root: &Path, child: &mut ServeChild) -> serde_json::Value {
    wait_ready_with_budget(root, child, 3000)
}

fn wait_ready_with_budget(root: &Path, child: &mut ServeChild, polls: usize) -> serde_json::Value {
    let child_pid = child.child.id();
    let ready_path = root.join("serve-ready.json");
    for _ in 0..polls {
        if let Ok(bytes) = fs::read(&ready_path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["pid"].as_u64() == Some(u64::from(child_pid)) {
                    return value;
                }
            }
        }
        match child.child.try_wait() {
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
    let _ = child.child.kill();
    let output = collect_output(child);
    panic!(
        "product published no ready receipt at {}: stdout={} stderr={}",
        ready_path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_drained(child: &mut ServeChild) -> Output {
    // End through the private control pipe once the phase's checks are done;
    // the configured serve window is only a fallback for a stalled fixture.
    drop(child.child.stdin.take());
    let deadline = PollDeadline::new();
    let drained = deadline.poll(Duration::from_millis(10), || match child.child.try_wait() {
        Ok(Some(_)) => Some(collect_output(child)),
        Ok(None) => None,
        Err(error) => panic!("product status could not be read: {error}"),
    });
    if let Some(output) = drained {
        return output;
    }
    let _ = child.child.kill();
    let output = collect_output(child);
    panic!(
        "the service did not drain after control EOF ({deadline}): stdout={} stderr={}",
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

fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, Option<serde_json::Value>) {
    let mut stream = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .unwrap_or_else(|error| panic!("endpoint connection failed: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
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

/// Bounded wait for the pre-bind refusal: no ready receipt for this pid
/// may appear, and the process must exit non-zero. A clean exit over an
/// incomplete carry is the failure this witness exists to catch.
fn wait_refused(root: &Path, child: &mut ServeChild) -> Output {
    let child_pid = child.child.id();
    let ready_path = root.join("serve-ready.json");
    for _ in 0..6000 {
        if let Ok(bytes) = fs::read(&ready_path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["pid"].as_u64() == Some(u64::from(child_pid)) {
                    let _ = child.child.kill();
                    let output = collect_output(child);
                    panic!(
                        "the incomplete carry bound its endpoint before refusing: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
        match child.child.try_wait() {
            Ok(Some(status)) => {
                let output = collect_output(child);
                assert!(
                    !status.success(),
                    "the product exited cleanly over an incomplete carry: stdout={} stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return output;
            }
            Ok(None) => {}
            Err(error) => panic!("product status could not be read: {error}"),
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.child.kill();
    let output = collect_output(child);
    panic!(
        "the incomplete carry neither bound nor refused within the bounded wait: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Wait for the answer's terminal finish before saving the history that a
/// reopened service must reproduce. Text can be visible while still streaming.
fn wait_message(address: SocketAddr, session_id: &str) -> serde_json::Value {
    let deadline = PollDeadline::new();
    while deadline.pending() {
        let (status, value) = request(
            address,
            "GET",
            &format!("/api/session/{session_id}/message"),
            None,
        );
        assert!(
            (200..300).contains(&status),
            "the message endpoint was refused: {value:?}"
        );
        let value = value.unwrap_or_else(|| panic!("the message endpoint had no body"));
        let finished = value["data"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["type"] == "assistant"
                    && row["finish"] == "stop"
                    && row["content"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part["type"] == "text"
                                && part["text"] == "fixture answer for: carry probe"
                        })
                    })
            })
        });
        if finished {
            return value;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("the assistant answer did not finish through the message endpoint ({deadline})");
}

/// A Session whose committed history names durable content outside the
/// database, proven to serve that identical history across a restart.
struct Carry {
    session_id: String,
    references: Vec<grapt_types::ArtifactRef>,
    _provider: scripted_provider::Server,
}

fn establish_carry(root: &Path) -> Carry {
    let database = root.join("database.sqlite3");
    ready_store(&database, root);
    write_inputs(root, 120_000, "open-ready");
    let provider = scripted_provider::Server::start(Vec::new(), |_| false)
        .unwrap_or_else(|error| panic!("provider fixture: {error}"));
    provider_profile::configure(root, provider.address)
        .unwrap_or_else(|error| panic!("provider profile: {error}"));

    // Phase one: the artifact-free carry is admitted, and the prompt
    // answer is the durable content the database will carry from here on.
    let mut product = launch_serve(root);
    let ready = wait_ready(root, &mut product);
    let address = endpoint(&ready);
    let (status, created) = request(address, "POST", "/api/session", Some("{}"));
    assert!(
        (200..300).contains(&status),
        "the session create was refused: {created:?}"
    );
    let created = created.unwrap_or_else(|| panic!("session create had no body"));
    let session_id = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session create had no id: {created}"))
        .to_owned();
    let (status, prompt) = request(
        address,
        "POST",
        &format!("/api/session/{session_id}/prompt"),
        Some(r#"{"text":"carry probe"}"#),
    );
    assert!(
        (200..300).contains(&status),
        "the prompt was refused: {prompt:?}"
    );
    let messages = wait_message(address, &session_id);
    let output = wait_drained(&mut product);
    assert!(
        output.status.success(),
        "the carry-establishing serve did not drain cleanly: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The database now names durable content that lives outside itself.
    let bounds = CarryEnumerationBounds::new(10_000, 8 * 1024 * 1024, 256 * 1024 * 1024, 1024)
        .unwrap_or_else(|| panic!("a nonzero test bound must construct"));
    let references = carried_content_refs(&database, &bounds)
        .unwrap_or_else(|error| panic!("the established carry is unreadable: {error}"));
    assert!(
        !references.is_empty(),
        "the prompt did not establish any carried content"
    );

    // Phase two: the complete carry serves the identical history.
    let mut product = launch_serve(root);
    let ready = wait_ready(root, &mut product);
    let address = endpoint(&ready);
    let (status, carried) = request(
        address,
        "GET",
        &format!("/api/session/{session_id}/message"),
        None,
    );
    assert!(
        (200..300).contains(&status),
        "the complete carry refused its history: {carried:?}"
    );
    let carried = carried.unwrap_or_else(|| panic!("the complete carry had no body"));
    assert_eq!(
        messages, carried,
        "the complete carry changed the committed history"
    );
    let output = wait_drained(&mut product);
    assert!(
        output.status.success(),
        "the complete carry did not drain cleanly: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    Carry {
        session_id,
        references,
        _provider: provider,
    }
}

#[test]
fn an_incomplete_session_carry_is_refused_on_open() {
    let root = root("j02-carry");
    reset(&root);
    let Carry {
        session_id,
        references,
        _provider,
    } = establish_carry(&root);

    // An incomplete Session refuses on open/history/resume; startup and a
    // different Session remain usable. Failed verification is never cached.
    fs::remove_dir_all(root.join("dogfood-artifacts"))
        .unwrap_or_else(|error| panic!("artifact carry removal failed: {error}"));
    let mut product = launch_serve(&root);
    let ready = wait_ready(&root, &mut product);
    let address = endpoint(&ready);
    for path in [
        format!("/api/session/{session_id}"),
        format!("/api/session/{session_id}/message"),
    ] {
        let (status, body) = request(address, "GET", &path, None);
        assert!(status >= 400, "incomplete Session was served: {body:?}");
        let body = body.unwrap_or_else(|| panic!("carry refusal body is absent"));
        let text = body.to_string();
        assert!(
            text.contains("carry refused") && text.contains("absent"),
            "{text}"
        );
        assert!(
            references
                .iter()
                .any(|reference| text.contains(reference.id().as_str())),
            "{text}"
        );
    }
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session_id}/prompt"),
        Some(r#"{"text":"must refuse"}"#),
    );
    assert!(status >= 400, "incomplete Session resumed: {body:?}");
    let (status, body) = request(address, "POST", "/api/session", Some("{}"));
    assert!(
        (200..300).contains(&status),
        "unrelated Session refused: {body:?}"
    );
    assert!(wait_drained(&mut product).status.success());
    reset(&root);
}

#[test]
fn the_first_launch_of_a_pre_fix_store_migrates_before_bind() {
    // J02 shipped-composition witness for the F4 correction. The claim
    // under test is the ORDER of the shipped serve, not a library
    // property: the authorized store open - and, under the migrate
    // directive, the migration it runs - establishes the type-leading
    // carry index before the first carried-reference query, with the
    // admission still before the listener bind. Only the real binary can
    // witness that, so the test drives it end to end from the exact
    // pre-fix state of a live database.
    let bounds = CarryEnumerationBounds::new(10_000, 8 * 1024 * 1024, 256 * 1024 * 1024, 1024)
        .unwrap_or_else(|| panic!("a nonzero test bound must construct"));
    let plan_limit = bounds.max_rows() as i64 + 1;

    // Phase 0: the exact pre-fix state, and the hazard it names. Without
    // the type-leading index the admission enumeration has no bounded
    // access path on the grown table.
    let make_root = root;
    let root = root("j02-prefix");
    reset(&root);
    let database = root.join("database.sqlite3");
    pre_fix_state(&database, &root.join("migration-backup"));
    assert_eq!(
        index_migration_rows(&database),
        0,
        "the pre-fix state must not journal the index migration"
    );
    assert!(
        !index_present(&database, "event_type_id_idx"),
        "the pre-fix state must not carry the index"
    );
    let plan = carry_query_plan(&database, plan_limit);
    let plan_text = plan.join(" | ");
    assert!(
        !plan.iter().any(|line| line.contains("event_type_id_idx")),
        "the pre-fix state cannot search an index that is absent: {plan_text}"
    );
    assert!(
        plan.iter().any(|line| line.contains("SCAN event")),
        "the pre-fix admission enumeration must be a full scan of the grown table: {plan_text}"
    );

    // Migration still precedes bind, but unrelated missing content no longer
    // blocks the service. Whole-carry inspection remains available offline.
    write_inputs(&root, 5000, "migrate");
    let mut product = launch_serve(&root);
    let _ready = wait_ready(&root, &mut product);
    assert!(wait_drained(&mut product).status.success());
    assert_eq!(
        index_migration_rows(&database),
        1,
        "the first launch must have recorded the index migration before refusing at admission"
    );
    assert_eq!(
        index_columns(&database, "event_type_id_idx"),
        ["type".to_owned(), "id".to_owned()],
        "the migrated index must carry exactly the query's predicate and ordering columns"
    );
    let plan = carry_query_plan(&database, plan_limit);
    let plan_text = plan.join(" | ");
    assert!(
        plan.iter()
            .any(|line| line.contains("SEARCH") && line.contains("event_type_id_idx")),
        "the first launch's admission enumeration must have searched the type-leading index: {plan_text}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("SCAN event")),
        "the first launch must not leave a full-scan access path: {plan_text}"
    );

    // Phase 2: the order disambiguation. Even a REFUSED store open precedes
    // the admission: under the open-ready directive the pre-fix database is
    // refused at the store authorization itself, before any event table is
    // read and before the admission could refuse.
    let ready_root = make_root("j02-prefix-ready");
    reset(&ready_root);
    let ready_database = ready_root.join("database.sqlite3");
    pre_fix_state(&ready_database, &ready_root.join("migration-backup"));
    write_inputs(&ready_root, 5000, "open-ready");
    let mut product = launch_serve(&ready_root);
    let output = wait_refused(&ready_root, &mut product);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("\"outcome\":\"refused\""),
        "the refusal receipt is missing: {stdout}"
    );
    assert!(
        stderr.contains("does not authorize first-contact state MigrationNeeded"),
        "the store authorization refusal is not named: {stderr}"
    );
    assert!(
        !stderr.contains("incomplete database carry refused before bind"),
        "the store open must refuse before the carry admission runs: {stderr}"
    );
    assert_eq!(
        index_migration_rows(&ready_database),
        0,
        "the refused open must not have migrated the database"
    );
    assert!(
        !index_present(&ready_database, "event_type_id_idx"),
        "the refused open must not have created the index"
    );

    reset(&root);
    reset(&ready_root);
}

#[test]
fn startup_with_more_than_the_global_content_bound_stays_within_five_seconds() {
    let root = root("large-history");
    reset(&root);
    let database = root.join("database.sqlite3");
    ready_store(&database, &root);
    write_inputs(&root, 5000, "open-ready");
    let mut seed = launch_serve(&root);
    let ready = wait_ready(&root, &mut seed);
    let (status, body) = request(
        endpoint(&ready),
        "POST",
        "/api/session",
        Some(r#"{"id":"ses_oldhistory"}"#),
    );
    assert!(
        (200..300).contains(&status),
        "history Session seed refused: {body:?}"
    );
    assert!(wait_drained(&mut seed).status.success());
    let connection = rusqlite::Connection::open(&database)
        .unwrap_or_else(|error| panic!("synthetic history open failed: {error}"));
    connection
        .execute(
            "UPDATE event_sequence SET seq=65538
         WHERE aggregate_family='session' AND aggregate_id='ses_oldhistory'",
            [],
        )
        .unwrap_or_else(|error| panic!("history sequence failed: {error}"));
    connection.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         SELECT 'evt_history_' || value, 'session', 'ses_oldhistory', value+1, 0,
                'session.content.appended.1',
                json_object('message', 'msg_history', 'item', json_object(
                  'ordinal', 0, 'kind', 'text', 'disposition', 'complete',
                  'payload', json_object('id', printf('%064x', value), 'length', 7,
                                         'digest', printf('%064x', value)))), 1
         FROM (WITH RECURSIVE gen(value) AS (
           SELECT 1 UNION ALL SELECT value + 1 FROM gen WHERE value < 65537)
           SELECT value FROM gen)", [],
    ).unwrap_or_else(|error| panic!("synthetic content insertion failed: {error}"));
    drop(connection);
    write_inputs(&root, 5000, "open-ready");
    // Measure through the approved clock adapter as well as bounding the poll.
    // A regressed launch cannot pass merely because filesystem polling is slow.
    let clock = grapt_store::SystemClock;
    let started = grapt_types::ClockAuthority::now(&clock).as_millis();
    let mut product = launch_serve(&root);
    let ready = wait_ready_with_budget(&root, &mut product, 500);
    let elapsed = grapt_types::ClockAuthority::now(&clock).as_millis() - started;
    assert!(
        (0..=5000).contains(&elapsed),
        "large-history startup took {elapsed} ms"
    );
    eprintln!("large-history startup: {elapsed} ms");
    let address = endpoint(&ready);
    let (status, created) = request(address, "POST", "/api/session", Some("{}"));
    assert!(
        (200..300).contains(&status),
        "new Session refused: {created:?}"
    );
    let created = created.unwrap_or_else(|| panic!("new Session body is absent"));
    let id = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("Session id absent"));
    let (status, opened) = request(address, "GET", &format!("/api/session/{id}"), None);
    assert!(
        (200..300).contains(&status),
        "unrelated history blocked open: {opened:?}"
    );
    assert!(wait_drained(&mut product).status.success());
    reset(&root);
}

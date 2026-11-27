//! The capture stream's product-level journeys (charter
//! EVENT-STREAM-REPLAY-CURSOR-01, revision 8, obligation B4).
//!
//! Everything goes through the shipped binary against a disposable
//! installation and a scripted loopback provider. The replay-and-restart
//! journey asserts that a stream opened mid-turn presents every durable
//! session row exactly once with the same reconstruction the live sink
//! uses on a continuing stream, and that a
//! restarted product replays the identical coordinate sequence before it
//! appends new rows. The live-cross journey asserts that the mid-turn
//! stream crosses from replay into live at the provider hold, and that a
//! second stream resumed from an explicit live checkpoint completes
//! the durable ground truth with no duplicates and no gaps.
#![cfg(feature = "loopback-provider-testkit")]

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use grapt_store::{apply_migrations, EventStore, MigrationOutcome, ProductionMigrations};

#[path = "support/poll_deadline.rs"]
mod poll_deadline;
use poll_deadline::PollDeadline;

const AUTHORIZATION: &str = "Basic YWxwaGE6emVwaHly";
// The product's bounded continuation can legitimately exceed 64 KiB before
// its 4 MiB context wall. Keep the loopback transport bounded while allowing
// structural-bound journeys to reach the product admission owner.
const PROVIDER_HEADER_BYTES: usize = 64 * 1_024;
// A legal 1 MiB tool artifact plus JSON framing must reach the scripted provider.
const PROVIDER_BODY_BYTES: usize = 2 * 1_024 * 1_024;
const PROVIDER_REQUEST_BYTES: usize = 2 * 1_024 * 1_024;
const BARRIER_WAIT_POLLS: usize = 120;

struct BarrierState {
    arrived: usize,
    generation: usize,
}

/// Reusable test barrier with a deterministic wait bound.
///
/// Process fixtures deliberately hold provider responses open. A missing
/// admission must fail with its observed arrival count instead of retaining a
/// carrier until an outer route timeout.
struct Barrier {
    parties: usize,
    state: Mutex<BarrierState>,
    wake: Condvar,
}

#[allow(dead_code)]
impl Barrier {
    fn new(parties: usize) -> Self {
        assert!(parties > 0, "test barrier requires at least one party");
        Self {
            parties,
            state: Mutex::new(BarrierState {
                arrived: 0,
                generation: 0,
            }),
            wake: Condvar::new(),
        }
    }

    fn wait(&self) -> usize {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let generation = state.generation;
        assert!(
            state.arrived < self.parties,
            "test barrier admitted more arrivals than parties"
        );
        state.arrived = match state.arrived.checked_add(1) {
            Some(arrived) => arrived,
            None => panic!("test barrier arrival count overflowed"),
        };
        if state.arrived == self.parties {
            state.arrived = 0;
            state.generation = match state.generation.checked_add(1) {
                Some(generation) => generation,
                None => panic!("test barrier generation overflowed"),
            };
            let released = state.generation;
            self.wake.notify_all();
            return released;
        }
        for _ in 0..BARRIER_WAIT_POLLS {
            let (next, _) = match self.wake.wait_timeout(state, Duration::from_millis(250)) {
                Ok(waited) => waited,
                Err(poisoned) => poisoned.into_inner(),
            };
            state = next;
            if state.generation != generation {
                return state.generation;
            }
        }
        panic!(
            "test barrier generation {generation} exceeded {BARRIER_WAIT_POLLS} polls of 250ms; observed {} of {} parties",
            state.arrived, self.parties
        );
    }
}
const SUCCESS: &str = concat!(
    "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Because.\"}\n\n",
    "data: {\"type\":\"response.reasoning_summary_text.done\"}\n\n",
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello from the live provider.\"}\n\n",
    "data: {\"type\":\"response.output_text.done\"}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":4,\"total_tokens\":15,\"input_tokens_details\":{\"cached_tokens\":2,\"cache_write_tokens\":1},\"output_tokens_details\":{\"reasoning_tokens\":3}}}}\n\n"
);

// The fixture modules are shared with the dogfood_headless root, which uses
// every item; this journey uses a subset, so the remainder stays admitted.
#[allow(dead_code)]
#[path = "support/shipped_cli.rs"]
mod shipped_cli;

#[path = "support/catalog_boundary.rs"]
mod catalog_boundary;

#[allow(dead_code)]
#[path = "dogfood_headless/terminal_coordination.rs"]
mod terminal_coordination;

#[path = "dogfood_headless/client_allowance.rs"]
mod client_allowance;

#[allow(dead_code)]
#[path = "dogfood_headless/provider_fixture.rs"]
mod provider_fixture;
use provider_fixture::*;

#[allow(dead_code)]
#[path = "dogfood_headless/provider_profile.rs"]
mod provider_profile;
use provider_profile::*;

#[allow(dead_code)]
#[path = "dogfood_headless/process_fixture.rs"]
mod process_fixture;
use process_fixture::{await_ready, spawn};

#[cfg(target_os = "linux")]
#[path = "capture/cmdline_stress.rs"]
mod cmdline_stress;

fn request(address: SocketAddr, method: &str, path: &str, body: &str) -> (u16, Vec<u8>) {
    request_with_read_timeout(address, method, path, body, Duration::from_secs(8))
}

fn request_with_read_timeout(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    read_timeout: Duration,
) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(address)
        .unwrap_or_else(|error| panic!("connect {path} failed: {error}"));
    stream
        .set_read_timeout(Some(read_timeout))
        .unwrap_or_else(|error| panic!("request timeout failed: {error}"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .unwrap_or_else(|error| panic!("write {path} failed: {error}"));
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap_or_else(|error| {
        panic!(
            "read {path} failed: {error}; received {} bytes, bounded prefix: {}",
            bytes.len(),
            String::from_utf8_lossy(&bytes[..bytes.len().min(2048)])
        )
    });
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("{path} returned no headers"));
    let head = String::from_utf8_lossy(&bytes[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_else(|| panic!("{path} returned no status"));
    (status, bytes[split + 4..].to_vec())
}

/// The unauthenticated form: the route must refuse before any query or
/// cursor work exists.
fn request_unauthenticated(address: SocketAddr, method: &str, path: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(address)
        .unwrap_or_else(|error| panic!("connect {path} failed: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap_or_else(|error| panic!("request timeout failed: {error}"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .unwrap_or_else(|error| panic!("write {path} failed: {error}"));
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .unwrap_or_else(|error| panic!("read {path} failed: {error}"));
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("{path} returned no headers"));
    let head = String::from_utf8_lossy(&bytes[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_else(|| panic!("{path} returned no status"));
    (status, bytes[split + 4..].to_vec())
}

fn json(address: SocketAddr, method: &str, path: &str, body: &str) -> serde_json::Value {
    let (status, bytes) = request(address, method, path, body);
    assert_eq!(status, 200, "{path}: {}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("{path} response was not JSON: {error}"))
}

fn percent_encode(raw: &str) -> String {
    raw.as_bytes()
        .iter()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(*byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn create(address: SocketAddr) -> String {
    json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned()
}

/// Create a session pinned to one location and return its id plus the
/// directory the product resolved for it, exactly as the session reads it
/// back. The capture query must carry that read-back value verbatim: the
/// location filter matches the directory the session-list projection stored.
fn create_at(address: SocketAddr, directory: &Path) -> (String, String) {
    let body = serde_json::json!({
        "location": { "directory": directory.to_string_lossy().into_owned() }
    })
    .to_string();
    let created = json(address, "POST", "/api/session", &body);
    let id = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id: {created}"))
        .to_owned();
    let directory = created["data"]["location"]["directory"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no resolved location: {created}"))
        .to_owned();
    (id, directory)
}

/// Open the capture stream and return the connection once the connected
/// frame has arrived. An empty query is the all-aggregate interest.
fn capture_stream(address: SocketAddr, query: &str) -> (TcpStream, Vec<u8>) {
    let path = if query.is_empty() {
        "/api/capture/v1/events".to_owned()
    } else {
        format!("/api/capture/v1/events?{query}")
    };
    let mut stream = TcpStream::connect(address)
        .unwrap_or_else(|error| panic!("capture stream connect failed: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap_or_else(|error| panic!("capture read timeout failed: {error}"));
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap_or_else(|error| panic!("capture request failed: {error}"));
    let mut observed = Vec::new();
    let mut chunk = [0u8; 8_192];
    for _ in 0..64 {
        let count = stream
            .read(&mut chunk)
            .unwrap_or_else(|error| panic!("capture connect read failed: {error}"));
        observed.extend_from_slice(&chunk[..count]);
        if String::from_utf8_lossy(&observed).contains("capture.connected") {
            return (stream, observed);
        }
    }
    panic!(
        "capture stream carried no connected frame: {}",
        String::from_utf8_lossy(&observed)
    );
}

fn collect(stream: TcpStream, observed: Vec<u8>, terminal: &str) -> String {
    collect_until(stream, observed, terminal, 1)
}

fn collect_successes(stream: TcpStream, observed: Vec<u8>, count: usize) -> String {
    collect_until(stream, observed, "session.execution.succeeded", count)
}

fn collect_until(
    mut stream: TcpStream,
    mut observed: Vec<u8>,
    terminal: &str,
    count: usize,
) -> String {
    let mut chunk = [0u8; 8_192];
    for _ in 0..4096 {
        assert!(
            observed.len() <= 4 * 1024 * 1024,
            "capture witness exceeded its byte bound"
        );
        if terminal_frame_complete(&observed, terminal)
            && String::from_utf8_lossy(&observed)
                .rsplit_once("\n\n")
                .is_some_and(|(complete, _)| complete.matches(terminal).count() >= count)
        {
            return String::from_utf8_lossy(&observed).into_owned();
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => observed.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break
            }
            Err(error) => panic!("capture read failed: {error}"),
        }
    }
    panic!(
        "capture stream carried no {terminal}: {}",
        String::from_utf8_lossy(&observed[observed.len().saturating_sub(4096)..])
    );
}

/// The terminal only counts once its SSE frame has fully arrived. A large
/// frame crosses TCP segment boundaries, so the marker can sit in an early
/// segment while the frame's closing newlines are still in flight; returning
/// then would hand the witness a truncated event. The first raw newline pair
/// after the marker is that frame's own terminator: serialized event data
/// never carries a raw newline.
fn terminal_frame_complete(observed: &[u8], terminal: &str) -> bool {
    let Some(marker) = observed
        .windows(terminal.len())
        .position(|window| window == terminal.as_bytes())
    else {
        return false;
    };
    observed[marker..]
        .windows(2)
        .any(|window| window == b"\n\n")
}

fn data_frames(observed: &str) -> Vec<serde_json::Value> {
    let mut frames = Vec::new();
    for line in observed.lines() {
        let Some(encoded) = line.strip_prefix("data: ") else {
            continue;
        };
        match serde_json::from_str::<serde_json::Value>(encoded) {
            Ok(value) => frames.push(value),
            Err(_) => continue,
        }
    }
    frames
}

fn first_frame(observed: &str) -> serde_json::Value {
    match data_frames(observed).into_iter().next() {
        Some(value) => value,
        None => panic!("stream carried no frame"),
    }
}

fn wait_capture_idle(address: SocketAddr, session: &str) {
    let idle = (0..3000).any(|_| {
        let active = json(address, "GET", "/api/session/active", "");
        if active["data"].get(session).is_none() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
        false
    });
    assert!(idle, "capture execution did not settle");
}

fn assert_continuing(observed: &str) {
    assert!(
        data_frames(observed)
            .iter()
            .all(|frame| frame["type"] != "capture.terminated"),
        "normal completion terminated capture: {observed}"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Event,
    Skipped,
}

/// One presented capture row: the durable coordinate and what the wire said.
#[derive(Debug)]
struct CapturedRow {
    aggregate: String,
    seq: u64,
    frame: FrameKind,
    /// The stored kind (Skipped) or the wire event type (Event).
    kind: String,
}

fn capture_rows(observed: &str) -> Vec<CapturedRow> {
    let mut rows = Vec::new();
    for value in data_frames(observed) {
        let frame = match value.get("type").and_then(serde_json::Value::as_str) {
            Some("capture.event") => FrameKind::Event,
            Some("capture.skipped") => FrameKind::Skipped,
            _ => continue,
        };
        let kind = match frame {
            FrameKind::Event => value["event"]["type"]
                .as_str()
                .unwrap_or_else(|| panic!("event frame carried no wire type: {value}"))
                .to_owned(),
            FrameKind::Skipped => value["kind"]
                .as_str()
                .unwrap_or_else(|| panic!("skipped frame carried no kind: {value}"))
                .to_owned(),
        };
        let coordinate = &value["coordinate"];
        let aggregate = coordinate["aggregateID"]
            .as_str()
            .unwrap_or_else(|| panic!("row frame carried no aggregate: {value}"))
            .to_owned();
        let seq = coordinate["seq"]
            .as_u64()
            .unwrap_or_else(|| panic!("row frame carried no sequence: {value}"));
        if frame == FrameKind::Event {
            let durable_aggregate = value["event"]["durable"]["aggregateID"]
                .as_str()
                .unwrap_or_else(|| panic!("durable event carried no aggregate: {value}"));
            assert_eq!(
                durable_aggregate, aggregate,
                "durable reference disagrees with its coordinate: {value}"
            );
            if let Some(session) = value["event"]["data"]["sessionID"].as_str() {
                assert_eq!(
                    session, aggregate,
                    "event payload sessionID disagrees with its coordinate: {value}"
                );
            }
        }
        rows.push(CapturedRow {
            aggregate,
            seq,
            frame,
            kind,
        });
    }
    rows
}

fn coordinates(observed: &str) -> Vec<(String, u64, String)> {
    capture_rows(observed)
        .into_iter()
        .map(|row| (row.aggregate, row.seq, row.kind))
        .collect()
}

/// The durable ground truth, read after the product is dead: every session
/// row the engine committed, in the store's own presentation order. This
/// mirrors the capture page's all-interest SQL exactly.
fn durable_rows(database: &Path) -> Vec<(String, u64, String)> {
    let connection = rusqlite::Connection::open(database)
        .unwrap_or_else(|error| panic!("ground truth store open failed: {error}"));
    let mut statement = connection
        .prepare(
            "SELECT aggregate_id, seq, type FROM event \
             WHERE aggregate_family = 'session' ORDER BY aggregate_id, seq",
        )
        .unwrap_or_else(|error| panic!("ground truth query failed: {error}"));
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                u64::try_from(row.get::<_, i64>(1)?).unwrap_or(u64::MAX),
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap_or_else(|error| panic!("ground truth read failed: {error}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("ground truth row failed: {error}"));
    rows
}

/// The capture law, stated from the wire's perspective: for each stored kind,
/// what the stream must present. "either" names the payload-dependent cases:
/// the projection reconstructs when the stored payload carries everything the
/// wire form needs (replay coordinates, an executor selection), and skips by
/// name when it does not. Every other kind is durable truth the wire cannot
/// reconstruct from the stored row alone, so it is named, skipped, and the
/// cursor still advances over it.
fn expected(kind: &str) -> &'static str {
    match kind {
        "session.input.admitted.1"
        | "session.input.promoted.1"
        | "session.agent.selected.1"
        | "session.model.selected.1"
        | "session.subagent-model.selected.1"
        | "session.subagent-model.cleared.1"
        | "session.execution.started.1"
        | "session.execution.succeeded.1"
        | "session.execution.failed.1"
        | "session.execution.interrupted.1"
        | "session.compaction.ended.1" => "event",
        "permission.asked.2"
        | "permission.replied.2"
        | "session.assistant-message.opened.1"
        | "session.assistant-message.opened.2" => "either",
        _ => "skipped",
    }
}

/// The wire type a stored kind reconstructs to. The durable kind is
/// versioned storage vocabulary; the wire type is usually the kind minus its
/// version, except the assistant opening, which the projection reconstructs
/// as the Step start when the stored payload carries the selection.
fn wire_kind(kind: &str) -> &str {
    if kind == "session.assistant-message.opened.2" {
        return "session.step.started";
    }
    match kind.rfind('.') {
        Some(dot) if dot > 0 => &kind[..dot],
        _ => kind,
    }
}

fn assert_rows_match_truth(rows: &[CapturedRow], truth: &[(String, u64, String)], label: &str) {
    let mut stored: BTreeMap<(String, u64), &str> = BTreeMap::new();
    for (aggregate, seq, kind) in truth {
        stored.insert((aggregate.clone(), *seq), kind);
    }
    for row in rows {
        let key = (row.aggregate.clone(), row.seq);
        let kind = *stored
            .get(&key)
            .unwrap_or_else(|| panic!("{label}: coordinate {key:?} is not in the durable truth"));
        match (row.frame, expected(kind)) {
            (FrameKind::Event, "event") | (FrameKind::Event, "either") => assert_eq!(
                row.kind,
                wire_kind(kind),
                "{label}: {key:?}: stored {kind} projected to {}",
                row.kind
            ),
            (FrameKind::Skipped, "skipped") | (FrameKind::Skipped, "either") => assert_eq!(
                row.kind, kind,
                "{label}: {key:?}: stored {kind} was skipped as {}",
                row.kind
            ),
            (FrameKind::Event, want) => {
                panic!("{label}: {key:?}: presented an event where {want} is owed")
            }
            (FrameKind::Skipped, want) => panic!("{label}: {key:?}: skipped where {want} is owed"),
        }
    }
}

/// The full bijection: the stream presented exactly the durable truth, no
/// more, no less. The partial form is for streams that lawfully resume
/// mid-history.
fn assert_capture_matches_truth(stream: &str, truth: &[(String, u64, String)], label: &str) {
    let rows = capture_rows(stream);
    assert_eq!(
        rows.len(),
        truth.len(),
        "{label}: the stream presented {} rows, the store holds {}",
        rows.len(),
        truth.len()
    );
    assert_rows_match_truth(&rows, truth, label);
}

/// The positive witnesses: the journey actually carried the interesting
/// shapes, so the bijection above is not vacuously small.
fn assert_witnesses(stream: &str) {
    let rows = capture_rows(stream);
    let event = |wanted: &str| {
        rows.iter()
            .any(|row| row.frame == FrameKind::Event && row.kind == wanted)
    };
    let skipped = |wanted: &str| {
        rows.iter()
            .any(|row| row.frame == FrameKind::Skipped && row.kind == wanted)
    };
    assert!(
        event("session.input.admitted"),
        "journey carried no admitted input: {rows:?}"
    );
    assert!(
        event("session.execution.started"),
        "journey carried no execution start: {rows:?}"
    );
    assert!(
        event("session.step.started"),
        "journey carried no step start: {rows:?}"
    );
    assert!(
        event("session.execution.succeeded"),
        "journey carried no execution success: {rows:?}"
    );
    assert!(
        skipped("session.created.1"),
        "journey carried no session creation: {rows:?}"
    );
    assert!(
        skipped("session.content.appended.1") || skipped("session.content-block.opened.1"),
        "journey carried no named content skip: {rows:?}"
    );
}

#[test]
fn capture_replay_and_restart_journey() {
    let _journey = process_journey_custody();
    let root = root("capture-replay");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    // Turn one holds its provider response between two latches: the first
    // stream opens on the exact mid-turn page (step rows committed,
    // response held, no reconcile yet), and the execution terminal
    // fires only while the stream is subscribed. Turn two runs normally on a continuing stream.
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::HeldOutput {
            bytes: 1_024,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);

    // The route is authenticated before any other refusal exists.
    let (status, body) = request_unauthenticated(address, "GET", "/api/capture/v1/events");
    assert_eq!(
        status,
        401,
        "unauthenticated capture: {}",
        String::from_utf8_lossy(&body)
    );
    let (status, body) = request(address, "GET", "/api/capture/v1/events?cursor=garbage", "");
    assert_eq!(
        status,
        400,
        "malformed cursor: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(
        String::from_utf8_lossy(&body).contains("invalid capture cursor"),
        "malformed cursor refusal misnamed: {}",
        String::from_utf8_lossy(&body)
    );

    let session = create(address);
    let prompt_path = format!("/api/session/{session}/prompt");
    let prompt =
        r#"{"id":"msg_capture_replay_1","text":"hello replay","delivery":"queue","resume":true}"#;
    let (status, body) = request(address, "POST", &prompt_path, prompt);
    assert_eq!(status, 200, "prompt: {}", String::from_utf8_lossy(&body));
    entered.wait();
    // The provider request is read and the response is held: the step rows
    // are committed and the turn owes the rest. The stream opens into that
    // exact state; releasing it settles the turn under the stream.
    let (stream, observed) = capture_stream(address, "");
    release.wait();
    let stream_a = collect(stream, observed, "session.execution.succeeded");
    child.stop_after_witness(&root);

    let truth_a = durable_rows(&root.join("database.sqlite3"));
    assert!(
        !truth_a.is_empty(),
        "journey committed no durable session rows"
    );
    let first = first_frame(&stream_a);
    assert_eq!(
        first["type"].as_str(),
        Some("capture.connected"),
        "first frame: {first}"
    );
    assert_eq!(first["version"].as_u64(), Some(1), "first frame: {first}");
    assert_eq!(
        first["interest"].as_str(),
        Some("all"),
        "first frame: {first}"
    );
    assert_continuing(&stream_a);
    assert_capture_matches_truth(&stream_a, &truth_a, "stream a");
    assert_witnesses(&stream_a);

    // The restart witness: the same installation relaunches, and a second
    // journey replays the first journey's coordinates identically before it
    // appends the new turn's rows. The second stream opens while the
    // restarted product is idle, so its replay is pure and the boundary
    // that ends it belongs to the second turn alone.
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let (stream, observed) = capture_stream(address, "");
    let prompt =
        r#"{"id":"msg_capture_replay_2","text":"hello restart","delivery":"queue","resume":true}"#;
    let (status, body) = request(address, "POST", &prompt_path, prompt);
    assert_eq!(
        status,
        200,
        "restart prompt: {}",
        String::from_utf8_lossy(&body)
    );
    let stream_b = collect_successes(stream, observed, 2);
    child.stop_after_witness(&root);

    let truth_b = durable_rows(&root.join("database.sqlite3"));
    assert!(
        truth_b.len() > truth_a.len(),
        "restart journey committed no new rows: {} then {}",
        truth_a.len(),
        truth_b.len()
    );
    assert_continuing(&stream_b);
    assert_capture_matches_truth(&stream_b, &truth_b, "stream b");
    let replayed = coordinates(&stream_a);
    let resumed = coordinates(&stream_b);
    assert!(
        resumed.starts_with(&replayed),
        "restart replay diverged from the original journey"
    );
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn capture_replay_crosses_live_and_resumes_exactly_once() {
    let _journey = process_journey_custody();
    let root = root("capture-cross");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    // Turn one holds its provider response between the two latches, so the
    // first stream opens mid-turn on a deterministic page: everything the
    // step committed is replay, everything the turn still owes is live.
    // The second turn runs normally on the resumed stream.
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let mut scripts = vec![Script::HeldOutput {
        bytes: 1_024,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }];
    scripts.extend((0..6).map(|_| Script::Success));
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let prompt_path = format!("/api/session/{session}/prompt");

    let prompt =
        r#"{"id":"msg_capture_cross_1","text":"first turn","delivery":"queue","resume":true}"#;
    let (status, body) = request(address, "POST", &prompt_path, prompt);
    assert_eq!(
        status,
        200,
        "prompt one: {}",
        String::from_utf8_lossy(&body)
    );
    entered.wait();
    let (stream, observed) = capture_stream(address, "");
    release.wait();
    // Small ordinary turns reach the protocol's periodic live checkpoint
    // without causing a public-frame refusal or changing the cursor format.
    for ordinal in 1..6 {
        wait_capture_idle(address, &session);
        let body = serde_json::json!({"id":format!("msg_capture_checkpoint_{ordinal}"),
            "text":"checkpoint turn","delivery":"queue","resume":true});
        json(address, "POST", &prompt_path, &body.to_string());
    }
    wait_capture_idle(address, &session);
    let stream_1 = collect(stream, observed, "capture.cursor");
    // A read can carry frames past the checkpoint. Resume from exactly the
    // prefix the token covers, rather than claiming its tail was checkpointed.
    let marker = stream_1
        .find("\"type\":\"capture.cursor\"")
        .unwrap_or_else(|| panic!("capture carried no checkpoint"));
    let end = marker
        + stream_1[marker..]
            .find("\n\n")
            .unwrap_or_else(|| panic!("checkpoint frame incomplete"))
        + 2;
    let stream_1 = &stream_1[..end];
    assert_continuing(stream_1);
    let checkpoint = data_frames(stream_1)
        .into_iter()
        .find(|frame| frame["type"] == "capture.cursor")
        .unwrap_or_else(|| panic!("capture checkpoint missing"));
    let cursor_1 = checkpoint["cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("checkpoint carried no cursor"));
    let rows_1 = capture_rows(stream_1);
    assert!(
        rows_1.iter().any(|row| row.frame == FrameKind::Event),
        "stream one carried no projected rows: {rows_1:?}"
    );

    // The first execution finishes after its live checkpoint. Its remaining
    // rows are owed on resume alongside the second execution's new rows.
    let (stream, observed) =
        capture_stream(address, &format!("cursor={}", percent_encode(cursor_1)));
    let prompt =
        r#"{"id":"msg_capture_cross_2","text":"second turn","delivery":"queue","resume":true}"#;
    let (status, body) = request(address, "POST", &prompt_path, prompt);
    assert_eq!(
        status,
        200,
        "prompt two: {}",
        String::from_utf8_lossy(&body)
    );
    let completed = rows_1
        .iter()
        .filter(|row| row.kind == "session.execution.succeeded")
        .count();
    let stream_2 = collect_successes(stream, observed, 7 - completed);
    assert_continuing(&stream_2);
    let rows_2 = capture_rows(&stream_2);
    assert!(!rows_2.is_empty(), "the resumed stream carried no rows");

    child.stop_after_witness(&root);
    let truth = durable_rows(&root.join("database.sqlite3"));

    let held_1: HashSet<(String, u64)> = rows_1
        .iter()
        .map(|row| (row.aggregate.clone(), row.seq))
        .collect();
    let held_2: HashSet<(String, u64)> = rows_2
        .iter()
        .map(|row| (row.aggregate.clone(), row.seq))
        .collect();
    for row in &rows_2 {
        assert!(
            !held_1.contains(&(row.aggregate.clone(), row.seq)),
            "the resumed stream re-presented a row the first stream already carried: ({}, {})",
            row.aggregate,
            row.seq
        );
    }
    let union: HashSet<(String, u64)> = held_1
        .iter()
        .cloned()
        .chain(held_2.iter().cloned())
        .collect();
    let store: HashSet<(String, u64)> = truth
        .iter()
        .map(|(aggregate, seq, _)| (aggregate.clone(), *seq))
        .collect();
    assert_eq!(
        union,
        store,
        "the two streams' union differs from the durable ground truth: \
         union={} store={}",
        union.len(),
        store.len()
    );
    assert_rows_match_truth(&capture_rows(stream_1), &truth, "stream one rows");
    assert_rows_match_truth(&capture_rows(&stream_2), &truth, "stream two rows");
}

#[test]
fn capture_location_interest_presents_only_its_own_location() {
    let _journey = process_journey_custody();
    let root = root("capture-location");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    // Two sibling locations inside the fixture's own Git boundary. Both
    // directories must exist before their sessions can be created, and the
    // location filter below matches the session-list projection's stored
    // directory, not the event stream itself.
    let location_a = root.join("loc-a");
    let location_b = root.join("loc-b");
    fs::create_dir_all(&location_a)
        .unwrap_or_else(|error| panic!("location a creation failed: {error}"));
    fs::create_dir_all(&location_b)
        .unwrap_or_else(|error| panic!("location b creation failed: {error}"));
    // Session B holds its provider response between the two latches, so its
    // early step rows commit while the location stream is live. Session A
    // runs normally, and its terminal settles the stream.
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::HeldOutput {
            bytes: 1_024,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);

    let (session_a, directory_a) = create_at(address, &location_a);
    let (session_b, _) = create_at(address, &location_b);

    // The stream opens into the location page and joins live: A's creation
    // row is replay, everything the turns still owe arrives under the join.
    let (stream, observed) = capture_stream(
        address,
        &format!("location[directory]={}", percent_encode(&directory_a)),
    );

    let prompt =
        r#"{"id":"msg_capture_loc_b","text":"held turn","delivery":"queue","resume":true}"#;
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session_b}/prompt"),
        prompt,
    );
    assert_eq!(status, 200, "prompt b: {}", String::from_utf8_lossy(&body));
    entered.wait();
    // B's step rows are now durable while the stream is subscribed: the
    // live-join window in which the location filter must keep them out.
    let prompt =
        r#"{"id":"msg_capture_loc_a","text":"closing turn","delivery":"queue","resume":true}"#;
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session_a}/prompt"),
        prompt,
    );
    assert_eq!(status, 200, "prompt a: {}", String::from_utf8_lossy(&body));
    let stream_a = collect(stream, observed, "session.execution.succeeded");
    release.wait();
    // A's terminal reached the continuing stream; B now completes and its remaining
    // rows commit, so the ground truth below spans both locations.
    child.stop_after_witness(&root);

    let truth = durable_rows(&root.join("database.sqlite3"));
    let truth_a: Vec<(String, u64, String)> = truth
        .iter()
        .filter(|(aggregate, _, _)| *aggregate == session_a)
        .cloned()
        .collect();
    let truth_b: usize = truth
        .iter()
        .filter(|(aggregate, _, _)| *aggregate == session_b)
        .count();
    assert!(
        !truth_a.is_empty(),
        "the journey committed no durable rows for its own location"
    );
    assert!(
        truth_b >= 6,
        "the held session committed no early rows under the live join: {truth_b}"
    );
    let first = first_frame(&stream_a);
    assert_eq!(
        first["interest"]["directory"].as_str(),
        Some(directory_a.as_str()),
        "interest echo: {first}"
    );
    assert_continuing(&stream_a);
    let rows = capture_rows(&stream_a);
    assert!(!rows.is_empty(), "the location stream carried no rows");
    for row in &rows {
        assert_eq!(
            row.aggregate, session_a,
            "presented a foreign-location row: ({}, {})",
            row.aggregate, row.seq
        );
    }
    assert_capture_matches_truth(&stream_a, &truth_a, "location stream");
    assert_witnesses(&stream_a);
}

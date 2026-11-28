#![cfg(feature = "loopback-provider-testkit")]

#[path = "dogfood_headless/ask_user.rs"]
mod ask_user;

#[cfg(windows)]
#[path = "dogfood_headless/resident_console.rs"]
mod resident_console;

#[path = "dogfood_headless/agent_definitions.rs"]
mod agent_definitions;

#[path = "dogfood_headless/dispatch_shadow.rs"]
mod dispatch_shadow;

#[path = "dogfood_headless/external_host.rs"]
mod external_host;

#[path = "dogfood_headless/hook_import.rs"]
mod hook_import;
#[path = "dogfood_headless/mcp_configuration.rs"]
mod mcp_configuration;

#[path = "dogfood_headless/mcp_configuration_list.rs"]
mod mcp_configuration_list;

#[path = "dogfood_headless/mcp_roots.rs"]
mod mcp_roots;

#[path = "dogfood_headless/mcp_subscriptions.rs"]
mod mcp_subscriptions;

#[path = "dogfood_headless/git_metadata_msys.rs"]
mod git_metadata_msys;
#[path = "dogfood_headless/lifecycle_hooks.rs"]
mod lifecycle_hooks;
#[path = "support/shipped_cli.rs"]
mod shipped_cli;

#[path = "support/catalog_boundary.rs"]
mod catalog_boundary;

#[path = "dogfood_headless/background_session.rs"]
mod background_session;

#[path = "dogfood_headless/session_stop.rs"]
mod session_stop;

#[path = "dogfood_headless/close_grace.rs"]
mod close_grace;

#[path = "dogfood_headless/credential_recovery.rs"]
mod credential_recovery;

#[path = "dogfood_headless/provider_boundaries/mod.rs"]
mod provider_boundaries;

#[path = "dogfood_headless/compaction_recovery.rs"]
mod compaction_recovery;

#[path = "dogfood_headless/bypass_preset.rs"]
mod bypass_preset;
#[path = "dogfood_headless/file_undo.rs"]
mod file_undo;
#[path = "dogfood_headless/rewind.rs"]
mod rewind;
#[path = "dogfood_headless/tui_busy_seat.rs"]
mod tui_busy_seat;
#[path = "dogfood_headless/unknown_effect.rs"]
mod unknown_effect;

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

#[path = "support/poll_deadline.rs"]
mod poll_deadline;
use poll_deadline::PollDeadline;

#[path = "dogfood_headless/polling.rs"]
mod polling;
use polling::wait_calls;

use grapt_artifact::{ArtifactStore, FilesystemArtifactStore, ReadBudget};
use grapt_store::{
    apply_migrations, AggregateId, EventStore, MigrationOutcome, OsRandomIds, ProductionMigrations,
    SystemClock,
};
use grapt_types::{DeterministicAuthorities, ExecutionId, SessionId};

const AUTHORIZATION: &str = "Basic YWxwaGE6emVwaHly";
// The product's bounded continuation can legitimately exceed 64 KiB before
// its 4 MiB context wall. Keep the loopback transport bounded while allowing
// structural-bound journeys to reach the product admission owner.
const PROVIDER_HEADER_BYTES: usize = 64 * 1_024;
// A request just under the 4 MiB request byte bound, plus its JSON framing,
// must reach the scripted provider.
const PROVIDER_BODY_BYTES: usize = 6 * 1_024 * 1_024;
const PROVIDER_REQUEST_BYTES: usize = 6 * 1_024 * 1_024;

struct BarrierState {
    arrived: usize,
    generation: usize,
}

/// Reusable test barrier with a monotonic progress allowance.
///
/// Process fixtures deliberately hold provider responses open. A missing
/// admission must fail with its observed arrival count instead of retaining a
/// carrier until an outer route timeout.
struct Barrier {
    parties: usize,
    allowance: Duration,
    state: Mutex<BarrierState>,
    wake: Condvar,
}

impl Barrier {
    fn new(parties: usize) -> Self {
        Self::within(parties, Duration::from_secs(60))
    }

    fn within(parties: usize, allowance: Duration) -> Self {
        assert!(parties > 0, "test barrier requires at least one party");
        Self {
            parties,
            allowance,
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
        let wait = PollDeadline::within(self.allowance);
        while wait.pending() {
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
            "test barrier generation {generation} exceeded its deadline ({wait}); observed {} of {} parties",
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

#[path = "dogfood_headless/billable.rs"]
mod billable;

#[cfg(unix)]
#[path = "dogfood_headless/step_input.rs"]
mod step_input;

#[path = "dogfood_headless/provider_fixture.rs"]
mod provider_fixture;
#[path = "dogfood_headless/provider_request_rejection.rs"]
mod provider_request_rejection;
use provider_fixture::*;
#[path = "dogfood_headless/provider_profile.rs"]
mod provider_profile;
#[path = "dogfood_headless/restart_resume.rs"]
mod restart_resume;
#[path = "dogfood_headless/stream_admission.rs"]
mod stream_admission;
#[path = "dogfood_headless/terminal_lifetime.rs"]
mod terminal_lifetime;
use provider_profile::*;

#[path = "dogfood_headless/permission_committed_view.rs"]
mod permission_committed_view;
#[path = "dogfood_headless/permission_preview.rs"]
mod permission_preview;
#[path = "dogfood_headless/process_fixture.rs"]
mod process_fixture;
use process_fixture::{await_ready, spawn, spawn_with};

#[path = "dogfood_headless/http.rs"]
mod http;
use http::{json, json_with_read_timeout, percent_encode, request, request_with_read_timeout};

/// The exact `read` output for a small file holding `text`: its numbered
/// lines and the sha256 trailer `patch` uses.
fn numbered_read(text: &str) -> String {
    let mut rendered: String = text
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{}: {line}\n", index + 1))
        .collect();
    let sha: String = ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    rendered.push_str(&format!("[sha256 for patch: {sha}]\n"));
    rendered
}

/// The exact `read` output for a [`text_lines`] fixture.
fn numbered_text(byte: u8, total: usize) -> String {
    numbered_read(
        &String::from_utf8(text_lines(byte, total))
            .unwrap_or_else(|error| panic!("fixture text: {error}")),
    )
}

/// The first `units` UTF-16 units of that output, as a bounded `recent`
/// tool result shows it.
fn numbered_prefix(byte: u8, total: usize, units: usize) -> String {
    String::from_utf16(
        &numbered_text(byte, total)
            .encode_utf16()
            .take(units)
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|error| panic!("prefix text: {error}"))
}

/// `total` bytes of `byte` as 99-character lines, each ending in a newline:
/// a fixture's byte size in lines a numbered read shows whole, so a size-tuned
/// journey keeps its bytes instead of meeting the 2000-character line cut.
fn text_lines(byte: u8, total: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(total);
    while bytes.len() < total {
        let line = (total - bytes.len()).min(100);
        bytes.extend(std::iter::repeat_n(byte, line - 1));
        bytes.push(b'\n');
    }
    bytes
}

fn create(address: SocketAddr) -> String {
    json(
        address,
        "POST",
        "/api/session",
        r#"{"agent":"agt_live_headless1"}"#,
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned()
}

fn event_stream(address: SocketAddr) -> (TcpStream, Vec<u8>) {
    let mut stream = TcpStream::connect(address)
        .unwrap_or_else(|error| panic!("event stream connect failed: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap_or_else(|error| panic!("event timeout failed: {error}"));
    stream
        .write_all(format!("GET /api/event HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap_or_else(|error| panic!("event request failed: {error}"));
    let mut observed = Vec::new();
    let mut chunk = [0u8; 4_096];
    for _ in 0..32 {
        let count = stream
            .read(&mut chunk)
            .unwrap_or_else(|error| panic!("event connect read failed: {error}"));
        observed.extend_from_slice(&chunk[..count]);
        if String::from_utf8_lossy(&observed).contains("server.connected") {
            return (stream, observed);
        }
    }
    panic!("event stream carried no server.connected")
}

fn collect(mut stream: TcpStream, mut observed: Vec<u8>, terminal: &str) -> String {
    let mut chunk = [0u8; 8_192];
    for _ in 0..4096 {
        assert!(
            observed.len() <= 4 * 1024 * 1024,
            "event witness exceeded its byte bound"
        );
        if terminal_frame_complete(&observed, terminal) {
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
            Err(error) => panic!("event read failed: {error}"),
        }
    }
    panic!(
        "event stream carried no {terminal}: {}",
        String::from_utf8_lossy(&observed[observed.len().saturating_sub(4096)..])
    )
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

fn durable_seq(observed: &str, kind: &str) -> u64 {
    for line in observed.lines() {
        let Some(encoded) = line.strip_prefix("data: ") else {
            continue;
        };
        let value = match serde_json::from_str::<serde_json::Value>(encoded) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some(kind) {
            continue;
        }
        return value
            .get("durable")
            .and_then(|durable| durable.get("seq"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("{kind} omitted its typed durable sequence: {value}"));
    }
    panic!("event stream omitted {kind}: {observed}")
}

#[test]
fn live_headless_wire_failure_and_resurrection_witnesses() {
    let _journey = process_journey_custody();
    run_live_headless_journey("wire", true);
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn run_live_headless_journey(label: &str, retry_before_restart: bool) {
    let root = root(label);
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![
        Script::Success,
        Script::Success,
        Script::Unauthorized,
        Script::Malformed,
        Script::StallBeforeFirstFragment,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);

    let success = create(address);
    let prompt_path = format!("/api/session/{success}/prompt");
    let messages_path = format!("/api/session/{success}/message?order=asc");
    let prompt =
        r#"{"id":"msg_live_success","text":"hello wire","delivery":"queue","resume":true}"#;
    let (events, connected) = event_stream(address);
    assert!(
        String::from_utf8_lossy(&connected).contains("server.connected"),
        "cursorless SSE never established before the deliberate gap"
    );
    drop(events);
    drop(connected);
    let first = json(address, "POST", &prompt_path, prompt);
    assert_eq!(
        first["data"]["id"], "msg_live_success",
        "stable prompt identity was not preserved"
    );
    permission_residuals::wait_session_idle(address, &success);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "first admitted prompt must reach the provider exactly once"
    );
    if retry_before_restart {
        assert_eq!(json(address, "POST", &prompt_path, prompt), first);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "stable retry repeated provider call"
        );
    }
    let messages = json(address, "GET", &messages_path, "");
    assert!(
        messages.to_string().contains("Because."),
        "durable re-list lost reasoning committed across the SSE gap"
    );
    assert!(
        messages
            .to_string()
            .contains("Hello from the live provider."),
        "durable re-list lost text committed across the SSE gap"
    );
    assert_eq!(messages["data"][0]["type"], "user");
    assert_eq!(messages["data"][0]["id"], "msg_live_success");
    assert_eq!(messages["data"][0]["text"], "hello wire");
    assert_eq!(
        messages["data"][1]["tokens"]["input"], 8.0,
        "public provider input is fresh input, not inclusive input"
    );
    assert_eq!(
        messages["data"][1]["tokens"]["output"], 4.0,
        "durable provider output usage was lost"
    );
    assert_eq!(
        messages["data"][1]["tokens"]["reasoning"], 3.0,
        "durable reported reasoning usage was lost"
    );
    assert_eq!(messages["data"][1]["tokens"]["cache"]["read"], 2.0);
    assert_eq!(messages["data"][1]["tokens"]["cache"]["write"], 1.0);
    assert_eq!(
        messages["data"][1]["observedUsage"],
        serde_json::json!({
            "inputTotal":11,"inputFresh":8,"output":4,"reasoning":3,
            "cacheRead":2,"cacheWrite":1,"total":15
        })
    );
    assert!(
        messages["data"][1]["time"]["created"]
            .as_f64()
            .is_some_and(|created| created > 0.0),
        "assistant message substituted zero for its durable creation time"
    );
    assert_completion_observations(&messages);
    let session = json(address, "GET", &format!("/api/session/{success}"), "");
    assert_eq!(
        session["data"]["tokens"]["input"], 8.0,
        "Session usage keeps the same fresh-input contract as message usage"
    );
    assert_eq!(
        session["data"]["tokens"]["output"], 4.0,
        "durable provider output usage was lost"
    );
    assert_eq!(session["data"]["tokens"]["reasoning"], 3.0);
    assert_eq!(session["data"]["tokens"]["cache"]["read"], 2.0);
    assert_eq!(session["data"]["tokens"]["cache"]["write"], 1.0);

    let live = create(address);
    let live_prompt_path = format!("/api/session/{live}/prompt");
    let live_prompt =
        r#"{"id":"msg_live_events","text":"hello live events","delivery":"queue","resume":true}"#;
    let (events, connected) = event_stream(address);
    let collector = thread::spawn(move || collect(events, connected, "execution.succeeded"));
    let _live_response = json(address, "POST", &live_prompt_path, live_prompt);
    let observed = collector
        .join()
        .unwrap_or_else(|_| panic!("event collector panicked"));
    for kind in [
        "reasoning.started",
        "reasoning.delta",
        "reasoning.ended",
        "text.started",
        "text.delta",
        "text.ended",
        "execution.succeeded",
    ] {
        assert!(
            observed.contains(kind),
            "live SSE omitted {kind}: {observed}"
        );
    }
    assert!(
        !observed.contains("usage.updated"),
        "live SSE invented a zero cost to complete the coupled usage event: {observed}"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    let excluded = create(address);
    let excluded_path = format!("/api/session/{excluded}/prompt");
    let (excluded_status, _) = request(
        address,
        "POST",
        &excluded_path,
        r#"{"id":"msg_live_attachment","text":"no","files":[{"type":"file"}]}"#,
    );
    assert_eq!(
        excluded_status, 400,
        "Dogfood-1 accepted a malformed attachment"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    for (script_id, expected_calls) in [("auth", 3usize), ("malformed", 4usize)] {
        let session = create(address);
        let path = format!("/api/session/{session}/prompt");
        let body = format!(
            r#"{{"id":"msg_live_{script_id}","text":"{script_id}","delivery":"queue","resume":true}}"#
        );
        let (events, connected) = event_stream(address);
        let collector = thread::spawn(move || collect(events, connected, "execution.failed"));
        let (status, response) = request(address, "POST", &path, &body);
        assert_eq!(
            status,
            200,
            "{script_id} failed admission before its execution: {}",
            String::from_utf8_lossy(&response)
        );
        let observed = collector
            .join()
            .unwrap_or_else(|_| panic!("failed execution collector panicked"));
        assert!(
            observed.contains("ExecutionFailed"),
            "provider refusal lost its typed execution failure"
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), expected_calls);
    }

    let interrupted = create(address);
    let interrupt_prompt = format!("/api/session/{interrupted}/prompt");
    let interrupt_path = format!("/api/session/{interrupted}/interrupt");
    let prompt_address = address;
    let prompt_thread = thread::spawn(move || {
        request(
            prompt_address,
            "POST",
            &interrupt_prompt,
            r#"{"id":"msg_live_interrupt","text":"wait","delivery":"queue","resume":true}"#,
        )
    });
    wait_calls(&provider.calls, 5);
    let active = json(address, "GET", "/api/session/active", "");
    let execution = active["data"][&interrupted]["executionID"]
        .as_str()
        .unwrap_or_else(|| panic!("active execution identity was absent"))
        .to_owned();
    assert!(!execution.is_empty());
    let (stale_status, _) = request(
        address,
        "POST",
        &interrupt_path,
        r#"{"executionID":"exe_stale"}"#,
    );
    assert_eq!(
        stale_status, 400,
        "a stale execution identity interrupted the run"
    );
    let (stream, connected) = event_stream(address);
    let interrupt_events =
        thread::spawn(move || collect(stream, connected, "execution.interrupted"));
    let interrupt_body = format!(r#"{{"executionID":"{execution}"}}"#);
    let (interrupt_status, _) = request(address, "POST", &interrupt_path, &interrupt_body);
    assert_eq!(interrupt_status, 204);
    let (prompt_status, prompt_body) = prompt_thread
        .join()
        .unwrap_or_else(|_| panic!("interrupt prompt panicked"));
    assert_eq!(
        prompt_status,
        200,
        "interrupted prompt failed: {}",
        String::from_utf8_lossy(&prompt_body)
    );
    let observed = interrupt_events
        .join()
        .unwrap_or_else(|_| panic!("interrupt collector panicked"));
    assert!(observed.contains("execution.interrupted"));
    assert!(!observed.contains("execution.succeeded"));
    assert!(
        observed.contains(&execution),
        "terminal publication lost the active execution identity"
    );
    assert!(
        json(address, "GET", "/api/session/active", "")["data"][&interrupted].is_null(),
        "settled execution remained publicly active"
    );
    let public_interrupt_seq = durable_seq(&observed, "session.execution.interrupted");
    assert_completion_observations(&json(
        address,
        "GET",
        &format!("/api/session/{interrupted}/message"),
        "",
    ));

    let heads = locked(&provider.heads).clone();
    assert_eq!(heads.len(), 5);
    assert!(
        heads.iter().all(|head| head
            .to_ascii_lowercase()
            .contains("authorization: bearer provider-secret")),
        "live request did not carry the exact platform credential capability"
    );
    assert!(heads.iter().all(|head| head
        .to_ascii_lowercase()
        .contains("chatgpt-account-id: account-a")));
    assert!(heads
        .iter()
        .all(|head| head.starts_with("POST /responses HTTP/1.1")));
    let bodies = locked(&provider.bodies).clone();
    assert!(bodies[0].contains("gpt-live"));
    let first_body = serde_json::from_str::<serde_json::Value>(&bodies[0])
        .unwrap_or_else(|error| panic!("first provider body was not JSON: {error}"));
    let input = first_body["input"]
        .as_array()
        .unwrap_or_else(|| panic!("first provider input was not an array"));
    let user: Vec<_> = input.iter().filter(|item| item["role"] == "user").collect();
    assert_eq!(user.len(), 1, "initial request must carry one user message");
    assert_eq!(
        user[0]["content"], "hello wire",
        "live request content drifted before provider"
    );
    assert!(bodies[1].contains("hello live events"));

    let before = json(address, "GET", &messages_path, "");
    child.stop_after_witness(&root);
    let interrupted_session: SessionId = match interrupted.parse() {
        Ok(session) => session,
        Err(_) => panic!("the interrupted Session identity is valid"),
    };
    let terminal_store = EventStore::open(&root.join("database.sqlite3"), "live-headless1")
        .unwrap_or_else(|error| panic!("terminal witness store reopen failed: {error}"));
    let execution_id = grapt_types::ExecutionId::new(execution.clone())
        .unwrap_or_else(|| panic!("execution id invalid"));
    let observations = grapt_session::execution::read(
        terminal_store.connection(),
        &interrupted_session,
        &execution_id,
    )
    .unwrap_or_else(|error| panic!("execution terminal evidence unreadable: {error}"));
    let committed_interrupt = observations
        .iter()
        .find(|row| row.outcome == Some(grapt_session::execution::Outcome::Interrupted))
        .unwrap_or_else(|| panic!("pre-first-fragment interrupt has no typed execution terminal"));
    assert_eq!(
        public_interrupt_seq, committed_interrupt.position.seq,
        "execution.interrupted must reference its exact durable execution terminal, not admission or Step"
    );
    drop(terminal_store);
    let calls_before_restart = provider.calls.load(Ordering::SeqCst);
    let mut restarted = spawn(&root, provider.address);
    let restarted_address = await_ready(&root, &mut restarted);
    assert_eq!(json(restarted_address, "GET", &messages_path, ""), before);
    let (retry_status, retry_body) = request(restarted_address, "POST", &prompt_path, prompt);
    assert_eq!(
        retry_status,
        200,
        "stable prompt retry did not reconcile across restart: {}",
        String::from_utf8_lossy(&retry_body)
    );
    let retry = serde_json::from_slice::<serde_json::Value>(&retry_body)
        .unwrap_or_else(|error| panic!("restart retry response was not JSON: {error}"));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        calls_before_restart,
        "restart/replay repeated provider call"
    );
    assert_eq!(
        retry, first,
        "stable prompt retry did not reconcile exactly"
    );
    restarted.stop_after_witness(&root);
    drop(restarted);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn headless_real_process_closes_every_journey_law() {
    let _journey = process_journey_custody();
    run_live_headless_journey("journey", false);
}

#[path = "dogfood_headless/cli_continuity.rs"]
mod cli_continuity;

#[path = "dogfood_headless/usage_resilience.rs"]
mod usage_resilience;

#[path = "dogfood_headless/cli_compatibility.rs"]
mod cli_compatibility;

#[path = "dogfood_headless/cli_server_loss.rs"]
mod cli_server_loss;

#[path = "dogfood_headless/fork_execution.rs"]
mod fork_execution;

#[path = "dogfood_headless/startup_sync.rs"]
mod startup_sync;

#[path = "dogfood_headless/compaction_continuity.rs"]
mod compaction_continuity;

#[path = "dogfood_headless/manual_compaction.rs"]
mod manual_compaction;

#[path = "dogfood_headless/over_bound_compaction.rs"]
mod over_bound_compaction;

#[path = "dogfood_headless/declared_crossing.rs"]
mod declared_crossing;
use declared_crossing::*;

#[path = "dogfood_headless/compaction_config.rs"]
mod compaction_config;

#[path = "dogfood_headless/unmetered_compaction.rs"]
mod unmetered_compaction;

#[path = "dogfood_headless/server_compaction.rs"]
mod server_compaction;

#[path = "dogfood_headless/prompt_cache.rs"]
mod prompt_cache;

#[path = "dogfood_headless/all_call_cost.rs"]
mod all_call_cost;
#[path = "dogfood_headless/cache_scope.rs"]
mod cache_scope;
#[path = "dogfood_headless/session_cost.rs"]
mod session_cost;

#[path = "dogfood_headless/compatible_reasoning.rs"]
mod compatible_reasoning;

#[path = "dogfood_headless/compatible_cache.rs"]
mod compatible_cache;

#[path = "dogfood_headless/write_overwrite.rs"]
mod write_overwrite;

#[path = "dogfood_headless/anthropic_epochs.rs"]
mod anthropic_epochs;
#[path = "dogfood_headless/anthropic_thinking.rs"]
mod anthropic_thinking;
#[path = "dogfood_headless/context_quality.rs"]
mod context_quality;

#[path = "dogfood_headless/continuity_journey.rs"]
mod continuity_journey;

#[path = "dogfood_headless/permission_residuals.rs"]
mod permission_residuals;

#[path = "dogfood_headless/rapid_submit.rs"]
mod rapid_submit;

#[path = "dogfood_headless/plugin_tool.rs"]
mod plugin_tool;

#[path = "dogfood_headless/provider_plugin.rs"]
mod provider_plugin;

#[path = "dogfood_headless/output_advisory.rs"]
mod output_advisory;

#[path = "dogfood_headless/acp_lifetime.rs"]
mod acp_lifetime;
#[path = "dogfood_headless/acp_process.rs"]
mod acp_process;

#[path = "dogfood_headless/acp_compaction.rs"]
mod acp_compaction;

#[path = "dogfood_headless/live_event_stream.rs"]
mod live_event_stream;

#[path = "dogfood_headless/acp_publication.rs"]
mod acp_publication;

#[path = "dogfood_headless/project_list.rs"]
mod project_list;

#[path = "dogfood_headless/provider_refusal.rs"]
mod provider_refusal;

#[path = "dogfood_headless/client_allowance.rs"]
mod client_allowance;

// MCP fixtures run `/usr/bin/sleep` and read `/proc`.
#[path = "dogfood_headless/prompt_admission.rs"]
mod prompt_admission;

#[path = "dogfood_headless/session_metadata.rs"]
mod session_metadata;
#[path = "dogfood_headless/step_cap.rs"]
mod step_cap;

#[path = "dogfood_headless/location_discovery.rs"]
mod location_discovery;

#[cfg(target_os = "linux")]
#[path = "dogfood_headless/mcp_notice.rs"]
mod mcp_notice;

#[cfg(target_os = "linux")]
#[path = "dogfood_headless/seat_launch.rs"]
mod seat_launch;
#[cfg(target_os = "linux")]
#[path = "dogfood_headless/stateengine_native.rs"]
mod stateengine_native;

#[cfg(target_os = "linux")]
#[path = "dogfood_headless/preset_launch.rs"]
mod preset_launch;

#[path = "dogfood_headless/permission_presets.rs"]
mod permission_presets;

#[path = "dogfood_headless/permission_boundaries.rs"]
mod permission_boundaries;

#[path = "dogfood_headless/process_loss.rs"]
mod process_loss;

#[path = "dogfood_headless/provider_accounts.rs"]
mod provider_accounts;

#[path = "dogfood_headless/recent_parity.rs"]
mod recent_parity;

#[path = "dogfood_headless/subagent_child.rs"]
mod subagent_child;

#[path = "dogfood_headless/agent_control.rs"]
mod agent_control;
#[path = "dogfood_headless/agent_listing.rs"]
mod agent_listing;
#[path = "dogfood_headless/agent_worktree.rs"]
mod agent_worktree;

#[path = "dogfood_headless/subagent_settlement.rs"]
mod subagent_settlement;

#[path = "dogfood_headless/subagent_continuity.rs"]
mod subagent_continuity;

#[path = "dogfood_headless/subagent_model_routing.rs"]
mod subagent_model_routing;

#[path = "dogfood_headless/skill_activation.rs"]
mod skill_activation;

#[path = "dogfood_headless/first_request_budget.rs"]
mod first_request_budget;

#[path = "dogfood_headless/cache_history.rs"]
mod cache_history;

#[path = "dogfood_headless/workflow_journey.rs"]
mod workflow_journey;

#[path = "dogfood_headless/scoped_instructions.rs"]
mod scoped_instructions;

#[path = "dogfood_headless/sustained_allowance.rs"]
mod sustained_allowance;

#[cfg(not(windows))]
#[path = "dogfood_headless/sustained_recovery.rs"]
mod sustained_recovery;

#[path = "support/completion_observations.rs"]
mod completion_observations;
use completion_observations::assert_completion_observations;

#[path = "dogfood_headless/provider_error_text.rs"]
mod provider_error_text;
#[path = "dogfood_headless/reasoning_retry.rs"]
mod reasoning_retry;
#[path = "dogfood_headless/step_failure_notice.rs"]
mod step_failure_notice;

#[path = "dogfood_headless/native_failures.rs"]
mod native_failures;
#[cfg(target_os = "linux")]
#[path = "dogfood_headless/shell_exit.rs"]
mod shell_exit;

#[path = "dogfood_headless/plan_compaction.rs"]
mod plan_compaction;
#[path = "dogfood_headless/plan_updates.rs"]
mod plan_updates;

#[cfg(target_os = "linux")]
#[path = "dogfood_headless/credential_children.rs"]
mod credential_children;

#[path = "dogfood_headless/plugin_transient.rs"]
mod plugin_transient;

#[path = "dogfood_headless/repeated_keys.rs"]
mod repeated_keys;

#[path = "dogfood_headless/terminal_coordination.rs"]
mod terminal_coordination;

#[path = "dogfood_headless/output_ceiling.rs"]
mod output_ceiling;

#[path = "dogfood_headless/image_view.rs"]
mod image_view;

#[path = "dogfood_headless/apply_patch.rs"]
mod apply_patch;
#[path = "dogfood_headless/effort_updates.rs"]
mod effort_updates;

#[path = "dogfood_headless/reasoning_effort.rs"]
mod reasoning_effort;

#[path = "dogfood_headless/terminal_compaction.rs"]
mod terminal_compaction;
#[cfg(unix)]
#[path = "dogfood_headless/terminal_io.rs"]
mod terminal_io;

#[cfg(any(target_os = "linux", windows))]
#[path = "dogfood_headless/git_metadata.rs"]
mod git_metadata;

#[path = "dogfood_headless/anthropic_cache_policy.rs"]
mod anthropic_cache_policy;
#[path = "dogfood_headless/approval.rs"]
mod approval;
#[path = "dogfood_headless/permission_config.rs"]
mod permission_config;

#[path = "dogfood_headless/message_page_bytes.rs"]
mod message_page_bytes;

#[path = "dogfood_headless/bound_session_tools.rs"]
mod bound_session_tools;
#[path = "dogfood_headless/shell_environment.rs"]
mod shell_environment;
#[path = "dogfood_headless/stateengine_binding.rs"]
mod stateengine_binding;

#[path = "dogfood_headless/local_inventory.rs"]
mod local_inventory;

#[path = "dogfood_headless/anthropic_tool_schemas.rs"]
mod anthropic_tool_schemas;

#[cfg(target_os = "linux")]
#[path = "dogfood_headless/native_boundary.rs"]
mod native_boundary;

#[path = "dogfood_headless/side_question.rs"]
mod side_question;

#[path = "dogfood_headless/diagnostics.rs"]
mod diagnostics;

#[path = "dogfood_headless/formatting.rs"]
mod formatting;

#[path = "dogfood_headless/formatting_interruption.rs"]
mod formatting_interruption;

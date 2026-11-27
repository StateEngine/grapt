//! The live event stream during a multi-tool turn, read the way an ACP client
//! reads it: subscribe, and on every end-of-stream subscribe again. ACP gives
//! up after 32 ends in a row without an event for its Session, so a turn whose
//! stream keeps ending without publishing the Session's progress fails.
#[cfg(unix)]
use super::acp_process::{AcpObserved, AcpProcess, ACP_FRAMES};
use super::*;
#[cfg(unix)]
use std::time::Instant;

/// This fixture's wall-clock deadlines.
#[allow(clippy::disallowed_methods)]
#[cfg(unix)]
fn now() -> Instant {
    Instant::now()
}

/// What one client saw across all its subscriptions.
#[derive(Debug, Default)]
struct Observed {
    /// Ends of stream followed by a new subscription.
    rebinds: usize,
    /// The longest run of rebinds with no event for the Session between them.
    longest_quiet_run: usize,
    /// Event types seen for the Session, in order.
    kinds: Vec<String>,
    /// The product's stream-end diagnostics, one JSON object per line.
    ends: Vec<serde_json::Value>,
}

/// Subscribe and resubscribe until `done` is set, recording the Session's
/// events. Bounded: at most 2048 subscriptions and 8 MiB per subscription.
fn watch(address: SocketAddr, session: String, done: Arc<AtomicBool>) -> Observed {
    let mut observed = Observed::default();
    let mut quiet = 0;
    for _ in 0..2048 {
        if done.load(Ordering::SeqCst) {
            break;
        }
        let (mut stream, mut buffer) = event_stream(address);
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap_or_else(|error| panic!("event timeout failed: {error}"));
        let mut chunk = [0u8; 8192];
        let mut parsed = 0;
        loop {
            // Every complete SSE frame so far.
            while let Some(end) = find(&buffer[parsed..], b"\n\n") {
                let frame = String::from_utf8_lossy(&buffer[parsed..parsed + end]).into_owned();
                parsed += end + 2;
                let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };

                let mine = event["data"]["sessionID"] == session.as_str();
                if mine {
                    quiet = 0;
                    observed
                        .kinds
                        .push(event["type"].as_str().unwrap_or_default().to_owned());
                }
            }
            if done.load(Ordering::SeqCst) || buffer.len() > 8 * 1024 * 1024 {
                return observed;
            }
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
        observed.rebinds += 1;
        quiet += 1;
        observed.longest_quiet_run = observed.longest_quiet_run.max(quiet);
    }
    observed
}

/// The product states why it ends streams; "unstated" is the hub's default
/// for a caller that did not.
fn assert_every_end_is_explained(observed: &Observed) {
    assert!(
        observed.ends.iter().all(|end| end["reason"]
            .as_str()
            .is_some_and(|reason| reason != "unstated")),
        "{:?}",
        observed.ends
    );
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// One response that opens with a reasoning item, as current OpenAI models
/// stream before acting, and then calls `read`.
fn reasoning_then_read(call: &str, path: &str) -> String {
    let arguments = serde_json::json!({"path": path}).to_string();
    let arguments = serde_json::to_string(&arguments)
        .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"));
    format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"reasoning\",\"id\":\"rs_{call}\",\"summary\":[]}}}}\n\n\
         data: {{\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_{call}\",\"summary_index\":0,\"delta\":\"Looking at the file.\"}}\n\n\
         data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"reasoning\",\"id\":\"rs_{call}\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"Looking at the file.\"}}],\"encrypted_content\":\"opaque\"}}}}\n\n\
         data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"read\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{arguments}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n"
    )
}

fn multi_tool_turn(case: &str, scripts: Vec<Script>, calls: &[&str]) -> Observed {
    multi_tool_turn_with(case, scripts, calls, &[])
}

fn multi_tool_turn_with(
    case: &str,
    scripts: Vec<Script>,
    calls: &[&str],
    files: &[(&str, &str)],
) -> Observed {
    let _journey = process_journey_custody();
    let root = root(case);
    prepare(&root, 200_000);
    fs::write(root.join("notes.txt"), "the answer is 42\n")
        .unwrap_or_else(|error| panic!("fixture file: {error}"));
    for (name, contents) in files {
        fs::write(root.join(name), contents)
            .unwrap_or_else(|error| panic!("fixture file: {error}"));
    }
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (session, done) = (session.clone(), Arc::clone(&done));
        thread::spawn(move || watch(address, session, done))
    };
    let body = serde_json::json!({"id": format!("msg_{}", case.replace('-', "_")), "text": "Read notes.txt a few times.", "delivery": "queue", "resume": true});
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &body.to_string(),
    );
    for call in calls {
        super::compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    done.store(true, Ordering::SeqCst);
    let mut observed = watcher
        .join()
        .unwrap_or_else(|_| panic!("event watcher panicked"));
    child.stop_after_witness(&root);
    observed.ends = fs::read_to_string(root.join("diagnostics/event-stream.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("bad diagnostic line {error}: {line}"))
        })
        .collect();
    drop(child);
    process_fixture::cleanup(&root);
    observed
}

const CALLS: [&str; 4] = ["call_live_1", "call_live_2", "call_live_3", "call_live_4"];

#[test]
fn a_plain_multi_tool_turn_publishes_its_tools_live() {
    let mut scripts: Vec<Script> = CALLS
        .iter()
        .map(|call| Script::ToolRead {
            call,
            path: "notes.txt",
        })
        .collect();
    scripts.push(Script::Success);
    let observed = multi_tool_turn("live-plain", scripts, &CALLS);
    eprintln!("plain: {observed:?}");
    assert_published(&observed, CALLS.len());
}

#[test]
fn a_reasoning_multi_tool_turn_publishes_its_tools_live() {
    let mut scripts: Vec<Script> = CALLS
        .iter()
        .map(|call| Script::Raw(reasoning_then_read(call, "notes.txt")))
        .collect();
    scripts.push(Script::Success);
    let observed = multi_tool_turn("live-reasoning", scripts, &CALLS);
    eprintln!("reasoning: {observed:?}");
    assert_published(&observed, CALLS.len());
}

/// One response that reasons, then calls `read` twice, as the benchmark's
/// model did (9 tool calls across 7 assistant messages).
fn reasoning_then_reads(calls: &[&str], path: &str) -> String {
    let arguments = serde_json::json!({"path": path}).to_string();
    let arguments = serde_json::to_string(&arguments)
        .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"));
    let first = calls[0];
    let mut body = format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"reasoning\",\"id\":\"rs_{first}\",\"summary\":[]}}}}\n\n\
         data: {{\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_{first}\",\"summary_index\":0,\"delta\":\"Reading.\"}}\n\n\
         data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"reasoning\",\"id\":\"rs_{first}\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"Reading.\"}}],\"encrypted_content\":\"opaque\"}}}}\n\n"
    );
    for call in calls {
        body.push_str(&format!(
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"read\"}}}}\n\n\
             data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{arguments}}}\n\n\
             data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n"
        ));
    }
    body.push_str("data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n");
    body
}

const PAIRS: [[&str; 2]; 3] = [
    ["call_pair_1a", "call_pair_1b"],
    ["call_pair_2a", "call_pair_2b"],
    ["call_pair_3a", "call_pair_3b"],
];

fn assert_published(observed: &Observed, calls: usize) {
    let started = observed
        .kinds
        .iter()
        .filter(|kind| kind.as_str() == "session.tool.input.started")
        .count();
    assert_eq!(started, calls, "{observed:?}");
    assert!(observed.longest_quiet_run < 32, "{observed:?}");
}

#[test]
fn a_turn_with_two_tool_calls_per_response_publishes_its_tools_live() {
    let mut scripts: Vec<Script> = PAIRS
        .iter()
        .map(|pair| Script::ToolReads {
            calls: pair.iter().map(|call| (*call, "notes.txt")).collect(),
        })
        .collect();
    scripts.push(Script::Success);
    let calls: Vec<&str> = PAIRS.iter().flatten().copied().collect();
    let observed = multi_tool_turn("live-pairs", scripts, &calls);
    eprintln!("pairs: {observed:?}");
    assert_published(&observed, calls.len());
}

#[test]
fn a_reasoning_turn_with_two_tool_calls_per_response_publishes_its_tools_live() {
    let mut scripts: Vec<Script> = PAIRS
        .iter()
        .map(|pair| Script::Raw(reasoning_then_reads(pair, "notes.txt")))
        .collect();
    scripts.push(Script::Success);
    let calls: Vec<&str> = PAIRS.iter().flatten().copied().collect();
    let observed = multi_tool_turn("live-reasoning-pairs", scripts, &calls);
    eprintln!("reasoning pairs: {observed:?}");
    assert_published(&observed, calls.len());
}

/// Drive one prompt through the shipped `grapt acp --embedded` against
/// `scripts`, within `allowance` from start to response.
#[cfg(unix)]
fn acp_turn_within(
    case: &str,
    scripts: Vec<Script>,
    allowance: Duration,
    frame_cap: usize,
) -> AcpObserved {
    acp_turn_cut(case, scripts, allowance, frame_cap, |_| 0, None)
}

/// An ACP turn whose product may be killed mid-request by its own cut.
/// `after_cut` runs once the turn is cleaned up, against the provider's
/// address, and returns how many truncated requests it sent; each is awaited
/// within a bound and reported with any the cut itself left.
#[cfg(unix)]
fn acp_turn_cut(
    case: &str,
    scripts: Vec<Script>,
    allowance: Duration,
    frame_cap: usize,
    after_cut: impl FnOnce(std::net::SocketAddr) -> usize,
    prompt_ready: Option<&Barrier>,
) -> AcpObserved {
    let _journey = process_journey_custody();
    let root = root(case);
    prepare(&root, 200_000);
    fs::write(root.join("notes.txt"), "the answer is 42\n")
        .unwrap_or_else(|error| panic!("fixture file: {error}"));
    let provider = ProviderFixture::start_tolerating_cuts(scripts);
    let mut started = now();
    // A held-provider deadline witnesses a running request, not cold startup.
    // Startup and the provider rendezvous retain their own finite bounds.
    let deadline = started
        + if prompt_ready.is_some() {
            Duration::from_secs(120)
        } else {
            allowance
        };
    let mut acp = AcpProcess::start(&root, provider.address, frame_cap);
    let mut observed = AcpObserved::default();
    let outcome = (|| {
        acp.request(
            1,
            "initialize",
            serde_json::json!({"protocolVersion": 1, "clientCapabilities": {}}),
            deadline,
            &mut observed,
        )?;
        let session = acp.request(
            2,
            "session/new",
            serde_json::json!({"cwd": root, "mcpServers": []}),
            deadline,
            &mut observed,
        )?;
        let session = session["sessionId"]
            .as_str()
            .ok_or("no sessionId")?
            .to_owned();
        let params = serde_json::json!({"sessionId": session, "prompt": [{"type": "text", "text": "Read notes.txt a few times."}]});
        let result = if let Some(ready) = prompt_ready {
            acp.send(serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": params}), deadline)?;
            ready.wait();
            started = now();
            acp.response_observing(
                3,
                "session/prompt",
                started + allowance,
                &mut observed,
                |_| {},
            )?
        } else {
            acp.request(3, "session/prompt", params, deadline, &mut observed)?
        };
        Ok::<String, String>(result["stopReason"].as_str().unwrap_or_default().to_owned())
    })();
    observed.outcome = match outcome {
        Ok(stop) => format!("stop:{stop}"),
        Err(error) => format!("error:{error}"),
    };
    acp.cleanup();
    observed.cleaned_up = acp.cleaned_up;
    observed.elapsed = started.elapsed();
    drop(acp);
    let injected = after_cut(provider.address);
    let settled = now() + Duration::from_secs(5);
    while locked(&provider.truncated).len() < injected && now() < settled {
        thread::sleep(Duration::from_millis(10));
    }
    observed.truncated_requests = locked(&provider.truncated).clone();
    drop(provider);
    process_fixture::cleanup(&root);
    observed
}

#[cfg(unix)]
fn summarize(observed: &AcpObserved) -> String {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for update in &observed.updates {
        *counts.entry(update.as_str()).or_default() += 1;
    }
    format!(
        "outcome={} permissions={} cleaned_up={} elapsed={:?} updates={counts:?}",
        observed.outcome, observed.permissions, observed.cleaned_up, observed.elapsed
    )
}

#[cfg(unix)]
#[test]
fn an_acp_multi_tool_turn_streams_its_tools_and_ends_its_turn() {
    let mut scripts: Vec<Script> = PAIRS
        .iter()
        .map(|pair| Script::Raw(reasoning_then_reads(pair, "notes.txt")))
        .collect();
    scripts.push(Script::Success);
    let observed = acp_turn_within("acp-live", scripts, Duration::from_secs(120), ACP_FRAMES);
    eprintln!("acp: {}", summarize(&observed));
    assert_eq!(
        observed.outcome,
        "stop:end_turn",
        "{}",
        summarize(&observed)
    );
    let tool_calls = observed
        .updates
        .iter()
        .filter(|update| update.as_str() == "tool_call")
        .count();
    assert_eq!(tool_calls, 6, "{}", summarize(&observed));
    assert!(observed.cleaned_up, "{}", summarize(&observed));
}

#[cfg(unix)]
#[test]
fn an_acp_turn_past_its_deadline_is_cut_and_leaves_no_process_behind() {
    // The provider never answers, so the prompt never completes: the turn
    // must end at its absolute deadline and take the embedded product with it.
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let scripts = vec![Script::HeldOutput {
        bytes: 16,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }];
    let observed = acp_turn_cut(
        "acp-deadline",
        scripts,
        Duration::from_secs(3),
        ACP_FRAMES,
        |_| 0,
        Some(&entered),
    );
    let _ = release.wait();
    assert!(
        observed.outcome.contains("deadline passed"),
        "{}",
        summarize(&observed)
    );
    assert!(
        observed.elapsed < Duration::from_secs(20),
        "{}",
        summarize(&observed)
    );
    assert!(observed.cleaned_up, "{}", summarize(&observed));
}

#[test]
fn every_stream_end_is_recorded_with_its_cause_and_reason() {
    // Ordinary completion leaves the live feed open.
    let mut scripts: Vec<Script> = PAIRS
        .iter()
        .map(|pair| Script::Raw(reasoning_then_reads(pair, "notes.txt")))
        .collect();
    scripts.push(Script::Success);
    let calls: Vec<&str> = PAIRS.iter().flatten().copied().collect();
    let observed = multi_tool_turn("live-diagnosed", scripts, &calls);
    assert_published(&observed, calls.len());
    assert_every_end_is_explained(&observed);
    assert!(
        observed.ends.is_empty(),
        "normal completion ended streams: {:?}",
        observed.ends
    );
    assert_eq!(observed.rebinds, 0, "normal completion forced a rebind");
}

#[test]
fn a_publication_refusal_is_recorded_as_the_reason_streams_ended() {
    // A real publication refusal: model text larger than one public frame.
    // Clients rebind and hydrate; the diagnostics must name the refused
    // publication and its reason instead of the reason being discarded.
    let scripts = vec![Script::Output(300_000)];
    let observed = multi_tool_turn("live-refused", scripts, &[]);
    eprintln!("refused: {:?}", observed.ends);
    assert_every_end_is_explained(&observed);
    assert!(
        observed.ends.iter().any(|end| end["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("publication refused"))),
        "{:?}",
        observed.ends
    );
}

#[cfg(unix)]
#[test]
fn an_acp_turn_that_floods_past_its_frame_bound_is_cut_and_leaves_no_process_behind() {
    // A normal turn produces far more than 8 frames; the bound must end it
    // and cleanup must still take the embedded product with it.
    let mut scripts: Vec<Script> = PAIRS
        .iter()
        .map(|pair| Script::Raw(reasoning_then_reads(pair, "notes.txt")))
        .collect();
    scripts.push(Script::Success);
    let observed = acp_turn_within("acp-flood", scripts, Duration::from_secs(120), 8);
    assert!(
        observed.outcome.contains("frame count bound exceeded"),
        "{}",
        summarize(&observed)
    );
    assert!(observed.cleaned_up, "{}", summarize(&observed));
}

/// SC-TEST-01: the product a cut kills may leave a provider request
/// unfinished. That is the cut's expected outcome, recorded, never a fixture
/// failure; the turn is still proven cut at its frame bound and cleaned up.
#[cfg(unix)]
#[test]
fn a_provider_request_left_unfinished_by_the_cut_is_recorded_not_a_fixture_failure() {
    let mut scripts: Vec<Script> = PAIRS
        .iter()
        .map(|pair| Script::Raw(reasoning_then_reads(pair, "notes.txt")))
        .collect();
    scripts.push(Script::Success);
    let observed = acp_turn_cut(
        "acp-flood-truncated",
        scripts,
        Duration::from_secs(120),
        8,
        |address| {
            // The request a killed product leaves behind: a head promising a
            // body, part of it, then the connection's end.
            let mut stream = std::net::TcpStream::connect(address)
                .unwrap_or_else(|error| panic!("truncated request connect: {error}"));
            stream
                .write_all(b"POST /responses HTTP/1.1\r\nContent-Length: 4096\r\n\r\n{\"input\":")
                .unwrap_or_else(|error| panic!("truncated request write: {error}"));
            1
        },
        None,
    );
    assert!(
        observed.outcome.contains("frame count bound exceeded"),
        "{}",
        summarize(&observed)
    );
    assert!(observed.cleaned_up, "{}", summarize(&observed));
    assert!(
        observed
            .truncated_requests
            .iter()
            .any(|why| why.contains("before its bounded body")),
        "{:?}",
        observed.truncated_requests
    );
}

//! Committed read results appear while the next model request remains in flight.
use super::*;
use grapt_client::transport::{FramePoll, WireResponse};

const MARKER: &str = "Completed read appears without reloading history.";
static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static CONTINUATION_DONE: AtomicBool = AtomicBool::new(false);
static CONTINUATION_STARTED: AtomicBool = AtomicBool::new(false);
/// Set by the test once it has observed what it needs from the held
/// continuation. The provider waits for this instead of sleeping, so pacing is
/// the test's observation rather than a fixed delay.
static RELEASE: AtomicBool = AtomicBool::new(false);
/// Set by the provider when the release never arrived inside the bound.
static RELEASE_FAILED: AtomicBool = AtomicBool::new(false);
/// How long the provider waits for the release, counted in polls because a test
/// site may not read the clock. 2_000 polls of 5 ms is ten seconds.
const RELEASE_POLLS: u32 = 2_000;
/// Poll interval while waiting for the release.
const RELEASE_POLL: Duration = Duration::from_millis(5);

/// Release the held continuation, so the provider may finish.
fn release_continuation() {
    RELEASE.store(true, Ordering::SeqCst);
}

/// Releases the continuation on any exit from a case, including a panicking
/// assertion, so teardown still drains the provider.
struct ReleaseOnDrop;
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        release_continuation();
    }
}

fn read_then_wait(stream: &mut TcpStream, _prompt: &str) {
    if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
        let arguments = serde_json::json!({"path":"tool-event-read.txt"});
        let events = [
            serde_json::json!({"type":"response.output_item.added","item":{"type":"function_call","id":"item_tool_read","call_id":"call_tool_read","name":"read"}}),
            serde_json::json!({"type":"response.function_call_arguments.delta","item_id":"item_tool_read","delta":arguments.to_string()}),
            serde_json::json!({"type":"response.function_call_arguments.done","item_id":"item_tool_read"}),
            serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":11,"output_tokens":4,"total_tokens":15}}}),
        ];
        let body = events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>();
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        stream
            .write_all(response.as_bytes())
            .unwrap_or_else(|error| panic!("read provider response: {error}"));
    } else {
        CONTINUATION_STARTED.store(true, Ordering::SeqCst);
        eprintln!("TOOL_EVENTS CONTINUATION_BEGIN");
        // Bounded wait for the test's release. A release that never arrives is
        // a drain failure the judging test asserts on, not a worker left
        // hanging and not a silent timeout. Counted in polls, because no test
        // site may read the clock.
        let mut waited = 0u32;
        while !RELEASE.load(Ordering::SeqCst) {
            if waited >= RELEASE_POLLS {
                RELEASE_FAILED.store(true, Ordering::SeqCst);
                eprintln!("TOOL_EVENTS RELEASE_DRAIN_FAILED allowance elapsed");
                return;
            }
            waited += 1;
            thread::sleep(RELEASE_POLL);
        }
        write_provider_success(stream, "The read continuation finishes.");
        CONTINUATION_DONE.store(true, Ordering::SeqCst);
        eprintln!("TOOL_EVENTS CONTINUATION_END");
    }
}

/// The second case consumes but withholds the two opening hints. This isolates
/// the terminal decoder: an opening's history read must not mask a lost terminal.
/// Both cases use the actual HTTP/SSE socket and authoritative message API.
#[derive(Debug)]
struct WitnessConnection {
    http: HttpConnection,
    terminal_only: bool,
    openings_withheld: usize,
    terminals_received: usize,
}
impl Transport for WitnessConnection {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        self.http.call(request)
    }
}
impl Connection for WitnessConnection {
    fn subscribed(&self) -> bool {
        self.http.subscribed()
    }
    fn begin_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        self.http.begin_reconnect()
    }
    fn poll_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        self.http.poll_reconnect()
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.http.begin_read(request)
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        self.http.poll_read()
    }
    fn cancel_read(&mut self) {
        self.http.cancel_read();
    }
    fn begin_tool_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.http.begin_tool_read(request)
    }
    fn poll_tool_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        self.http.poll_tool_read()
    }
    fn cancel_tool_read(&mut self) {
        self.http.cancel_tool_read();
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        let frame = self.http.poll_frame()?;
        // A positions subscription carries the same payloads as positioned frames.
        if let FramePoll::Frame(payload) | FramePoll::Positioned { data: payload, .. } = &frame {
            let value: serde_json::Value = serde_json::from_str(payload)
                .unwrap_or_else(|error| panic!("real event JSON: {error}"));
            match value["type"].as_str() {
                Some("session.tool.input.started" | "session.tool.called")
                    if self.terminal_only =>
                {
                    self.openings_withheld += 1;
                    return Ok(FramePoll::Idle);
                }
                Some("session.tool.success" | "session.tool.failed") => {
                    assert_eq!(value["durable"]["version"], 2, "real terminal version");
                    self.terminals_received += 1;
                }
                _ => {}
            }
        }
        Ok(frame)
    }
    fn resubscribe(&mut self) {
        self.http.resubscribe();
    }
}

/// A selected session on a subscribed real stream, its positions proof
/// settled before the turn so the launch re-read cannot mask the hint the
/// journey measures.
fn mode_proved_client(
    product: &Product,
    address: SocketAddr,
    terminal_only: bool,
) -> (Client<WitnessConnection>, grapt_client::wire::SessionWireId) {
    let peer = WitnessConnection {
        http: HttpConnection::attach(address, credential(), Bounds::PRODUCTION),
        terminal_only,
        openings_withheld: 0,
        terminals_received: 0,
    };
    let mut client = Client::new(peer, Bounds::PRODUCTION);
    select_offer(&mut client, product);
    let session = client
        .create_session()
        .unwrap_or_else(|error| panic!("session: {error}"));
    client.peer_mut().resubscribe();
    assert!(
        client.peer_mut().http.subscribed(),
        "real stream must subscribe"
    );
    prove_positions(&mut client);
    (client, session)
}

#[test]
fn a_read_terminal_refreshes_the_healthy_stream_before_the_next_message() {
    for terminal_only in [false, true] {
        CALLS.store(0, Ordering::SeqCst);
        CONTINUATION_DONE.store(false, Ordering::SeqCst);
        CONTINUATION_STARTED.store(false, Ordering::SeqCst);
        RELEASE.store(false, Ordering::SeqCst);
        RELEASE_FAILED.store(false, Ordering::SeqCst);
        let label = if terminal_only {
            "tool-terminal-only"
        } else {
            "tool-live"
        };
        let (product, address) = serve_with_provider(label, true, read_then_wait);
        // The release owner is created AFTER the product, so on an early-unwind
        // it is dropped FIRST. `Product::drop` kills and reaps the child, and
        // `ProviderFixture::drop` then joins the provider thread; if the guard
        // were declared earlier it would drop after that join and the provider
        // would sit out the whole bounded wait instead of being released
        // promptly.
        let _released = ReleaseOnDrop;
        fs::write(product.launch.join("tool-event-read.txt"), MARKER)
            .unwrap_or_else(|error| panic!("read fixture: {error}"));
        let (mut client, session) = mode_proved_client(&product, address, terminal_only);
        client
            .submit_prompt("Read tool-event-read.txt and continue.")
            .unwrap_or_else(|error| panic!("prompt: {error}"));
        let mut raw = HttpConnection::attach(address, credential(), Bounds::PRODUCTION);
        let request = WireRequest::new(Operation::MessageList, Some(&session), None)
            .unwrap_or_else(|error| panic!("message request: {error}"));
        let mut committed = false;
        for _ in 0..JOURNEY_POLLS {
            let response = raw
                .call(&request)
                .unwrap_or_else(|error| panic!("durable messages: {error}"));
            if String::from_utf8_lossy(&response.body).contains(MARKER) {
                committed = true;
                break;
            }
            thread::sleep(JOURNEY_SLEEP);
        }
        assert!(committed, "read must actually commit");
        // Establish the in-flight continuation before inspecting the TUI.
        // Otherwise early display can tear down a request still being sent,
        // making the provider's strict body reader fail during cleanup.
        assert!(
            wait_for(|| CONTINUATION_STARTED.load(Ordering::SeqCst)),
            "continuation must actually enter the provider"
        );
        assert!(
            !shows_read(&client),
            "independent API read must not populate the TUI"
        );
        let reconciliations = client.diagnostics().reconciliations;
        eprintln!("TOOL_EVENTS {label} DURABLE_READ_COMMITTED");
        let mut displayed = false;
        for _ in 0..JOURNEY_POLLS {
            client
                .pump()
                .unwrap_or_else(|error| panic!("healthy stream pump: {error}"));
            if shows_read(&client) {
                displayed = true;
                break;
            }
            thread::sleep(JOURNEY_SLEEP);
        }
        eprintln!("TOOL_EVENTS {label} DISPLAYED {displayed}");
        assert!(
            displayed,
            "committed read remained absent without a manual reload"
        );
        assert_held_stream(&mut client, terminal_only, reconciliations);
        // Every mid-stream assertion above has now run, so the release may
        // fire. Teardown then drains the owned response instead of racing it.
        release_continuation();
        assert!(
            wait_for(|| CONTINUATION_DONE.load(Ordering::SeqCst)),
            "continuation must drain before teardown"
        );
        assert!(
            !RELEASE_FAILED.load(Ordering::SeqCst),
            "the provider's bounded release wait elapsed instead of draining"
        );
    }
}

/// Everything the held continuation must still show while it is withheld.
fn assert_held_stream(
    client: &mut Client<WitnessConnection>,
    terminal_only: bool,
    reconciliations: usize,
) {
    assert!(
        !CONTINUATION_DONE.load(Ordering::SeqCst),
        "next model message must still be withheld"
    );
    if terminal_only {
        assert!(
            client.peer_mut().terminals_received > 0,
            "real terminal must arrive"
        );
        assert_eq!(
            client.peer_mut().openings_withheld,
            2,
            "isolation must consume both opening hints"
        );
    }
    assert!(
        client.peer_mut().http.subscribed(),
        "stream must stay healthy"
    );
    assert_eq!(client.diagnostics().stream_closes, 0);
    assert_eq!(
        client.diagnostics().reconciliations,
        reconciliations,
        "no full reload may mask the hint"
    );
}

/// Poll a condition for the journey's bounded window.
fn wait_for(condition: impl Fn() -> bool) -> bool {
    for _ in 0..JOURNEY_POLLS {
        if condition() {
            return true;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    false
}

fn shows_read(client: &Client<WitnessConnection>) -> bool {
    client
        .view()
        .is_some_and(|view| view.slices.iter().any(|slice| slice.text.contains(MARKER)))
}

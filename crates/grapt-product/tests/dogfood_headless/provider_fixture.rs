//! The scripted provider every headless witness runs against.
//!
//! It is a real HTTP server on loopback speaking the provider's own event
//! shapes, so the product's transport, streaming decode and durable capture are
//! the ones under test; only the model is scripted. It lives beside the
//! witnesses rather than inside their carrier because it is a fixture with a
//! contract of its own — the `Script` enum IS that contract, and a reader
//! asking "what can a turn be made to do" should find one file that answers.

use super::*;
#[path = "provider_responses.rs"]
mod responses;
#[cfg(not(windows))]
pub(super) use responses::respond_tool_shell;
pub(super) use responses::{
    respond, respond_output, respond_success_text, respond_tool_history, respond_tool_plugin,
    respond_tool_read, respond_tool_reads, respond_tool_subagent, respond_with_headers,
    stall_before_first_fragment, stream_paced, stream_text_then_stall,
};
#[cfg(not(windows))]
use responses::{respond_partial_then_held_disconnect, respond_tool_history_input};
use responses::{respond_text_then_reads, respond_tool_input, respond_tool_reads_measured};

#[path = "chunked_provider.rs"]
mod chunked_provider;
#[path = "provider_request.rs"]
mod provider_request;
#[path = "terminal_provider.rs"]
mod terminal_provider;
pub(super) use provider_request::read_provider_request;

/// A scripted summary the product's guard accepts: it carries an objective.
pub(super) const OBJECTIVE_SUMMARY: &str = "## Objective\n- finish the task";

/// What a scripted output of at least its length opens with: an Objective
/// section, so the same script stands as a summary the product's guard
/// accepts. The total stays `bytes`.
pub(super) const OUTPUT_OBJECTIVE: &str = "## Objective\n- ";

#[derive(Clone)]
pub(super) enum Script {
    Success,
    /// Select one response from the actual request when concurrent Sessions
    /// can reach the fixture in either order. Selectors cannot nest.
    ForRequest(fn(&str) -> Script),
    SuccessText(&'static str),
    /// An exact event stream, for a protocol family the named scripts do not
    /// speak. The witness that builds it owns its shape.
    Raw(String),
    HeldRaw {
        body: String,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    },
    /// Exact HTTP body chunks for transport-boundary witnesses.
    RawChunks(Vec<Vec<u8>>),
    /// A 200 with extra response headers, as a route that hands back routing
    /// state (Codex's x-codex-turn-state) does.
    RawWithHeaders {
        headers: Vec<(&'static str, String)>,
        body: String,
    },
    /// Grouped reads whose reported input usage is the request's byte length
    /// divided by `bytes_per_token`: a plausible tokenizer, so admission can
    /// calibrate against it.
    ToolReadsMeasured {
        calls: Vec<(&'static str, &'static str)>,
        bytes_per_token: usize,
    },
    CompatibleSuccess,
    /// One bounded native tool input, for semantic permission witnesses.
    ToolInput {
        call: &'static str,
        tool: &'static str,
        arguments: serde_json::Value,
    },
    #[cfg(unix)]
    TerminalInput {
        call: &'static str,
        source_call: &'static str,
        chars: &'static str,
    },
    ToolRead {
        call: &'static str,
        path: &'static str,
    },
    ToolReads {
        calls: Vec<(&'static str, &'static str)>,
    },
    /// Hold a tool-producing response while selection commits, so the next
    /// Step witnesses the running execution's captured permission policy.
    #[cfg(target_os = "linux")]
    HeldReads {
        calls: Vec<(&'static str, &'static str)>,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    },
    /// An assistant text of `text_bytes`, then these reads, in one response:
    /// a Step whose text a summary must carry, unlike its tool output.
    TextThenReads {
        text_bytes: usize,
        calls: Vec<(&'static str, &'static str)>,
    },
    ToolPlugin {
        call: &'static str,
        text: &'static str,
    },
    #[cfg(not(windows))]
    ToolShell {
        call: &'static str,
        command: &'static str,
    },
    ToolHistory {
        call: &'static str,
        target: &'static str,
    },
    #[cfg(not(windows))]
    ToolHistoryInput {
        call: &'static str,
        arguments: serde_json::Value,
    },
    /// The model asks for a subagent. The CHILD's own turn is served by a later
    /// script in the same queue, which is what makes this a real
    /// parent -> tool -> child -> result path rather than a mocked tool result.
    ToolSubagent {
        call: &'static str,
        task: &'static str,
    },
    PhasedSubagent {
        call: &'static str,
        task: &'static str,
        finish: Arc<terminal_coordination::Phase>,
        tail_release: Arc<terminal_coordination::Phase>,
    },
    Output(usize),
    HeldOutput {
        bytes: usize,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    },
    HeldTerminalOutput {
        bytes: usize,
        entered: Arc<terminal_coordination::Phase>,
        release: Arc<terminal_coordination::Phase>,
    },
    ControlledTerminalOutput {
        bytes: usize,
        entered: Arc<terminal_coordination::Phase>,
        release: Arc<terminal_coordination::Phase>,
        disconnect: Arc<AtomicBool>,
    },
    PhasedOutput {
        advance: Arc<terminal_coordination::Phase>,
        finish: Arc<terminal_coordination::Phase>,
    },
    Unauthorized,
    HeldRefusal {
        status: u16,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    },
    /// A non-success status with this exact body, as a provider refusing
    /// the request answers.
    Refused {
        status: u16,
        body: &'static str,
    },
    /// Send a refusal head, then disconnect before its promised body byte.
    TruncatedRefusal {
        status: u16,
    },
    Malformed,
    /// Real text, and THEN a frame that is not JSON.
    ///
    /// The Step fails after committing durable assistant text, which is the
    /// only shape that separates "the child failed" from "the child said
    /// nothing". A consumer that read a child's transcript on liveness alone
    /// would find this text and report it as an answer.
    PartialThenMalformed,
    /// A finished reasoning summary, then unfinished text, then the stream
    /// ends with no terminal: a retryable drop after finished reasoning.
    FinishedReasoningThenDrop,
    #[cfg(not(windows))]
    DisconnectBeforeHead,
    #[cfg(not(windows))]
    PartialThenHeldDisconnect {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    },
    StallBeforeFirstFragment,
    /// Text, a pause past the checkpoint interval, more text, then silence
    /// until the server's connection closes: a process lost mid-answer.
    TextThenStall,
    /// A burst of small numbered text deltas, more at a fixed pace, then
    /// completion: a fast streaming model. A closed connection ends the stream quietly, since an
    /// interrupt or a service restart closes it on purpose.
    Paced {
        label: &'static str,
        burst: usize,
        deltas: usize,
        interval: Duration,
        ticks: &'static AtomicUsize,
        /// While set, the stream waits after its burst (at most 30 s), so a
        /// test can act mid-stream before the rest is sent.
        hold: Option<&'static AtomicBool>,
        /// Each tick ends with a line break rather than a space. An open
        /// answer is drawn a whole line at a time, so a test that must see
        /// held text on screen asks for line breaks explicitly.
        breaks: bool,
    },
}

pub(super) struct ProviderFixture {
    queue: Arc<Mutex<VecDeque<Script>>>,
    pub(super) address: SocketAddr,
    /// Scripted coding/SE-summary calls; unsupported compactions are separate.
    pub(super) calls: Arc<AtomicUsize>,
    pub(super) unsupported_compactions: Arc<Mutex<Vec<String>>>,
    pub(super) heads: Arc<Mutex<Vec<String>>>,
    pub(super) bodies: Arc<Mutex<Vec<String>>>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) thread: Option<thread::JoinHandle<()>>,
    terminal_workers: Arc<Mutex<Vec<terminal_coordination::Worker>>>,
    /// Unexpected malformed requests remain fixture failures, even after the peer
    /// receives an HTTP refusal and its Session can settle normally.
    pub(super) refused: Arc<Mutex<Vec<String>>>,
    /// Requests their client cut short, when the fixture tolerates cuts.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(super) truncated: Arc<Mutex<Vec<String>>>,
}

impl ProviderFixture {
    pub(super) fn start(scripts: Vec<Script>) -> Self {
        Self::start_with(scripts, false, false)
    }

    /// For journeys that cancel requests or kill the product mid-turn: a request
    /// the product's own cut leaves unfinished is recorded in `truncated`,
    /// consuming no script, instead of failing the fixture. Any other
    /// refused request still fails it.
    pub(super) fn start_tolerating_cuts(scripts: Vec<Script>) -> Self {
        Self::start_with(scripts, true, false)
    }

    /// Provider-compaction cases explicitly script the provider's response.
    pub(super) fn start_compacting(scripts: Vec<Script>) -> Self {
        Self::start_with(scripts, false, true)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn start_with(scripts: Vec<Script>, tolerate_cuts: bool, supports_compaction: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|error| panic!("provider loopback bind failed: {error}"));
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|error| panic!("provider nonblocking failed: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("provider address failed: {error}"));
        let calls = Arc::new(AtomicUsize::new(0));
        let heads = Arc::new(Mutex::new(Vec::new()));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let queue = Arc::new(Mutex::new(VecDeque::from(scripts)));
        let unsupported_compactions = Arc::new(Mutex::new(Vec::new()));
        let thread_unsupported = Arc::clone(&unsupported_compactions);
        let thread_calls = Arc::clone(&calls);
        let thread_heads = Arc::clone(&heads);
        let thread_bodies = Arc::clone(&bodies);
        let thread_stop = Arc::clone(&stop);
        let thread_queue = Arc::clone(&queue);
        let terminal_workers = Arc::new(Mutex::new(Vec::new()));
        let owned_workers = Arc::clone(&terminal_workers);
        let refused = Arc::new(Mutex::new(Vec::new()));
        let truncated = Arc::new(Mutex::new(Vec::new()));
        let thread_truncated = Arc::clone(&truncated);
        let thread_refused = Arc::clone(&refused);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap_or_else(|error| {
                            panic!("provider stream blocking mode failed: {error}")
                        });
                        let (head, body) = match read_provider_request(&mut stream) {
                            Ok(request) => request,
                            Err(error) if tolerate_cuts && error.is_truncation() => {
                                locked(&thread_truncated).push(error.to_string());
                                continue;
                            }
                            Err(error) => {
                                let mut failures = locked(&thread_refused);
                                assert!(failures.len() < 16, "provider request refusal bound");
                                failures.push(error.to_string());
                                drop(failures);
                                // A parser refusal is not a transport outage. Give the
                                // real client a terminal HTTP status instead of killing
                                // the listener and inducing its normal transport retry.
                                // The retained refusal still fails fixture Drop unless
                                // a negative witness explicitly consumes it.
                                if stream
                                    .set_write_timeout(Some(Duration::from_secs(1)))
                                    .is_ok()
                                {
                                    let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                                }
                                continue;
                            }
                        };
                        if !supports_compaction && body.contains("\"compaction_trigger\"") {
                            let mut refusals = locked(&thread_unsupported);
                            assert!(refusals.len() < 64, "unsupported compaction bound");
                            refusals.push(body);
                            respond(&mut stream, 400, "unsupported compaction");
                            continue;
                        }
                        thread_calls.fetch_add(1, Ordering::SeqCst);
                        locked(&thread_heads).push(head);
                        let script = locked(&thread_queue).pop_front();
                        let script = match script {
                            Some(Script::ForRequest(select)) => Some(select(&body)),
                            other => other,
                        };
                        locked(&thread_bodies).push(body);
                        match script {
                            Some(Script::ForRequest(_)) => panic!("provider selectors cannot nest"),
                            Some(Script::Success) => respond(&mut stream, 200, SUCCESS),
                            Some(Script::HeldRefusal { status, entered, release }) => {
                                entered.wait();
                                release.wait();
                                respond(&mut stream, status, "");
                            }
                            Some(Script::SuccessText(text)) => {
                                respond_success_text(&mut stream, text)
                            }
                            Some(Script::Raw(body)) => respond(&mut stream, 200, &body),
                            Some(Script::HeldRaw { body, entered, release }) => {
                                entered.wait();
                                release.wait();
                                respond(&mut stream, 200, &body);
                            }
                            Some(Script::RawChunks(chunks)) => chunked_provider::respond(&mut stream, &chunks),
                            Some(Script::RawWithHeaders { headers, body }) => {
                                respond_with_headers(&mut stream, &headers, &body)
                            }
                            Some(Script::ToolReadsMeasured {
                                calls,
                                bytes_per_token,
                            }) => {
                                let measured = locked(&thread_bodies)
                                    .last()
                                    .map_or(0, |body| body.len() / bytes_per_token.max(1));
                                respond_tool_reads_measured(&mut stream, &calls, measured)
                            }
                            Some(Script::CompatibleSuccess) => respond(&mut stream, 200, concat!(
                                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Secondary account reached.\"},\"finish_reason\":null}]}\n\n",
                                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4,\"total_tokens\":16}}\n\n",
                                "data: [DONE]\n\n"
                            )),
                            Some(Script::ToolInput { call, tool, arguments }) => {
                                respond_tool_input(&mut stream, call, tool, &arguments)
                            }
                            #[cfg(unix)]
                            Some(Script::TerminalInput { call, source_call, chars }) => {
                                let body = locked(&thread_bodies).last().cloned().unwrap_or_default();
                                let operation = terminal_provider::operation_from_body(&body, source_call)
                                    .unwrap_or_else(|error| panic!("terminal provider fixture: {error}"));
                                respond_tool_input(&mut stream, call, "write_stdin", &serde_json::json!({
                                    "session_id":operation, "chars":chars, "yield_time_ms":5000
                                }));
                            }
                            Some(Script::ToolRead { call, path }) => {
                                respond_tool_read(&mut stream, call, path)
                            }
                            #[cfg(target_os = "linux")]
                            Some(Script::HeldReads { calls, entered, release }) => {
                                thread::spawn(move || {
                                    let _ = entered.wait();
                                    let _ = release.wait();
                                    respond_tool_reads(&mut stream, &calls);
                                });
                            }
                            Some(Script::ToolReads { calls }) => {
                                respond_tool_reads(&mut stream, &calls)
                            }
                            Some(Script::TextThenReads { text_bytes, calls }) => {
                                respond_text_then_reads(&mut stream, text_bytes, &calls)
                            }
                            Some(Script::ToolPlugin { call, text }) => {
                                respond_tool_plugin(&mut stream, call, text)
                            }
                            #[cfg(not(windows))]
                            Some(Script::ToolShell { call, command }) => {
                                respond_tool_shell(&mut stream, call, command)
                            }
                            Some(Script::ToolHistory { call, target }) => {
                                respond_tool_history(&mut stream, call, target)
                            }
                            #[cfg(not(windows))]
                            Some(Script::ToolHistoryInput { call, arguments }) => {
                                respond_tool_history_input(&mut stream, call, &arguments)
                            }
                            Some(Script::ToolSubagent { call, task }) => {
                                respond_tool_subagent(&mut stream, call, task)
                            }
                            Some(Script::PhasedSubagent { call, task, finish, tail_release }) => {
                                let stop = Arc::clone(&thread_stop);
                                let cancellation = Arc::clone(&stop);
                                assert!(locked(&owned_workers).len() < 3, "SSE provider worker bound");
                                locked(&owned_workers).push(terminal_coordination::Worker::spawn("subagent provider", stop, move || {
                                    terminal_provider::respond_phased_subagent(&mut stream, call, task, &finish, &tail_release, &cancellation)
                                }));
                            }
                            Some(Script::Output(bytes)) => respond_output(&mut stream, bytes),
                            Some(Script::HeldOutput {
                                bytes,
                                entered,
                                release,
                            }) => {
                                thread::spawn(move || {
                                    let _ = entered.wait();
                                    let _ = release.wait();
                                    respond_output(&mut stream, bytes);
                                });
                            }
                            Some(Script::Unauthorized) => respond(&mut stream, 401, "denied"),
                            Some(Script::HeldTerminalOutput { bytes, entered, release }) => {
                                let stop = Arc::clone(&thread_stop);
                                let cancellation = Arc::clone(&stop);
                                assert!(locked(&owned_workers).len() < 3, "SSE provider worker bound");
                                locked(&owned_workers).push(terminal_coordination::Worker::spawn("held provider", stop, move || {
                                    stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(|error| format!("SSE provider write deadline: {error}"))?;
                                    entered.release();
                                    release.wait("held output release", &cancellation)?;
                                    respond_output(&mut stream, bytes);
                                    Ok(())
                                }));
                            }
                            Some(Script::ControlledTerminalOutput { bytes, entered, release, disconnect }) => {
                                let stop = Arc::clone(&thread_stop);
                                let cancellation = Arc::clone(&stop);
                                assert!(locked(&owned_workers).len() < 3, "SSE provider worker bound");
                                locked(&owned_workers).push(terminal_coordination::Worker::spawn("controlled provider", stop, move || {
                                    stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(|error| format!("SSE provider write deadline: {error}"))?;
                                    entered.release();
                                    release.wait("controlled output release", &cancellation)?;
                                    if !disconnect.load(Ordering::SeqCst) { respond_output(&mut stream, bytes); }
                                    Ok(())
                                }));
                            }
                            Some(Script::PhasedOutput { advance, finish }) => {
                                let stop = Arc::clone(&thread_stop);
                                let cancellation = Arc::clone(&stop);
                                assert!(locked(&owned_workers).len() < 3, "SSE provider worker bound");
                                locked(&owned_workers).push(terminal_coordination::Worker::spawn("phased provider", stop, move || {
                                    terminal_provider::respond_phased_output(&mut stream, &advance, &finish, &cancellation)
                                }));
                            }
                            Some(Script::Refused { status, body }) => {
                                respond(&mut stream, status, body)
                            }
                            Some(Script::TruncatedRefusal { status }) => {
                                stream.write_all(format!("HTTP/1.1 {status} Refused\r\nContent-Length: 1\r\nConnection: close\r\n\r\n").as_bytes())
                                    .unwrap_or_else(|error| panic!("truncated refusal: {error}"));
                            }
                            Some(Script::Malformed) => {
                                respond(&mut stream, 200, "data: {not-json}\n\n")
                            }
                            Some(Script::FinishedReasoningThenDrop) => respond(
                                &mut stream,
                                200,
                                concat!(
                                    "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"first thought\"}\n\n",
                                    "data: {\"type\":\"response.reasoning_summary_text.done\"}\n\n",
                                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"half an answer\"}\n\n"
                                ),
                            ),
                            Some(Script::PartialThenMalformed) => respond(
                                &mut stream,
                                200,
                                concat!(
                                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Partial child text.\"}\n\n",
                                    "data: {\"type\":\"response.output_text.done\"}\n\n",
                                    "data: {not-json}\n\n"
                                ),
                            ),
                            #[cfg(not(windows))]
                            Some(Script::DisconnectBeforeHead) => {}
                            #[cfg(not(windows))]
                            Some(Script::PartialThenHeldDisconnect { entered, release }) => {
                                respond_partial_then_held_disconnect(
                                    &mut stream,
                                    &entered,
                                    &release,
                                )
                            }
                            Some(Script::StallBeforeFirstFragment) => {
                                stall_before_first_fragment(&mut stream);
                            }
                            Some(Script::TextThenStall) => stream_text_then_stall(&mut stream),
                            Some(Script::Paced {
                                label,
                                burst,
                                deltas,
                                interval,
                                ticks,
                                hold,
                                breaks,
                            }) => stream_paced(
                                &mut stream,
                                label,
                                (burst, deltas, interval),
                                ticks,
                                (hold, breaks),
                            ),
                            None => respond(&mut stream, 500, "unexpected provider call"),
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("provider accept failed: {error}"),
                }
            }
        });
        Self {
            queue,
            address,
            calls,
            unsupported_compactions,
            heads,
            bodies,
            stop,
            thread: Some(handle),
            terminal_workers,
            refused,
            truncated,
        }
    }
}

impl ProviderFixture {
    pub(super) fn append(&self, scripts: Vec<Script>) {
        let mut queue = locked(&self.queue);
        assert!(
            queue.len() + scripts.len() <= 64,
            "scripted continuation bound"
        );
        queue.extend(scripts);
    }

    pub(super) fn settle_terminal_workers(&self) -> Result<(), String> {
        let mut failure = None;
        for worker in locked(&self.terminal_workers).iter_mut() {
            if let Err(error) = worker.finish() {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
impl Drop for ProviderFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let listener_failed = self
            .thread
            .take()
            .is_some_and(|handle| handle.join().is_err());
        let workers = self.settle_terminal_workers();
        let refused = locked(&self.refused);
        if listener_failed || workers.is_err() || !refused.is_empty() {
            if thread::panicking() {
                eprintln!(
                    "provider cleanup: listener failed={listener_failed}, workers={workers:?}, refused={refused:?}"
                );
            } else {
                panic!("provider fixture failed: listener failed={listener_failed}, workers={workers:?}, refused={refused:?}");
            }
        }
    }
}

// These are independent duration-bounded process journeys, not a shared-disk
// throughput benchmark. Each journey retains its own concurrent clients/tasks.
// Parallel journeys exhausted their short service leases while ext4 fsync waited
// in jbd2_log_wait_commit; keep that unrelated pressure outside each witness.
static PROCESS_JOURNEY_CUSTODY: Mutex<()> = Mutex::new(());

/// Every custody hand-off, counted, so a waiter can tell a queue that moves
/// from a holder that is stuck.
static PROCESS_JOURNEY_HANDOFFS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn process_journey_custody() -> std::sync::MutexGuard<'static, ()> {
    match custody_within(
        &PROCESS_JOURNEY_CUSTODY,
        &PROCESS_JOURNEY_HANDOFFS,
        Duration::from_secs(300),
        Duration::from_millis(25),
    ) {
        Ok(guard) => guard,
        Err(generation) => panic!(
            "independent process journey custody made no hand-off within 300 seconds \
             (waiting journey {:?}, custody generation {generation})",
            thread::current().name().unwrap_or("unnamed")
        ),
    }
}

/// Wait for `lock`, bounded per HAND-OFF rather than in total.
///
/// Journeys queue on one custody lock, so the last waiter's total wait grows
/// with the number of journeys ahead of it: a total bound fails a healthy queue
/// just because it is long. The bound therefore restarts whenever custody
/// changes hands, detected by a generation counter bumped on every acquire (a
/// thread identity cannot tell A->B->A from A holding throughout). A holder
/// that never releases still fails its waiters after `allowance`; the refusal
/// carries the last custody generation observed.
pub(super) fn custody_within<'lock>(
    lock: &'lock Mutex<()>,
    handoffs: &AtomicUsize,
    allowance: Duration,
    interval: Duration,
) -> Result<std::sync::MutexGuard<'lock, ()>, usize> {
    let mut seen = handoffs.load(Ordering::SeqCst);
    let mut wait = PollDeadline::within(allowance);
    loop {
        match lock.try_lock() {
            Ok(guard) => {
                handoffs.fetch_add(1, Ordering::SeqCst);
                return Ok(guard);
            }
            Err(std::sync::TryLockError::Poisoned(error)) => {
                handoffs.fetch_add(1, Ordering::SeqCst);
                return Ok(error.into_inner());
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                let now = handoffs.load(Ordering::SeqCst);
                if now != seen {
                    seen = now;
                    wait = PollDeadline::within(allowance);
                }
                if !wait.pending() {
                    return Err(seen);
                }
                thread::sleep(interval);
            }
        }
    }
}

/// A queue longer than the bound, whose holders each release promptly, is
/// served in full; a holder that never releases fails its waiter in bounded time.
#[test]
fn custody_is_bounded_per_hand_off_not_per_queue() {
    static QUEUE: Mutex<()> = Mutex::new(());
    static QUEUE_HANDOFFS: AtomicUsize = AtomicUsize::new(0);
    // 200 ms per hand-off. Sixteen holders of 25 ms each make
    // a 400 ms queue, twice the bound, which a total bound cannot serve.
    let (allowance, interval) = (Duration::from_millis(200), Duration::from_millis(5));
    let holders: Vec<_> = (0..16)
        .map(|_| {
            thread::spawn(move || {
                let guard = custody_within(&QUEUE, &QUEUE_HANDOFFS, allowance, interval);
                assert!(
                    guard.is_ok(),
                    "FSE106 a healthy queue longer than the bound starved a waiter"
                );
                thread::sleep(Duration::from_millis(25));
                drop(guard);
            })
        })
        .collect();
    for holder in holders {
        if holder.join().is_err() {
            panic!("FSE106 a healthy queue longer than the bound starved a waiter");
        }
    }

    static STUCK: Mutex<()> = Mutex::new(());
    static STUCK_HANDOFFS: AtomicUsize = AtomicUsize::new(0);
    let held = custody_within(&STUCK, &STUCK_HANDOFFS, allowance, interval);
    assert!(held.is_ok());
    assert_eq!(
        custody_within(&STUCK, &STUCK_HANDOFFS, allowance, interval).err(),
        Some(1),
        "a waiter behind a holder that never releases was served, or misnamed its generation"
    );
    drop(held);
}

pub(super) fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(super) fn root(label: &str) -> PathBuf {
    let base = resolved_base()
        .unwrap_or_else(|error| panic!("headless fixture base for {label}: {error}"));
    base.join(format!(
        "secode-live-headless1-{}-{label}",
        std::process::id()
    ))
}

/// The fixture base for this process. Under a gate the environment holds
/// the base the gate resolved and published before this process started:
/// the fixture validates and consumes that value, and never chooses
/// another. When the variable is absent the run is standalone (an
/// ungoverned local run), and the fixture keeps its own default: the
/// system temp directory, or the drive-level temp on Windows — on
/// Windows the temp directory sits under the user profile, and
/// `CatalogSources::read` walks every ancestor of the service directory
/// for `.claude` and `.agents` skill sources, so the profile's own
/// skills would fold into every journey; at the drive level the ancestor
/// chain carries no user-global sources.
fn resolved_base() -> Result<PathBuf, String> {
    let override_set = std::env::var_os("SECODE_HEADLESS_FIXTURE_BASE");
    let base = match &override_set {
        Some(raw) => PathBuf::from(raw),
        None => {
            if cfg!(windows) {
                PathBuf::from(r"C:\temp")
            } else {
                std::env::temp_dir()
            }
        }
    };
    if override_set.is_some() {
        if base.as_os_str().is_empty() {
            return Err("SECODE_HEADLESS_FIXTURE_BASE is set but empty".to_owned());
        }
        if !governed_absolute(&base) {
            return Err(format!(
                "SECODE_HEADLESS_FIXTURE_BASE must name a fully absolute (rooted, \
                 drive-qualified) directory; refusing {base:?}"
            ));
        }
    }
    let names_directory = fs::metadata(&base)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);
    if !names_directory {
        if override_set.is_some() {
            return Err(format!(
                "SECODE_HEADLESS_FIXTURE_BASE must name an existing directory; \
                 refusing {base:?}"
            ));
        }
        return Err(format!(
            "standalone headless fixture base {base:?} does not name an existing directory"
        ));
    }
    Ok(base)
}

/// A fully absolute native path: rooted, and on Windows an ordinary
/// disk-letter drive with a rooted path, where drive-relative (`C:temp`),
/// root-only current-drive (`C:`) and UNC/verbatim prefixes refuse — the
/// same contract the gate's resolver applies.
fn governed_absolute(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    #[cfg(windows)]
    {
        let mut components = path.components();
        let drive_qualified = components.any(|component| {
            matches!(
                component,
                std::path::Component::Prefix(prefix)
                    if matches!(prefix.kind(), std::path::Prefix::Disk(_))
            )
        });
        let rooted = components.any(|component| component == std::path::Component::RootDir);
        drive_qualified && rooted
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// One Responses stream that calls `write`.
pub(super) fn write_call(call: &str, path: &str, content: &str) -> Script {
    let arguments = serde_json::json!({"path": path, "content": content}).to_string();
    Script::Raw(format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"write\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"))
    ))
}

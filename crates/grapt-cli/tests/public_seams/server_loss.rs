//! WINDOWS-CLI-SERVER-LOSS-01: the `grapt run` server-loss and cancellation
//! contract in `docs/cli-server-loss/CONTRACT.md`, at the executor seam.

use std::sync::mpsc;
use std::time::Duration;

use grapt_cli::{
    run_with_cancellation, ApiClientError, ClientEndpoint, RunReceipt, SessionSelector,
    CANCELLATION_HANDOFF, MAX_CONSECUTIVE_TRANSPORT_FAILURES, TRANSPORT_STALL_EXIT_BOUND,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

use super::*;

const PROMPT_ACCEPTED: &str = r#"{"data":{"admittedSeq":1,"id":"msg_user","sessionID":"ses_a","timeCreated":1.0,"type":"user","data":{"text":"hello"},"delivery":"queue"}}"#;
/// Scheduling allowance on a loaded gate machine; never part of the contract.
const SCHEDULING_SLACK: Duration = Duration::from_secs(5);
/// How long a stalled stdout reader stays stalled before the witness releases
/// it, so a run that cannot be cancelled fails the assertion instead of hanging.
const STALLED_READER_RELEASE: Duration = Duration::from_secs(15);
/// How long an embedded product may take to end once its CLI stops owning it.
const PRODUCT_END: Duration = Duration::from_secs(5);
/// Scheduling allowance for the multi-minute stall-bound witness: it crosses
/// seven timer wakeups on a loaded gate machine.
const SILENT_EXIT_SLACK: Duration = Duration::from_secs(15);
/// The stall witness's request allowance: a few seconds, not the 30 s
/// production one, so the red and green witnesses each run in seconds.
const STALL_REQUEST_ALLOWANCE: Duration = Duration::from_secs(2);
/// The stalling server's silence on the first permission poll: 1.5x the
/// witness's request allowance, the D01 defect shape scaled down.
const PERMISSION_STALL: Duration = Duration::from_secs(4);
/// The durable message page after the admitted prompt, newest-first.
const HISTORY_WITH_PROMPT: &str = r#"{"data":[{"type":"user","id":"msg_user","time":{"created":0.5},"text":"hello"}],"cursor":{}}"#;
const NO_PERMISSIONS: &str = r#"{"data":[],"hasMore":false}"#;
const SETTLED: &str = r#"{"data":{}}"#;

fn session() -> TestResult<SessionSelector> {
    Ok(SessionSelector::Id {
        session: "ses_a".parse()?,
        project: None,
    })
}

/// Answers the event stream and the prompt admission, then goes silent: every
/// later connection is accepted and read, and never answered.
async fn spawn_silent_after_admission() -> TestResult<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let mut held: Vec<TcpStream> = Vec::new();
        while let Ok((mut stream, _)) = listener.accept().await {
            let Ok(request) = support::read_request(&mut stream).await else {
                continue;
            };
            let line = request.lines().next().unwrap_or_default().to_owned();
            if line.starts_with("GET /api/event ") {
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
                let _written = stream.write_all(head.as_bytes()).await;
            } else if line.starts_with("POST /api/session/ses_a/prompt ") {
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{PROMPT_ACCEPTED}",
                    PROMPT_ACCEPTED.len()
                );
                let _written = stream.write_all(response.as_bytes()).await;
            }
            held.push(stream);
        }
    });
    Ok(format!("http://{address}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_run_whose_server_goes_silent_after_admission_exits_remote_within_the_bound() -> TestResult
{
    let endpoint = spawn_silent_after_admission().await?;
    let output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let started = Instant::now();
    let (result, output) = run_with_cancellation(
        Runner::new(client(&endpoint)?),
        session()?,
        "hello".to_owned(),
        output,
        std::future::pending(),
    )
    .await;
    let elapsed = started.elapsed();
    let error = match result {
        Err(error) => error,
        Ok(receipt) => return Err(format!("a silent server settled a run: {receipt:?}").into()),
    };
    assert_eq!(error.exit_class(), ExitClass::Remote, "{error}");
    // A silent server is tolerated, not fatal, at the first timed out
    // re-read: the durable reads reissue under the bounded streak, and the
    // run ends when the streak exhausts it. The error must name the service
    // that stopped answering, the exhausted count, and the last failure.
    let (failures, service, last) = match &error {
        RunError::Api(ApiClientError::TransportStalled {
            failures,
            service,
            last,
        }) => (failures, service.as_str(), last.as_str()),
        other => {
            return Err(format!(
                "the silent server did not surface as a stalled transport: {other}"
            )
            .into())
        }
    };
    assert_eq!(
        *failures, MAX_CONSECUTIVE_TRANSPORT_FAILURES,
        "the streak ended short of its bound: {last}"
    );
    assert!(
        service.contains(&endpoint.replace("http://", "")),
        "the stalled service must be named by endpoint: {service}"
    );
    // reqwest's display of a timed out request is the generic
    // "error sending request for url (...)"; what must survive is that it
    // names the request that stopped answering.
    assert!(
        last.contains(endpoint.replace("http://", "").as_str()),
        "the last transport failure must name the service: {last}"
    );
    // From the moment the server stops answering: the stall bound itself.
    // It already carries the tick to the first stalled request and the
    // allowance of every reissue; adding the server-loss bound would count
    // that first request twice. The slack is scheduling headroom on a
    // loaded gate machine, not contract time.
    let bound = TRANSPORT_STALL_EXIT_BOUND;
    assert!(
        elapsed <= bound + SILENT_EXIT_SLACK,
        "exit took {elapsed:?}, beyond the {bound:?} stall bound"
    );
    let (stdout, _stderr, _receipt) = output
        .ok_or("the run kept its output on a drained stdout")?
        .into_parts();
    assert!(
        String::from_utf8(stdout)?.contains("msg_user"),
        "the admission was not reported before the server went silent"
    );
    Ok(())
}

/// Answers the event stream and the prompt admission. The first permission
/// poll is held for [`PERMISSION_STALL`] without an answer, the D01 defect
/// shape: a live service that stops answering mid-run. Later polls answer at
/// once, and the session settles.
async fn spawn_stalling_permission_server() -> TestResult<(
    String,
    tokio::task::JoinHandle<TestResult<Vec<String>>>,
    tokio::sync::oneshot::Sender<()>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let mut held: Vec<TcpStream> = Vec::new();
        let mut requests: Vec<String> = Vec::new();
        let mut permission_attempts = 0_u32;
        'outer: loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { break; };
                    let Ok(request) = support::read_request(&mut stream).await else {
                        continue 'outer;
                    };
                    let line = request.lines().next().unwrap_or_default().to_owned();
                    let request_line = format!(
                        "{} {}",
                        line.split_whitespace().next().unwrap_or_default(),
                        line.split_whitespace().nth(1).unwrap_or_default()
                    );
                    let response = if line.starts_with("GET /api/event ") {
                        let head =
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
                        let _written = stream.write_all(head.as_bytes()).await;
                        held.push(stream);
                        requests.push(request_line);
                        continue 'outer;
                    } else if line.starts_with("POST /api/session/ses_a/prompt ") {
                        Some(format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PROMPT_ACCEPTED}",
                            PROMPT_ACCEPTED.len()
                        ))
                    } else if line.starts_with("GET /api/session/ses_a/permission ") {
                        permission_attempts = permission_attempts.saturating_add(1);
                        if permission_attempts == 1 {
                            tokio::time::sleep(PERMISSION_STALL).await;
                        }
                        Some(json_response(NO_PERMISSIONS))
                    } else if line.starts_with("GET /api/session/active ") {
                        Some(json_response(SETTLED))
                    } else if line.starts_with("GET /api/session/ses_a/message ") {
                        Some(json_response(HISTORY_WITH_PROMPT))
                    } else {
                        None
                    };
                    let Some(response) = response else {
                        return Err(format!(
                            "unstalled server got an unexpected request: {line}"
                        )
                        .into());
                    };
                    requests.push(request_line);
                    let _written = stream.write_all(response.as_bytes()).await;
                }
                _ = &mut shutdown_rx => break 'outer,
            }
        }
        Ok(requests)
    });
    Ok((format!("http://{address}"), server, shutdown_tx))
}

fn json_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_run_whose_permission_poll_stalls_once_recovers_and_completes() -> TestResult {
    // Red at the pre-fix client: the first timed out re-read ended the run
    // Remote (12) — the D01 defect, measured twice.
    let (endpoint, server, shutdown) = spawn_stalling_permission_server().await?;
    let client = ApiClient::loopback_testkit_allowance(
        ClientEndpoint::loopback_testkit(&endpoint)?,
        CliLimits::production(),
        STALL_REQUEST_ALLOWANCE,
        Duration::from_secs(60),
    )?;
    let output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let started = Instant::now();
    let (result, _output) = run_with_cancellation(
        Runner::new(client),
        session()?,
        "hello".to_owned(),
        output,
        std::future::pending(),
    )
    .await;
    let receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => return Err(format!("a recovered stall ended the run: {error}").into()),
    };
    assert_eq!(receipt.exit, ExitClass::Success);
    let elapsed = started.elapsed();
    assert!(
        elapsed <= PERMISSION_STALL + STALL_REQUEST_ALLOWANCE + SCHEDULING_SLACK,
        "recovery took {elapsed:?}, far beyond the stall itself"
    );
    let _ = shutdown.send(());
    let requests = server.await??;
    let polls = requests
        .iter()
        .filter(|line| *line == "GET /api/session/ses_a/permission")
        .count();
    assert!(
        polls >= 2,
        "the stalled permission poll was not reissued: {requests:?}"
    );
    Ok(())
}

/// Timing stays local to this loopback case; no captured user input is replayed.
#[derive(Debug, Default)]
struct TerminalPollLog {
    requests: Vec<(String, Duration, Duration)>,
    frames: Vec<Duration>,
    admitted_at: Option<Duration>,
    stopped_at: Duration,
}

async fn spawn_terminal_poll_server(
    foreign_events: bool,
) -> TestResult<(
    String,
    tokio::task::JoinHandle<TestResult<TerminalPollLog>>,
    tokio::sync::oneshot::Sender<()>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (shutdown, mut stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let serve = async move {
            let began = Instant::now();
            let mut log = TerminalPollLog::default();
            let mut events: Option<TcpStream> = None;
            let mut beat = tokio::time::interval(Duration::from_millis(100));
            beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => {
                        log.stopped_at = began.elapsed();
                        break;
                    }
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted?;
                        let request = support::read_request(&mut stream).await?;
                        let line = request.lines().next().ok_or("request line absent")?;
                        let mut words = line.split_whitespace();
                        let method = words.next().ok_or("request method absent")?;
                        let path = words.next().ok_or("request path absent")?;
                        let received = began.elapsed();
                        if log.requests.len() >= 64 {
                            return Err("terminal-poll fixture exceeded its request bound".into());
                        }
                        if method == "GET" && path == "/api/event" {
                            if events.is_some() {
                                return Err("terminal-poll fixture unexpectedly resubscribed".into());
                            }
                            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await?;
                            events = Some(stream);
                        } else {
                            let body = match (method, path) {
                                ("POST", "/api/session/ses_a/prompt") => {
                                    log.admitted_at = Some(began.elapsed());
                                    PROMPT_ACCEPTED
                                }
                                ("GET", "/api/session/ses_a/permission") => NO_PERMISSIONS,
                                ("GET", "/api/session/active") => SETTLED,
                                ("GET", "/api/session/ses_a/message") => {
                                    r#"{"data":[{"type":"assistant","id":"msg_answer","time":{"created":1.0,"completed":2.0},"agent":"agt_fixture","model":{"providerID":"vendor","id":"model"},"finish":"stop","content":[{"type":"text","text":"done"}]},{"type":"user","id":"msg_user","time":{"created":0.5},"text":"hello"}],"cursor":{}}"#
                                }
                                _ => return Err(format!("unexpected terminal-poll request: {line}").into()),
                            };
                            stream.write_all(json_response(body).as_bytes()).await?;
                        }
                        log.requests.push((format!("{method} {path}"), received, began.elapsed()));
                    }
                    _ = beat.tick(), if foreign_events && log.admitted_at.is_some() && log.frames.len() < 60 => {
                        let ordinal = log.frames.len();
                        let frame = format!(
                            "event: message\ndata: {{\"type\":\"session.text.delta\",\"id\":\"evt_peer_{ordinal}\",\"created\":1.0,\"data\":{{\"sessionID\":\"ses_peer\",\"assistantMessageID\":\"msg_peer\",\"ordinal\":{ordinal},\"delta\":\".\"}}}}\n\n"
                        );
                        grapt_cli::AdvisoryEvent::decode_sse(frame.as_bytes(), CliLimits::production())?;
                        events.as_mut().ok_or("event stream absent")?.write_all(frame.as_bytes()).await?;
                        log.frames.push(began.elapsed());
                    }
                }
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(log)
        };
        tokio::time::timeout(Duration::from_secs(10), serve).await?
    });
    Ok((format!("http://{address}"), server, shutdown))
}

#[tokio::test]
async fn foreign_session_events_do_not_delay_an_inactive_runs_receipt() -> TestResult {
    let mut outcomes = Vec::new();
    // Run and join both controls before asserting either outcome.
    for foreign_events in [false, true] {
        let (endpoint, server, shutdown) = spawn_terminal_poll_server(foreign_events).await?;
        let mut output = BoundedOutput::new(
            Vec::new(),
            Vec::new(),
            OutputMode::Jsonl,
            CliLimits::production(),
        );
        let began = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(4), async {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                Runner::new(client(&endpoint)?)
                    .run_selected("ses_a".parse()?, 1, "hello".to_owned(), &mut output)
                    .await?,
            )
        })
        .await;
        let returned = began.elapsed();
        let stopped = shutdown.send(());
        let joined = server.await;
        eprintln!("foreign_events={foreign_events}, returned={returned:?}, joined={joined:?}");
        outcomes.push((foreign_events, result, returned, output, stopped, joined));
    }
    for (foreign_events, result, returned, output, stopped, joined) in outcomes {
        let log = joined??;
        stopped.map_err(|_| "terminal-poll server ended before joined cleanup")?;
        let admitted = log.admitted_at.ok_or("prompt was not admitted")?;
        if foreign_events {
            assert!(
                log.frames.len() >= 2,
                "foreign control delivered no paced traffic: {log:?}"
            );
            assert!(
                log.frames[0] - admitted < Duration::from_secs(1),
                "first frame was late: {log:?}"
            );
            assert!(
                log.frames
                    .windows(2)
                    .all(|pair| pair[1] - pair[0] < Duration::from_secs(1)),
                "foreign traffic had a poll-sized gap: {log:?}"
            );
            assert!(
                log.stopped_at - *log.frames.last().ok_or("foreign frame absent")?
                    < Duration::from_secs(1),
                "foreign traffic stopped before the run returned: {log:?}"
            );
        } else {
            assert!(
                log.frames.is_empty(),
                "quiet control was not quiet: {log:?}"
            );
        }
        let receipt = result.map_err(|_| format!(
            "inactive run did not finish within4s; foreign_events={foreign_events}, returned={returned:?}, log={log:?}"
        ))??;
        assert_eq!(receipt.exit, ExitClass::Success, "{log:?}");
        assert!(
            log.requests
                .iter()
                .any(|(request, _, _)| request == "GET /api/session/active"),
            "receipt had no activity read: {log:?}"
        );
        let (stdout, _, _) = output.into_parts();
        let records = String::from_utf8(stdout)?
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<Result<Vec<_>, _>>()?;
        assert!(
            records
                .iter()
                .filter_map(|record| record["data"].as_array())
                .flatten()
                .any(|row| row["id"] == "msg_answer"),
            "completed admitted-input answer was not emitted: {log:?}"
        );
    }
    Ok(())
}

/// A stdout whose reader stops draining after the first record begins.
#[derive(Debug)]
struct StalledReader {
    entered: Option<tokio::sync::oneshot::Sender<Instant>>,
    release: mpsc::Receiver<()>,
}

impl Write for StalledReader {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(entered) = self.entered.take() {
            let _entered_result = entered.send(Instant::now());
        }
        let _released = self.release.recv();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Releases the stalled reader when dropped: at the end of a witness, or from
/// its watchdog after [`STALLED_READER_RELEASE`] if the run could not be
/// cancelled while stalled.
#[derive(Debug)]
struct StalledReaderRelease {
    finished: Option<mpsc::Sender<()>>,
    watchdog: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StalledReaderRelease {
    fn drop(&mut self) {
        drop(self.finished.take());
        if let Some(watchdog) = self.watchdog.take() {
            let _joined = watchdog.join();
        }
    }
}

/// A run cancelled while its stdout reader has stopped draining. The reader
/// stays stalled until `release` is dropped.
struct StalledCancellation {
    result: Result<RunReceipt, RunError>,
    output_returned: bool,
    waited: Duration,
    server: tokio::task::JoinHandle<TestResult<Vec<String>>>,
    release: StalledReaderRelease,
}

async fn cancel_during_stalled_write(
    client: impl FnOnce(&str) -> TestResult<grapt_cli::ApiClient>,
) -> TestResult<StalledCancellation> {
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
        Script::json(
            "GET",
            "/api/session/active",
            200,
            r#"{"data":{"ses_a":{"type":"running"}}}"#,
        ),
        Script::status("POST", "/api/session/ses_a/interrupt", 200),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let (entered, stalled) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::channel::<()>();
    let (finished, watch) = mpsc::channel::<()>();
    let release = StalledReaderRelease {
        finished: Some(finished),
        watchdog: Some(std::thread::spawn(move || {
            let _finished = watch.recv_timeout(STALLED_READER_RELEASE);
            drop(release);
        })),
    };
    let output = BoundedOutput::new(
        StalledReader {
            entered: Some(entered),
            release: released,
        },
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let stalled_at = std::sync::Arc::new(std::sync::Mutex::new(None));
    let signal = {
        let stalled_at = std::sync::Arc::clone(&stalled_at);
        async move {
            if let (Ok(at), Ok(mut slot)) = (stalled.await, stalled_at.lock()) {
                *slot = Some(at);
            }
            Ok(CancellationCause::Sigint)
        }
    };
    let (result, output) = run_with_cancellation(
        Runner::new(client(&endpoint)?),
        session()?,
        "hello".to_owned(),
        output,
        signal,
    )
    .await;
    let returned = Instant::now();
    let stalled_at = stalled_at
        .lock()
        .map_err(|_| "stall instant lock poisoned")?
        .ok_or("the stalled reader was never entered")?;
    Ok(StalledCancellation {
        result,
        output_returned: output.is_some(),
        waited: returned.duration_since(stalled_at),
        server,
        release,
    })
}

// One worker, which the stalled write occupies: cancellation must not need a
// second one.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn cancellation_interrupts_the_session_while_stdout_is_back_pressured() -> TestResult {
    let stalled = cancel_during_stalled_write(|endpoint| Ok(client(endpoint)?)).await?;
    drop(stalled.release);
    let receipt = stalled.result?;
    assert_eq!(receipt.exit, ExitClass::Cancelled);
    assert_eq!(receipt.cancelled, Some(CancellationCause::Sigint));
    assert_eq!(
        receipt.cancellation_outcome,
        Some(CancellationOutcome::InterruptSettled)
    );
    assert!(
        !stalled.output_returned,
        "the output came back from a write its reader never drained"
    );
    assert!(
        stalled.waited <= CANCELLATION_HANDOFF + SCHEDULING_SLACK,
        "cancellation waited {:?} for a stalled stdout reader",
        stalled.waited
    );
    assert!(
        stalled
            .server
            .await??
            .contains(&"POST /api/session/ses_a/interrupt".to_owned()),
        "the Session was not interrupted"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_cancellation_blocked_by_stdout_still_ends_the_embedded_product() -> TestResult {
    // A long-lived stand-in for the embedded product: the CLI itself, waiting
    // on a stdin this witness keeps open. Spawned like the real embedded child.
    let mut product = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .args([
            "run",
            "--remote",
            "https://127.0.0.1:9/",
            "--credential",
            "absent",
            "--new",
            "--stdin",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let _held_stdin = product.stdin.take().ok_or("product stdin absent")?;
    let mut product_stdout = product.stdout.take().ok_or("product stdout absent")?;
    let pid = product.id().ok_or("product pid absent")?;
    let mut product = Some(product);
    let stalled = cancel_during_stalled_write(|endpoint| {
        let service = DiscoveredService::loopback_testkit(registration_for_endpoint(
            pid,
            endpoint,
            "instance-live",
        )?);
        let product = product.take().ok_or("product already embedded")?;
        match grapt_cli::ApiClient::from_embedded(&service, product, CliLimits::production()) {
            Ok(client) => Ok(client),
            Err((error, _child)) => Err(error.to_string().into()),
        }
    })
    .await?;
    assert!(
        !stalled.output_returned,
        "the output came back from a write its reader never drained"
    );
    // The run is still blocked in its write here. The product must end anyway,
    // exactly as it does when a run finishes normally.
    let mut remaining = Vec::new();
    let ended = tokio::time::timeout(PRODUCT_END, product_stdout.read_to_end(&mut remaining)).await;
    drop(stalled.release);
    assert!(
        matches!(ended, Ok(Ok(_))),
        "the embedded product outlived a cancelled run blocked in its write"
    );
    Ok(())
}

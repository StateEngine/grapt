//! CLI-LONG-TURN-SSE-01: routine event-stream ends against a scripted server.
//!
//! Every scripted event stream sends the `server.connected` handshake and then ends,
//! as a healthy stream does when the request allowance expires, only sooner. Each
//! cycle is exactly what the runner asks for: the handshake triggers a durable relist,
//! the stream end triggers another relist and an activity read, and an active session
//! is subscribed again.

use std::time::Duration;

use super::support::{spawn_server, Script};
use super::{client, TestResult};
use grapt_cli::{BoundedOutput, CliLimits, ExitClass, OutputMode, RunError, Runner};

const SESSION: &str = "ses_a";
const CONNECTED: &str =
    "event: message\ndata: {\"type\":\"server.connected\",\"id\":\"evt_connected\",\"data\":{}}\n\n";
const PROMPT: &str = r#"{"data":{"admittedSeq":1,"id":"msg_user","sessionID":"ses_a","timeCreated":1.0,"type":"user","data":{"text":"hello"},"delivery":"queue"}}"#;
const NO_PERMISSIONS: &str = r#"{"data":[],"hasMore":false}"#;
const RUNNING: &str = r#"{"data":{"ses_a":{"type":"running"}}}"#;
const SETTLED: &str = r#"{"data":{}}"#;
/// WINDOWS-CLI-SERVER-LOSS-01's bound on noticing a lost server.
const LOSS_BOUND: Duration = Duration::from_secs(31);

/// The admitted prompt as durable history holds it, under every step of the turn.
const USER: &str = r#"{"type":"user","id":"msg_user","time":{"created":0.5},"text":"hello"}"#;
/// The product's default history page, newest-first.
const PAGE_ROWS: usize = 50;

/// The turn's history after `steps` completed assistant steps, newest-first.
fn rows(steps: usize) -> Vec<String> {
    let mut rows: Vec<String> = (1..=steps)
        .rev()
        .map(|step| {
            format!(
                r#"{{"type":"assistant","id":"msg_step{step}","time":{{"created":{step}.0,"completed":{step}.0}},"agent":"agt_live_headless1","model":{{"id":"gpt-5.6","providerID":"openai"}},"content":[{{"type":"text","text":"step {step}","state":{{"durableBytes":6,"durableCapture":"complete","publicBoundBytes":262144,"publicProjection":"complete"}}}}],"finish":"stop","cost":0.0,"tokens":{{"input":1.0,"output":1.0,"reasoning":0.0,"cache":{{"read":0.0,"write":0.0}}}},"observedUsage":{{"inputTotal":1,"inputFresh":1,"output":1,"reasoning":0,"cacheRead":0,"cacheWrite":0,"total":2}}}}"#
            )
        })
        .collect();
    rows.push(USER.to_owned());
    rows
}

/// The durable message page after `steps` completed assistant steps.
fn page(steps: usize) -> String {
    format!(r#"{{"data":[{}],"cursor":{{}}}}"#, rows(steps).join(","))
}

/// A durable relist in text mode: the newest page and, once the turn outgrows
/// it, the older page its cursor points at, which holds the admitted prompt.
fn text_relist(scripts: &mut Vec<Script>, steps: usize) {
    let messages = format!("/api/session/{SESSION}/message");
    let rows = rows(steps);
    if rows.len() <= PAGE_ROWS {
        scripts.push(Script::json("GET", messages, 200, &page(steps)));
        return;
    }
    let cursor = format!("older{steps}");
    let (newest, older) = rows.split_at(PAGE_ROWS);
    scripts.push(Script::json(
        "GET",
        messages.clone(),
        200,
        &format!(
            r#"{{"data":[{}],"cursor":{{"next":"{cursor}"}}}}"#,
            newest.join(",")
        ),
    ));
    scripts.push(Script::json(
        "GET",
        format!("{messages}?cursor={cursor}"),
        200,
        &format!(r#"{{"data":[{}],"cursor":{{}}}}"#, older.join(",")),
    ));
}

/// One stream cycle: subscribe, relist on the handshake, relist again when the stream
/// ends, then read activity.
fn cycle(scripts: &mut Vec<Script>, steps: usize, active: bool) {
    let messages = format!("/api/session/{SESSION}/message");
    let permissions = format!("/api/session/{SESSION}/permission");
    scripts.push(Script::sse("GET", "/api/event", CONNECTED));
    for _relist in 0..2 {
        scripts.push(Script::json("GET", messages.clone(), 200, &page(steps)));
        scripts.push(Script::json(
            "GET",
            permissions.clone(),
            200,
            NO_PERMISSIONS,
        ));
    }
    scripts.push(Script::json(
        "GET",
        "/api/session/active",
        200,
        if active { RUNNING } else { SETTLED },
    ));
}

/// The prompt admission that follows the first subscription.
fn admission(scripts: &mut Vec<Script>) {
    scripts.push(Script::json(
        "POST",
        format!("/api/session/{SESSION}/prompt"),
        200,
        PROMPT,
    ));
}

async fn run(endpoint: &str) -> (Result<grapt_cli::RunReceipt, RunError>, String) {
    run_in(endpoint, OutputMode::Jsonl).await
}

async fn run_in(
    endpoint: &str,
    mode: OutputMode,
) -> (Result<grapt_cli::RunReceipt, RunError>, String) {
    let runner = match client(endpoint) {
        Ok(client) => Runner::new(client),
        Err(error) => return (Err(RunError::Api(error)), String::new()),
    };
    let mut output = BoundedOutput::new(Vec::new(), Vec::new(), mode, CliLimits::production());
    let session = match SESSION.parse() {
        Ok(session) => session,
        Err(_) => return (Err(RunError::NoSession), String::new()),
    };
    let result = runner
        .run_selected(session, 1, "hello".to_owned(), &mut output)
        .await;
    let (stdout, _stderr, _receipt) = output.into_parts();
    (result, String::from_utf8_lossy(&stdout).into_owned())
}

/// Many more routine stream ends than `MAX_SSE_RECONNECTS`, each with durable progress.
const LONG_TURN_CYCLES: usize = 24;

#[tokio::test]
async fn a_long_turn_survives_many_routine_stream_ends_while_it_progresses() -> TestResult {
    let mut scripts = Vec::new();
    for step in 1..=LONG_TURN_CYCLES {
        cycle(&mut scripts, step, step < LONG_TURN_CYCLES);
        if step == 1 {
            admission(&mut scripts);
        }
    }
    // The final durable read after terminal truth.
    scripts.push(Script::json(
        "GET",
        format!("/api/session/{SESSION}/message"),
        200,
        &page(LONG_TURN_CYCLES),
    ));
    let expected = scripts.len();
    let (endpoint, server) = spawn_server(scripts).await?;
    let (result, stdout) = run(&endpoint).await;
    let receipt =
        result.map_err(|error| format!("CLT101 a healthy long turn was refused: {error}"))?;
    assert_eq!(receipt.exit, ExitClass::Success);
    assert!(
        stdout.contains(&format!("\"msg_step{LONG_TURN_CYCLES}\"")),
        "the complete durable result was not emitted"
    );
    let served = server.await??;
    assert_eq!(
        served
            .iter()
            .filter(|request| *request == "GET /api/event")
            .count(),
        LONG_TURN_CYCLES,
        "every routine stream end was followed by a resubscription"
    );
    assert_eq!(served.len(), expected);
    Ok(())
}

/// Steps a text-mode long turn adds per stream cycle: its last cycles hold more
/// rows after the prompt than one history page.
const TEXT_STEPS_PER_CYCLE: usize = 3;

/// The default `run` format is text. A long text-mode turn keeps CLT's progress
/// signal through every routine stream end, and renders every step, although
/// its history outgrows the first page and the prompt drops off it.
#[tokio::test]
async fn a_long_text_turn_survives_stream_ends_and_renders_past_the_first_page() -> TestResult {
    let mut scripts = Vec::new();
    for cycle_number in 1..=LONG_TURN_CYCLES {
        let steps = cycle_number * TEXT_STEPS_PER_CYCLE;
        scripts.push(Script::sse("GET", "/api/event", CONNECTED));
        if cycle_number == 1 {
            admission(&mut scripts);
        }
        for _relist in 0..2 {
            text_relist(&mut scripts, steps);
            scripts.push(Script::json(
                "GET",
                format!("/api/session/{SESSION}/permission"),
                200,
                NO_PERMISSIONS,
            ));
        }
        scripts.push(Script::json(
            "GET",
            "/api/session/active",
            200,
            if cycle_number < LONG_TURN_CYCLES {
                RUNNING
            } else {
                SETTLED
            },
        ));
    }
    let steps = LONG_TURN_CYCLES * TEXT_STEPS_PER_CYCLE;
    assert!(steps > PAGE_ROWS, "the turn must outgrow one history page");
    text_relist(&mut scripts, steps);
    let expected = scripts.len();
    let (endpoint, server) = spawn_server(scripts).await?;
    let (result, stdout) = run_in(&endpoint, OutputMode::Text).await;
    let receipt =
        result.map_err(|error| format!("a healthy long text turn was refused: {error}"))?;
    assert_eq!(receipt.exit, ExitClass::Success);
    let answer: String = (1..=steps).map(|step| format!("step {step}\n")).collect();
    assert_eq!(
        stdout, answer,
        "CCM112 the text turn lost steps past the first page"
    );
    let served = server.await??;
    assert_eq!(
        served
            .iter()
            .filter(|request| *request == "GET /api/event")
            .count(),
        LONG_TURN_CYCLES,
        "every routine stream end was followed by a resubscription"
    );
    assert_eq!(served.len(), expected);
    Ok(())
}

#[tokio::test]
async fn a_server_that_stays_up_without_progress_is_still_refused() -> TestResult {
    // The same durable page every cycle: the session stays active and never progresses.
    let mut scripts = Vec::new();
    for cycle_number in 1..=9 {
        cycle(&mut scripts, 1, true);
        if cycle_number == 1 {
            admission(&mut scripts);
        }
    }
    let (endpoint, server) = spawn_server(scripts).await?;
    let (result, _stdout) = run(&endpoint).await;
    match result {
        Err(error @ RunError::SseReconnectBound { maximum: 8 }) => {
            assert_eq!(error.exit_class(), ExitClass::Remote);
            assert!(error.to_string().contains("without durable progress"));
        }
        other => return Err(format!("a stalled server was not refused: {other:?}").into()),
    }
    let served = server.await??;
    assert_eq!(
        served
            .iter()
            .filter(|request| *request == "GET /api/event")
            .count(),
        9,
        "the first handshake plus 8 reconnects, then the bound"
    );
    Ok(())
}

#[tokio::test]
async fn a_lost_server_still_exits_remote_within_the_loss_bound() -> TestResult {
    // Three progressing cycles; then the server is gone and every connection is refused.
    let mut scripts = Vec::new();
    for step in 1..=3 {
        cycle(&mut scripts, step, true);
        if step == 1 {
            admission(&mut scripts);
        }
    }
    let (endpoint, server) = spawn_server(scripts).await?;
    let outcome = tokio::time::timeout(LOSS_BOUND, run(&endpoint)).await;
    let Ok((result, _stdout)) = outcome else {
        return Err("CLT102 a lost server was not noticed within the loss bound".into());
    };
    match result {
        Err(error @ RunError::Api(_)) => assert_eq!(error.exit_class(), ExitClass::Remote),
        other => {
            return Err(format!("a lost server did not end the run as Remote: {other:?}").into())
        }
    }
    server.await??;
    Ok(())
}

#[tokio::test]
async fn resumed_jsonl_does_not_republish_large_history_on_each_step() -> TestResult {
    // The typed message decoder requires the discriminator first.
    let old = format!(
        r#"{{"type":"user","id":"msg_old","time":{{"created":0.0}},"text":{}}}"#,
        serde_json::to_string(&"old history ".repeat(4096))?,
    );
    let mut scripts = Vec::new();
    let mut last = String::new();
    for step in 0..=12 {
        let mut messages = rows(step);
        messages.push(old.clone());
        last = format!(r#"{{"data":[{}],"cursor":{{}}}}"#, messages.join(","));
        scripts.push(Script::sse("GET", "/api/event", CONNECTED));
        for _ in 0..2 {
            scripts.push(Script::json(
                "GET",
                "/api/session/ses_a/message",
                200,
                &last,
            ));
            scripts.push(Script::json(
                "GET",
                "/api/session/ses_a/permission",
                200,
                NO_PERMISSIONS,
            ));
        }
        scripts.push(Script::json(
            "GET",
            "/api/session/active",
            200,
            if step == 12 { SETTLED } else { RUNNING },
        ));
    }
    admission(&mut scripts);
    scripts.push(Script::json(
        "GET",
        "/api/session/ses_a/message",
        200,
        &last,
    ));
    let (endpoint, server) = spawn_server(scripts).await?;
    let limits = CliLimits::try_new(grapt_cli::CliLimitValues {
        stdout_bytes: 128 * 1024,
        ..CliLimits::production().values()
    })?;
    let mut output = BoundedOutput::new(Vec::new(), Vec::new(), OutputMode::Jsonl, limits);
    let receipt = Runner::new(client(&endpoint)?)
        .run_selected(SESSION.parse()?, 1, "hello".into(), &mut output)
        .await?;
    assert_eq!(receipt.exit, ExitClass::Success);
    server.await??;
    Ok(())
}

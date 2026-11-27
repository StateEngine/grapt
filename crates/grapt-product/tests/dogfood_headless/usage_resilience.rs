//! Incomplete provider accounting cannot terminate a usable CLI conversation.
use super::*;
use serde_json::{json as value, Value};

const USAGE_FIELDS: &[&str] = &[
    "inputTotal",
    "inputFresh",
    "output",
    "reasoning",
    "cacheRead",
    "cacheWrite",
    "total",
];

#[derive(Clone, Copy)]
enum Route {
    Responses,
    Compatible,
    Anthropic,
}

impl Route {
    fn select(self, address: SocketAddr, session: &str) {
        if matches!(self, Self::Anthropic) {
            context_quality::select_anthropic(address, session);
        }
        if matches!(self, Self::Compatible) {
            let (status, body) = request(
                address,
                "POST",
                &format!("/api/session/{session}/model"),
                r#"{"model":{"providerID":"openrouter","id":"fixture-usage"}}"#,
            );
            assert_eq!(
                status,
                204,
                "compatible model: {}",
                String::from_utf8_lossy(&body)
            );
        }
    }

    fn counts(self, optional: Value) -> Value {
        match self {
            Self::Responses => value!({
                "input_tokens":11, "output_tokens":4, "total_tokens":15,
                "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":optional},
                "output_tokens_details":{"reasoning_tokens":optional},
            }),
            Self::Compatible => value!({
                "prompt_tokens":11, "completion_tokens":4, "total_tokens":15,
                "prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":optional},
                "completion_tokens_details":{"reasoning_tokens":optional},
            }),
            Self::Anthropic => value!({
                "input_tokens":11, "output_tokens":0,
                "cache_read_input_tokens":0, "cache_creation_input_tokens":optional,
            }),
        }
    }

    fn malformed(self) -> Value {
        match self {
            Self::Responses => value!({"input_tokens":{"invalid":"not-a-count"}}),
            Self::Compatible => value!({"prompt_tokens":{"invalid":"not-a-count"}}),
            Self::Anthropic => value!({"input_tokens":{"invalid":"not-a-count"}}),
        }
    }

    fn response(self, text: &str, usage: Value) -> Script {
        let frames = match self {
            Self::Responses => vec![
                value!({"type":"response.output_text.delta","delta":text}),
                value!({"type":"response.output_text.done"}),
                value!({"type":"response.completed","response":{"usage":usage}}),
            ],
            Self::Anthropic => return context_quality::anthropic_text(text, usage),
            Self::Compatible => vec![
                value!({"choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":null}]}),
                value!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                value!({"choices":[],"usage":usage}),
            ],
        };
        let mut stream: String = frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect();
        if matches!(self, Self::Compatible) {
            stream.push_str("data: [DONE]\n\n");
        }
        Script::Raw(stream)
    }
}

fn run_prompt(root: &Path, session: &str, prompt: &str, answer: &str) {
    let service = root.to_string_lossy().into_owned();
    let run = cli_continuity::cli(
        root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            session,
            "--prompt",
            prompt,
            "--jsonl",
        ],
    );
    assert_eq!(
        run.code,
        Some(0),
        "CLI failed: {}\n{}",
        run.stderr,
        run.stdout
    );
    assert!(
        run.stdout.contains(answer),
        "CLI lost streamed answer: {}",
        run.stdout
    );
    let receipt = run.receipt();
    assert_eq!(receipt["session"], session);
    assert_eq!(receipt["prompted"], true);
}

fn readable_history(address: SocketAddr, session: &str, answers: &[&str]) -> Vec<Value> {
    let info = json(address, "GET", &format!("/api/session/{session}"), "");
    assert_eq!(info["data"]["id"], session);
    assert!(
        info["data"].get("tokens").is_none(),
        "unknown session accounting became aggregate counts: {info}"
    );
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=20"),
        "",
    );
    let rows = page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history rows absent: {page}"));
    let assistants: Vec<Value> = rows
        .iter()
        .filter(|row| row["type"] == "assistant")
        .cloned()
        .collect();
    assert_eq!(
        assistants.len(),
        answers.len(),
        "wrong assistant count: {page}"
    );
    for (row, answer) in assistants.iter().zip(answers) {
        assert!(
            row.to_string().contains(*answer),
            "durable answer missing: {row}"
        );
        assert!(
            row.get("error").is_none(),
            "accounting failed the turn: {row}"
        );
        assert!(
            row["time"]["completed"].is_number(),
            "turn did not settle: {row}"
        );
    }
    assistants
}

fn unknown(row: &Value, fields: &[&str]) {
    let observed = &row["observedUsage"];
    assert!(observed.is_object(), "usage observation absent: {row}");
    for field in fields {
        assert!(
            observed.get(*field).is_none(),
            "unknown {field} became a count: {observed}"
        );
    }
    assert!(
        row.get("tokens").is_none(),
        "incomplete usage became complete counters: {row}"
    );
}

fn genuine_zero(row: &Value, route: Route) {
    let observed = &row["observedUsage"];
    let fields: &[&str] = match route {
        Route::Anthropic => &["cacheWrite"],
        Route::Responses | Route::Compatible => &["cacheWrite", "reasoning"],
    };
    for field in fields {
        assert_eq!(
            observed[*field].as_u64(),
            Some(0),
            "reported zero was lost: {observed}"
        );
    }
    assert_eq!(
        observed["output"].as_u64(),
        Some(if matches!(route, Route::Anthropic) {
            3
        } else {
            4
        }),
        "valid usage disappeared: {observed}"
    );
}

fn prepare_profile(root: &Path, route: Route) {
    prepare(root, 120_000);
    // Capacity accommodates the reconstructed input and the output reserve.
    provider_profile::prepare_model_inventory(root, 256_000);
    if matches!(route, Route::Compatible) {
        compatible_cache::configure_openrouter(root, "fixture-usage");
        fs::write(
            root.join("openrouter-account-models.json"),
            value!({"models":[{"slug":"fixture-usage","visibility":"list",
                "context_window":256_000,"supported_reasoning_levels":[]}]})
            .to_string(),
        )
        .unwrap_or_else(|error| panic!("compatible inventory write failed: {error}"));
    }
    if matches!(route, Route::Anthropic) {
        context_quality::configure_anthropic(root, 256_000);
    }
}

fn conversation(label: &str, route: Route, usage: Value, unknown_fields: &[&str]) {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare_profile(&root, route);
    let provider = ProviderFixture::start(vec![
        route.response("first answer with unknown usage", usage),
        route.response("answer after unknown usage", route.counts(value!(0))),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    route.select(address, &session);
    let answers = [
        "first answer with unknown usage",
        "answer after unknown usage",
    ];

    run_prompt(&root, &session, "first task", answers[0]);
    let first = readable_history(address, &session, &answers[..1]);
    unknown(&first[0], unknown_fields);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "usage triggered a retry"
    );

    run_prompt(&root, &session, "next task", answers[1]);
    let second = readable_history(address, &session, &answers);
    unknown(&second[0], unknown_fields);
    genuine_zero(&second[1], route);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "next prompt did not use one attempt"
    );
    child.stop_after_witness(&root);
    drop(child);

    let mut reopened = spawn(&root, provider.address);
    let address = await_ready(&root, &mut reopened);
    let history = readable_history(address, &session, &answers);
    unknown(&history[0], unknown_fields);
    genuine_zero(&history[1], route);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "reads dispatched another request"
    );
    reopened.stop_after_witness(&root);
}

#[test]
fn responses_null_optional_usage_keeps_the_cli_session_usable() {
    conversation(
        "responses-null-usage",
        Route::Responses,
        Route::Responses.counts(Value::Null),
        &["cacheWrite", "reasoning", "inputFresh"],
    );
}

#[test]
fn compatible_null_optional_usage_keeps_the_cli_session_usable() {
    conversation(
        "compatible-null-usage",
        Route::Compatible,
        Route::Compatible.counts(Value::Null),
        &["cacheWrite", "reasoning", "inputFresh"],
    );
}

#[test]
fn compatible_reconstructed_null_cache_write_keeps_the_cli_session_usable() {
    // Numeric observations reconstructed from diagnostics; the null leaf is
    // inferred from the diagnosis, not a captured provider frame.
    conversation(
        "compatible-reconstructed-null-cache",
        Route::Compatible,
        value!({
            "prompt_tokens":107728, "completion_tokens":900, "total_tokens":108628,
            "prompt_tokens_details":{"cached_tokens":107008,"cache_write_tokens":null},
            "completion_tokens_details":{"reasoning_tokens":612},
        }),
        &["cacheWrite", "inputFresh"],
    );
}

#[test]
fn responses_reconstructed_null_cache_write_keeps_the_cli_session_usable() {
    // The same reconstructed observations expressed on the Responses route.
    conversation(
        "responses-reconstructed-null-cache",
        Route::Responses,
        value!({
            "input_tokens":107728, "output_tokens":900, "total_tokens":108628,
            "input_tokens_details":{"cached_tokens":107008,"cache_write_tokens":null},
            "output_tokens_details":{"reasoning_tokens":612},
        }),
        &["cacheWrite", "inputFresh"],
    );
}

#[test]
fn responses_malformed_usage_remains_unknown_without_failing_the_cli_turn() {
    conversation(
        "responses-malformed-usage",
        Route::Responses,
        Route::Responses.malformed(),
        USAGE_FIELDS,
    );
}

#[test]
fn compatible_malformed_usage_remains_unknown_without_failing_the_cli_turn() {
    conversation(
        "compatible-malformed-usage",
        Route::Compatible,
        Route::Compatible.malformed(),
        USAGE_FIELDS,
    );
}

#[test]
fn responses_string_count_keeps_usage_unknown_and_the_cli_session_usable() {
    conversation(
        "responses-string-count",
        Route::Responses,
        Route::Responses.counts(value!("12")),
        USAGE_FIELDS,
    );
}

#[test]
fn compatible_string_count_keeps_usage_unknown_and_the_cli_session_usable() {
    conversation(
        "compatible-string-count",
        Route::Compatible,
        Route::Compatible.counts(value!("12")),
        USAGE_FIELDS,
    );
}

#[test]
fn responses_excess_cache_partition_keeps_usage_unknown_and_the_cli_session_usable() {
    let mut usage = Route::Responses.counts(value!(4));
    usage["input_tokens_details"]["cached_tokens"] = value!(8);
    conversation(
        "responses-excess-cache",
        Route::Responses,
        usage,
        USAGE_FIELDS,
    );
}

#[test]
fn compatible_excess_cache_partition_keeps_usage_unknown_and_the_cli_session_usable() {
    let mut usage = Route::Compatible.counts(value!(4));
    usage["prompt_tokens_details"]["cached_tokens"] = value!(8);
    conversation(
        "compatible-excess-cache",
        Route::Compatible,
        usage,
        USAGE_FIELDS,
    );
}

#[test]
fn responses_inconsistent_total_keeps_usage_unknown_and_the_cli_session_usable() {
    let mut usage = Route::Responses.counts(value!(0));
    usage["total_tokens"] = value!(16);
    conversation(
        "responses-inconsistent-total",
        Route::Responses,
        usage,
        USAGE_FIELDS,
    );
}

#[test]
fn compatible_inconsistent_total_keeps_usage_unknown_and_the_cli_session_usable() {
    let mut usage = Route::Compatible.counts(value!(0));
    usage["total_tokens"] = value!(16);
    conversation(
        "compatible-inconsistent-total",
        Route::Compatible,
        usage,
        USAGE_FIELDS,
    );
}

#[test]
fn anthropic_decreasing_snapshot_clears_usage_and_keeps_the_cli_session_usable() {
    let mut usage = Route::Anthropic.counts(value!(0));
    // The fixture's terminal delta reports three output tokens after this snapshot.
    usage["output_tokens"] = value!(9);
    conversation(
        "anthropic-decreasing-usage",
        Route::Anthropic,
        usage,
        USAGE_FIELDS,
    );
}

fn diagnostic_script(route: Route, oversized: bool) -> Script {
    let mut usage = route.counts(value!(0));
    let (input, secret) = match route {
        Route::Compatible => ("prompt_tokens", "openrouter-secret"),
        Route::Responses => ("input_tokens", "provider-secret"),
        Route::Anthropic => panic!("diagnostic fixture requires a supported wire route"),
    };
    usage[input] = value!("12");
    usage["diagnostic_tag"] = value!("first-usage-snapshot");
    usage["credential_echo"] = value!(secret);
    if oversized {
        usage["extra"] = value!("large diagnostic field ".repeat(256));
    }
    let frames = match route {
        Route::Compatible => vec![
            value!({"choices":[], "usage":usage, "vendor_content":"surrounding-private-frame"}),
            value!({"choices":[], "usage":{"prompt_tokens":"bad", "diagnostic_tag":"later-usage-snapshot"}}),
            value!({"choices":[{"index":0,"delta":{"role":"assistant","content":"diagnostic answer"},"finish_reason":null}]}),
            value!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            value!({"choices":[], "usage":route.counts(value!(0))}),
        ],
        Route::Responses => vec![
            value!({"type":"response.output_text.delta","delta":"diagnostic answer"}),
            value!({"type":"response.output_text.done"}),
            value!({"type":"response.completed","response":{"usage":usage,"vendor_content":"surrounding-private-frame"}}),
        ],
        Route::Anthropic => panic!("diagnostic fixture requires a supported wire route"),
    };
    let mut stream: String = frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect();
    if matches!(route, Route::Compatible) {
        stream.push_str("data: [DONE]\n\n");
    }
    Script::Raw(stream)
}

fn await_diagnostic(path: &Path, ready: impl Fn(&str) -> bool) -> String {
    let wait = PollDeadline::new();
    wait.poll(Duration::from_millis(20), || {
        match fs::read_to_string(path) {
            Ok(text) => ready(&text).then_some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("diagnostic read failed: {error}"),
        }
    })
    .unwrap_or_else(|| panic!("diagnostic not ready ({wait}): {}", path.display()))
}

fn diagnostic_conversation(label: &str, route: Route, enabled: bool, oversized: bool) {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare_profile(&root, route);
    if enabled {
        let config = root.join("service.conf");
        let mut text = fs::read_to_string(&config)
            .unwrap_or_else(|error| panic!("service config read failed: {error}"));
        text.push_str("provider_error_text=true\n");
        fs::write(config, text)
            .unwrap_or_else(|error| panic!("service config write failed: {error}"));
    }
    let provider = ProviderFixture::start(vec![
        diagnostic_script(route, oversized),
        route.response("answer after unknown usage", route.counts(value!(0))),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    route.select(address, &session);
    run_prompt(&root, &session, "first task", "diagnostic answer");
    let first = readable_history(address, &session, &["diagnostic answer"]);
    unknown(&first[0], USAGE_FIELDS);
    let log = root.join("diagnostics/provider-errors.jsonl");
    let text = await_diagnostic(&log, |text| {
        text.ends_with('\n') && text.contains("usage_detail")
    });
    let records: Vec<Value> = text
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("diagnostic JSON invalid: {error}"))
        })
        .collect();
    assert_eq!(records.len(), 1, "usage warning repeated: {text}");
    assert_eq!(records[0]["usage_warning"]["issue"], "wrong-type");
    let usage = &records[0]["usage_detail"]["usage"];
    assert!(usage.to_string().len() <= grapt_provider::ERROR_TEXT_BYTES);
    assert!(!text.contains("provider-secret") && !text.contains("openrouter-secret"));
    assert!(!text.contains("surrounding-private-frame"));
    assert!(!text.contains("later-usage-snapshot"));
    if oversized {
        assert_eq!(usage["truncated"], true, "oversize was not marked: {text}");
    } else {
        assert_eq!(usage["diagnostic_tag"], "first-usage-snapshot");
        assert!(
            text.contains("[redacted]"),
            "credential echo was not masked: {text}"
        );
    }
    let private = root.join("logs/provider-error.txt");
    if enabled {
        let saved = await_diagnostic(&private, |text| serde_json::from_str::<Value>(text).is_ok());
        assert!(saved.len() <= grapt_provider::TERMINAL_DETAIL_BYTES);
        assert!(!saved.contains("provider-secret") && !saved.contains("openrouter-secret"));
        assert!(!saved.contains("surrounding-private-frame"));
        let saved: Value = serde_json::from_str(&saved)
            .unwrap_or_else(|error| panic!("private diagnostic JSON invalid: {error}"));
        if oversized {
            assert_eq!(saved["usage"]["truncated"], true);
        } else {
            assert_eq!(saved["usage"]["diagnostic_tag"], "first-usage-snapshot");
        }
    } else {
        assert!(!private.exists(), "last-error copy ignored its opt-in");
    }
    run_prompt(&root, &session, "next task", "answer after unknown usage");
    let history = readable_history(
        address,
        &session,
        &["diagnostic answer", "answer after unknown usage"],
    );
    unknown(&history[0], USAGE_FIELDS);
    genuine_zero(&history[1], route);
    let public = history[0].to_string();
    assert!(!public.contains("first-usage-snapshot") && !public.contains("credential_echo"));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "diagnosis changed provider attempts"
    );
    child.stop_after_witness(&root);
}

#[test]
fn compatible_invalid_usage_records_only_the_first_masked_snapshot() {
    diagnostic_conversation(
        "compatible-usage-diagnostic",
        Route::Compatible,
        true,
        false,
    );
}

#[test]
fn responses_invalid_usage_records_masked_usage_without_private_opt_in() {
    diagnostic_conversation("responses-usage-diagnostic", Route::Responses, false, false);
}

#[test]
fn compatible_oversized_usage_diagnostic_is_explicitly_bounded() {
    diagnostic_conversation(
        "compatible-large-usage-diagnostic",
        Route::Compatible,
        true,
        true,
    );
}

#[test]
fn responses_oversized_usage_diagnostic_is_explicitly_bounded() {
    diagnostic_conversation(
        "responses-large-usage-diagnostic",
        Route::Responses,
        true,
        true,
    );
}

fn mixed_error_terminal(label: &str, error_type: &str, retry: bool) -> grapt_session::StepTerminal {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare_profile(&root, Route::Compatible);
    let frame = value!({"usage":{"prompt_tokens":"12"},
        "error":{"type":error_type,"message":"private-provider-error-text"}});
    let provider = ProviderFixture::start(vec![
        Script::Raw(format!("data: {frame}\n\n")),
        Route::Compatible.response(
            "answer after primary error",
            Route::Compatible.counts(value!(0)),
        ),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    Route::Compatible.select(address, &session);
    let service = root.to_string_lossy().into_owned();
    let run = cli_continuity::cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "first task",
            "--jsonl",
        ],
    );
    assert_eq!(
        run.code == Some(0),
        retry,
        "CLI outcome: {}\n{}",
        run.stdout,
        run.stderr
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        if retry { 2 } else { 1 },
        "accounting changed the primary error's retry decision"
    );
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=20"),
        "",
    );
    assert!(!history.to_string().contains("private-provider-error-text"));
    if retry {
        assert!(history.to_string().contains("answer after primary error"));
    }
    child.stop_after_witness(&root);
    drop(child);
    let store = EventStore::open(&root.join("database.sqlite3"), "mixed-error-evidence")
        .unwrap_or_else(|error| panic!("history open failed: {error}"));
    let session = session
        .parse::<SessionId>()
        .unwrap_or_else(|error| panic!("invalid session identity: {error}"));
    let history = grapt_session::SessionHistoryStore::over(&store)
        .fold(&session)
        .unwrap_or_else(|error| panic!("history fold failed: {error}"));
    let messages = history
        .messages()
        .unwrap_or_else(|error| panic!("messages failed: {error}"));
    assert_eq!(messages.len(), 1, "retry created another assistant message");
    messages[0]
        .terminal
        .clone()
        .unwrap_or_else(|| panic!("Step did not settle"))
}

#[test]
fn compatible_bad_usage_preserves_a_retryable_in_band_error() {
    let terminal = mixed_error_terminal("mixed-usage-retry-error", "server_error", true);
    assert!(
        matches!(
            terminal,
            grapt_session::StepTerminal::Ended {
                completion: grapt_types::ProviderCompletionDisposition::AssertedComplete
            }
        ),
        "the primary error did not recover: {terminal:?}"
    );
}

#[test]
fn compatible_bad_usage_preserves_a_terminal_authentication_error() {
    let terminal = mixed_error_terminal("mixed-usage-auth-error", "authentication_error", false);
    assert!(
        matches!(
            terminal,
            grapt_session::StepTerminal::Failed {
                reason: grapt_types::StepTerminalReason::ProviderFailure {
                    class: grapt_types::ProviderFailureClass::Authentication
                }
            }
        ),
        "usage masked the primary error: {terminal:?}"
    );
}

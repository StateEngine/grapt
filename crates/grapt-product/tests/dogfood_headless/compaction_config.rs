//! Oracle-parity `compaction` config, through the real drain: `buffer` moves
//! the threshold, `auto: false` turns the threshold off, and `keep` is
//! accepted with no effect (docs/coding-quality/COMPACTION-CONFIG.md).
use super::*;

const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";
const READ_BYTES: usize = 36_000;
const READS: [(&str, &str); 2] = [("call_config_a", "a.txt"), ("call_config_b", "b.txt")];
/// Admits the first request (instructions and tool schemas) but not the
/// 72 000 bytes the grouped reads return; SE's default admits both.
const BUFFER: u64 = 128_000 - 16_384 - 30_000;

/// Adds `compaction`'s keys to the fixture's own grapt.jsonc, beside the
/// legacy server_side key. The scripted provider explicitly refuses compaction.
fn configure(root: &Path, compaction: serde_json::Value) {
    set_compaction(root, compaction);
}

/// Descriptor triggers committed so far, read-only.
fn triggers(root: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("store open failed: {error}"));
    let mut query = db
        .prepare(
            "SELECT json_extract(data, '$.trigger') FROM event
             WHERE type = 'session.compaction.ended.2' ORDER BY seq",
        )
        .unwrap_or_else(|error| panic!("descriptor query failed: {error}"));
    query
        .query_map([], |row| row.get::<_, String>(0))
        .and_then(|rows| rows.collect())
        .unwrap_or_else(|error| panic!("descriptor read failed: {error}"))
}

/// Applies platform config to a journey's fresh root.
type Platform = Box<dyn Fn(&Path)>;

thread_local! {
    /// Platform config a journey applies to its fresh root before the service
    /// starts (provider family, web search). Thread-local: a journey runs on
    /// its test's own thread, so one test's platform never reaches another.
    static PLATFORM: std::cell::RefCell<Option<Platform>> =
        const { std::cell::RefCell::new(None) };
    /// Each committed descriptor's selection `summaryPlan`, from the last
    /// journey on this thread, read before its root was cleaned up.
    static PLANS: std::cell::RefCell<Vec<serde_json::Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Each committed descriptor's selection `serverSide`, likewise.
    static SERVER: std::cell::RefCell<Vec<serde_json::Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Distinct summary calls with terminal usage in the last journey.
    static SUMMARY_USAGE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Whether the next journey's route sends thread headers: only ChatGPT
    /// does, so a platform on another route clears it.
    static THREAD_HEADERS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// The hosted-search journeys (SC-CACHE-07).
#[path = "compaction_config/hosted.rs"]
mod hosted;

/// Each committed compaction's selection `summaryPlan` and `serverSide`
/// (null when absent).
fn summary_plans(root: &Path) -> Vec<(serde_json::Value, serde_json::Value)> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("store open failed: {error}"));
    let mut query = db
        .prepare(
            "SELECT json_extract(data, '$.retained.descriptor.id') FROM event
             WHERE type = 'session.compaction.ended.2' ORDER BY seq",
        )
        .unwrap_or_else(|error| panic!("descriptor query failed: {error}"));
    let ids: Vec<String> = query
        .query_map([], |row| row.get(0))
        .and_then(|rows| rows.collect())
        .unwrap_or_else(|error| panic!("descriptor read failed: {error}"));
    ids.iter()
        .map(|id| {
            let object = root
                .join("dogfood-artifacts")
                .join("objects")
                .join(&id[..2])
                .join(id);
            let selection: serde_json::Value = serde_json::from_slice(
                &fs::read(&object).unwrap_or_else(|error| panic!("selection read failed: {error}")),
            )
            .unwrap_or_else(|error| panic!("selection decode failed: {error}"));
            (
                selection["summaryPlan"].clone(),
                selection["serverSide"].clone(),
            )
        })
        .collect()
}

/// One tool Step reads two 36 000-byte files against a 128 000-token budget,
/// then answers. Returns the provider bodies, descriptor triggers and calls.
fn journey(
    label: &str,
    compaction: Option<serde_json::Value>,
    scripts: Vec<Script>,
) -> (Vec<String>, Vec<String>, usize) {
    journey_in(label, compaction, scripts, false)
}

/// As [`journey`]; with `outside`, the Session works in a project directory
/// beside the service root rather than in it, as the benchmark's does.
fn journey_in(
    label: &str,
    compaction: Option<serde_json::Value>,
    scripts: Vec<Script>,
    outside: bool,
) -> (Vec<String>, Vec<String>, usize) {
    let (bodies, triggers, calls, _, _) = journey_with_history(label, compaction, scripts, outside);
    (bodies, triggers, calls)
}

/// As [`journey_in`], also returning the Session's public history and how
/// many summary calls it durably started.
fn journey_with_history(
    label: &str,
    compaction: Option<serde_json::Value>,
    scripts: Vec<Script>,
    outside: bool,
) -> (Vec<String>, Vec<String>, usize, serde_json::Value, usize) {
    let (bodies, triggers, calls, history, summary_calls, _) =
        journey_sized(label, compaction, scripts, outside, READ_BYTES, None, None);
    (bodies, triggers, calls, history, summary_calls)
}

/// As [`journey_with_history`], each read file `read_bytes` long. A measured
/// provider script names its own seed files and calls; other scripts use READS.
#[expect(clippy::too_many_lines, reason = "legacy")]
fn journey_sized(
    label: &str,
    compaction: Option<serde_json::Value>,
    scripts: Vec<Script>,
    outside: bool,
    read_bytes: usize,
    then: Option<&str>,
    window: Option<u64>,
) -> (
    Vec<String>,
    Vec<String>,
    usize,
    serde_json::Value,
    usize,
    String,
) {
    let _journey = process_journey_custody();
    let root = root(label);
    let reads = match scripts.first() {
        Some(Script::ToolReadsMeasured { calls, .. }) => calls.clone(),
        _ => READS.to_vec(),
    };
    prepare(&root, 120_000);
    if let Some(window) = window {
        prepare_model_inventory(&root, window);
    }
    let provider_compacts = compaction
        .as_ref()
        .is_some_and(|config| config["server_side"] == true);
    if let Some(compaction) = compaction {
        configure(&root, compaction);
    }
    PLATFORM.with(|platform| {
        if let Some(platform) = platform.borrow_mut().take() {
            platform(&root);
        }
    });
    let project = if outside {
        let project = root.with_extension("project");
        fs::create_dir_all(&project)
            .unwrap_or_else(|error| panic!("project directory failed: {error}"));
        project
    } else {
        root.clone()
    };
    for &(_, path) in &reads {
        fs::write(project.join(path), text_lines(b'x', read_bytes))
            .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    }
    let provider = if provider_compacts {
        ProviderFixture::start_compacting(scripts)
    } else {
        ProviderFixture::start(scripts)
    };
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = if outside {
        let body = serde_json::json!({"location": {"directory": project}}).to_string();
        json(address, "POST", "/api/session", &body)["data"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("created session had no id"))
            .to_owned()
    } else {
        create(address)
    };
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_config_seed","text":"read the seed","delivery":"queue","resume":true}"#,
    );
    let calls: Vec<_> = reads.iter().map(|(call, _)| *call).collect();
    compaction_continuity::approve_batch(address, &session, &calls);
    permission_residuals::wait_session_idle(address, &session);
    // An optional next prompt, after the first has settled.
    if let Some(text) = then {
        let body = serde_json::json!({"id":"msg_config_then","text":text,"delivery":"queue","resume":true});
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &body.to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    child.stop_after_witness(&root);
    drop(child);
    let calls = provider.calls.load(Ordering::SeqCst);
    let bodies = locked(&provider.bodies).clone();
    let threads = THREAD_HEADERS.with(|threads| threads.replace(true));
    for head in locked(&provider.heads).iter() {
        if threads {
            cache_scope::assert_thread_headers(head, &session);
        } else {
            assert!(
                !head.to_ascii_lowercase().contains("thread-id:"),
                "a route without thread headers sent one: {head}"
            );
        }
    }
    let triggers = triggers(&root);
    // This helper exercises SE text summaries after an explicit unsupported
    // provider-compaction response; those physical calls remain durable.
    let summary_calls = summary_calls(&root)
        .checked_sub(locked(&provider.unsupported_compactions).len())
        .unwrap_or_else(|| panic!("missing durable provider compaction call"));
    let (plans, server): (Vec<_>, Vec<_>) = summary_plans(&root).into_iter().unzip();
    PLANS.with(|last| *last.borrow_mut() = plans);
    SERVER.with(|last| *last.borrow_mut() = server);
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("summary accounting open failed: {error}"));
    let terminal_usage = db
        .query_row(
            "SELECT COUNT(DISTINCT json_extract(data, '$.attempt')) FROM event
         WHERE type = 'session.compaction.call.usage.1'
         AND json_extract(data, '$.usage.phase') = 'terminal'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or_else(|error| panic!("summary accounting read failed: {error}"));
    SUMMARY_USAGE_CALLS.with(|calls| {
        calls.set(
            usize::try_from(terminal_usage)
                .unwrap_or_else(|error| panic!("invalid summary usage count: {error}")),
        )
    });
    drop(db);
    let stderr = fs::read_to_string(root.join("product-stderr-0.log")).unwrap_or_default();
    process_fixture::cleanup(&root);
    if outside {
        let _ = fs::remove_dir_all(&project);
    }
    (bodies, triggers, calls, history, summary_calls, stderr)
}

/// Summary calls durably started, read-only.
fn summary_calls(root: &Path) -> usize {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("store open failed: {error}"));
    db.query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.compaction.call.started.1'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| usize::try_from(count).unwrap_or_default())
    .unwrap_or_else(|error| panic!("summary call count failed: {error}"))
}

fn seed_read() -> Script {
    Script::ToolReads {
        calls: READS.to_vec(),
    }
}

/// The same grouped reads stay under SE's default threshold...
#[test]
fn without_config_the_reads_do_not_compact() {
    let (bodies, triggers, calls) = journey(
        "config-default",
        None,
        vec![seed_read(), Script::SuccessText("answered")],
    );
    assert_eq!(calls, 2);
    assert!(triggers.is_empty(), "{triggers:?}");
    assert!(!bodies.iter().any(|body| body.contains(SUMMARY_REQUEST)));
}

/// ...but a buffer that leaves 30 000 tokens of input moves the threshold
/// below it: the Step after the reads compacts first.
#[test]
fn a_configured_buffer_moves_the_threshold() {
    let buffer = BUFFER;
    let (bodies, triggers, calls) = journey(
        "config-buffer",
        Some(serde_json::json!({"buffer": buffer})),
        vec![
            seed_read(),
            Script::SuccessText("## Objective\n- the seed was read"),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert_eq!(calls, 3);
    assert!(
        bodies[1].contains(SUMMARY_REQUEST),
        "the threshold did not move"
    );
    assert_eq!(triggers, vec!["policyThreshold".to_owned()]);
}

/// The benchmark's shape: the buffer is set only in the service root's
/// config and the Session works in a project outside that root. The root
/// config is the base every Session's sources fold over, so it still moves
/// the threshold.
#[test]
fn a_buffer_in_the_service_root_config_applies_to_a_session_outside_it() {
    let (bodies, triggers, calls) = journey_in(
        "config-root-buffer",
        Some(serde_json::json!({"buffer": BUFFER})),
        vec![
            seed_read(),
            Script::SuccessText("## Objective\n- the seed was read"),
            Script::SuccessText("answered after compaction"),
        ],
        true,
    );
    assert_eq!(calls, 3);
    assert!(
        bodies[1].contains(SUMMARY_REQUEST),
        "the root config's buffer did not reach a Session outside the root"
    );
    assert_eq!(triggers, vec!["policyThreshold".to_owned()]);
}

/// `auto: false` turns that threshold off: the same buffer, no compaction.
#[test]
fn auto_false_turns_the_threshold_off() {
    let buffer = BUFFER;
    let (bodies, triggers, calls) = journey(
        "config-auto-off",
        Some(serde_json::json!({"auto": false, "buffer": buffer})),
        vec![seed_read(), Script::SuccessText("answered")],
    );
    assert_eq!(calls, 2);
    assert!(triggers.is_empty(), "{triggers:?}");
    assert!(!bodies.iter().any(|body| body.contains(SUMMARY_REQUEST)));
}

/// By ruling, an oracle config carrying `keep` loads, and
/// `keep` changes nothing.
#[test]
fn keep_tokens_load_and_change_nothing() {
    let (bodies, triggers, calls) = journey(
        "config-keep",
        Some(serde_json::json!({"keep": {"tokens": 2_000}})),
        vec![seed_read(), Script::SuccessText("answered")],
    );
    assert_eq!(calls, 2);
    assert!(triggers.is_empty(), "{triggers:?}");
    assert!(!bodies.iter().any(|body| body.contains(SUMMARY_REQUEST)));
}

/// The buffer decides when to compact, not whether the turn may run (Sonnet
/// C04): a 60 000-byte summary leaves the rebuilt request far
/// above the buffer's 30 000 tokens but inside the model's window, so it
/// commits, nothing new is compacted again, and the turn is answered.
#[test]
fn a_summary_over_the_buffer_still_commits_and_the_turn_continues() {
    let buffer = BUFFER;
    let summary = format!("## Objective\n- {}", "y".repeat(60_000 - 15));
    let (bodies, triggers, calls) = journey(
        "config-buffer-rebuilt",
        Some(serde_json::json!({"buffer": buffer})),
        vec![
            seed_read(),
            Script::SuccessText(Box::leak(summary.into_boxed_str())),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert!(
        bodies[1].contains(SUMMARY_REQUEST),
        "compaction was not tried"
    );
    assert_eq!(
        triggers,
        vec!["policyThreshold".to_owned()],
        "the summary did not commit, or a second compaction followed it"
    );
    assert_eq!(calls, 3, "the turn did not continue after the summary");
    assert!(
        bodies[2].contains("yyyy") && !bodies[2].contains(SUMMARY_REQUEST),
        "the continued request does not carry the summary"
    );
}

/// A configured policy is never silently replaced: once a project config
/// source cannot be read (here it is a directory, not a file), the chain is
/// refused instead of running with the defaults in place of `auto: false`.
#[test]
fn an_unreadable_config_source_refuses_the_chain_instead_of_defaulting() {
    let _journey = process_journey_custody();
    let root = root("config-unreadable");
    prepare(&root, 120_000);
    configure(&root, serde_json::json!({"auto": false, "buffer": BUFFER}));
    let provider = ProviderFixture::start(vec![Script::SuccessText("answered with defaults")]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    fs::create_dir_all(root.join(".grapt").join("grapt.jsonc"))
        .unwrap_or_else(|error| panic!("unreadable source setup failed: {error}"));
    let _ = request(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_config_unreadable","text":"hello","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    let calls = provider.calls.load(Ordering::SeqCst);
    process_fixture::cleanup(&root);
    assert_eq!(calls, 0, "the chain ran on default compaction settings");
}

/// `keep.tokens` is accepted with no effect, and the Session is told so once:
/// two prompts read the same configuration, and history names the key one
/// time. The notice is the user's; it never reaches the provider.
#[test]
fn an_ignored_keep_tokens_is_named_once_per_session() {
    let _journey = process_journey_custody();
    let root = root("config-keep-notice");
    prepare(&root, 120_000);
    configure(&root, serde_json::json!({"keep": {"tokens": 2000}}));
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first answer"),
        Script::SuccessText("second answer"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    for (id, text) in [("msg_keep_one", "first"), ("msg_keep_two", "second")] {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id": id, "text": text, "delivery": "queue", "resume": true})
                .to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);

    let rows = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"));
    let named: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["metadata"]["configIgnored"] == true)
        .collect();
    assert_eq!(
        named.len(),
        1,
        "the key was not named exactly once: {history}"
    );
    assert_eq!(named[0]["metadata"]["key"], "compaction.keep.tokens");
    assert_eq!(
        named[0]["text"],
        "Config key compaction.keep.tokens is accepted but not applied; SE uses its own recent-context retention."
    );
    assert_eq!(bodies.len(), 2, "both prompts were answered once each");
    assert!(
        !bodies
            .iter()
            .any(|body| body.contains("is accepted but not applied")),
        "the notice reached the provider"
    );
}

/// A summary with no text is never committed: the compaction fails as no
/// progress, once, by name, and the Step stops. Nothing replaces usable
/// history.
#[test]
fn an_empty_summary_is_refused_and_named() {
    let (bodies, triggers, calls, history, summary_calls) = journey_with_history(
        "config-summary-refused",
        Some(serde_json::json!({"buffer": BUFFER})),
        vec![
            seed_read(),
            Script::SuccessText(""),
            Script::SuccessText("answered with no summary"),
        ],
        false,
    );
    assert_eq!(calls, 2, "the empty summary was retried");
    assert!(bodies[1].contains(SUMMARY_REQUEST));
    assert!(
        triggers.is_empty(),
        "a refused summary committed: {triggers:?}"
    );
    assert_eq!(summary_calls, 1, "one summary call, no retry");
    assert!(
        history["data"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["metadata"]["executionFailure"] == true)),
        "the stopped execution is not visible in the history: {history}"
    );
    // And the Session says why, and what to do next.
    let notices: Vec<&serde_json::Value> = history["data"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["metadata"]["compactionFailed"] == true)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        notices.len(),
        1,
        "no single compaction-failed notice: {history}"
    );
    assert_eq!(
        notices[0]["text"],
        "Couldn't summarize the conversation to free up context (the model's summary was unusable). Your history is intact. Try again, or start a new session from here."
    );
}

/// A summary cut at the size bound is not a summary: the compaction fails by
/// name with nothing committed, as an empty one does, rather than installing
/// the text that arrived before the cut.
#[test]
fn an_oversize_summary_is_refused_and_nothing_is_committed() {
    let (bodies, triggers, calls, history, summary_calls) = journey_with_history(
        "config-summary-oversize",
        Some(serde_json::json!({"buffer": BUFFER})),
        vec![
            seed_read(),
            Script::Output(80_000),
            Script::SuccessText("answered with no summary"),
        ],
        false,
    );
    assert_eq!((calls, summary_calls), (2, 1), "one call, no retry");
    assert!(bodies[1].contains(SUMMARY_REQUEST));
    assert!(
        triggers.is_empty(),
        "a truncated summary committed: {triggers:?}"
    );
    assert!(
        history.to_string().contains("compactionFailed"),
        "the refusal is not visible in the history: {history}"
    );
}

/// Fresh-token slice #1: SE's own summary call is
/// cache-warm. The matched C04 Luna pairs spent 123k of 313k fresh input on
/// three summary calls that cached nothing; the provider arm's cached all but
/// ~5k each. The summary request is the coding request it follows, with the
/// same instructions and tools, and one last user message: the template and
/// the task restated. Tools stay offered for the cached prefix but are not
/// callable (`tool_choice: none`).
#[test]
fn the_summary_request_extends_the_coding_request_it_follows() {
    let (bodies, triggers, calls) = journey(
        "config-warm-summary",
        Some(serde_json::json!({"buffer": BUFFER})),
        vec![
            seed_read(),
            Script::SuccessText("## Objective\n- read the seed files"),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert_eq!(calls, 3);
    assert_eq!(triggers, vec!["policyThreshold".to_owned()]);
    assert_eq!(
        PLANS.with(|plans| plans.borrow().clone()),
        vec![serde_json::json!("warm")]
    );
    let body = |index: usize| -> serde_json::Value {
        serde_json::from_str(&bodies[index])
            .unwrap_or_else(|error| panic!("request {index} is not JSON: {error}"))
    };
    let (sent, summary) = (body(0), body(1));
    assert_eq!(
        summary["instructions"], sent["instructions"],
        "instructions differ"
    );
    assert_eq!(summary["tools"], sent["tools"], "tools differ");
    assert_eq!(
        summary["tool_choice"], "none",
        "the summary call may call tools"
    );
    let sent_input = sent["input"]
        .as_array()
        .unwrap_or_else(|| panic!("no input"));
    let input = summary["input"]
        .as_array()
        .unwrap_or_else(|| panic!("no summary input"));
    assert!(
        input.len() > sent_input.len() && input[..sent_input.len()] == sent_input[..],
        "the summary request does not extend the request last sent:\nsent {sent_input:#?}\nsummary {input:#?}"
    );
    let last = input
        .last()
        .unwrap_or_else(|| panic!("empty summary input"));
    assert_eq!(
        last["role"], "user",
        "the summary task is not the last user message: {last}"
    );
    let task = last.to_string();
    assert!(
        task.contains("Output exactly the Markdown structure"),
        "the last message does not carry the template: {task}"
    );
}

/// The cold request: the history quoted as data, the summary's own framing.
const COLD_SUMMARY: &str = "as quoted JSON data";

/// A summary request offering the coding request's tools is the warm one.
fn warm(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|body| {
        body["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty())
    })
}

/// A summary response that calls a tool is refused before any effect and the
/// call is ignored; with no text left the compaction fails as an empty
/// summary. (The fixture answers with a call even though the warm request sent
/// tool_choice none, as an Anthropic route, which keeps its coding tool_choice,
/// may.)
#[test]
fn a_summary_that_calls_a_tool_never_runs_it() {
    let (_, triggers, calls, history, summary_calls) = journey_with_history(
        "config-warm-tool",
        Some(serde_json::json!({"buffer": BUFFER})),
        vec![
            seed_read(),
            Script::ToolRead {
                call: "call_summary_tool",
                path: "a.txt",
            },
            Script::SuccessText("answered after compaction"),
        ],
        false,
    );
    assert_eq!((calls, summary_calls), (2, 1));
    assert!(triggers.is_empty(), "{triggers:?}");
    assert!(
        !history.to_string().contains("call_summary_tool"),
        "the summary's tool call reached the Session: {history}"
    );
}

/// The warm request is judged at its own size. Reads
/// too large for the window leave a coding request, and so a warm summary,
/// that does not fit; the cold request, whose quoted outputs are cut to a
/// prefix, does, and the compaction commits.
#[test]
fn a_summary_too_large_to_send_warm_is_sent_cold() {
    let (bodies, triggers, calls, _, summary_calls, stderr) = journey_sized(
        "config-warm-too-large",
        None,
        vec![
            seed_read(),
            Script::SuccessText("## Objective\n- read the seed files"),
            Script::SuccessText("answered after compaction"),
        ],
        false,
        300_000,
        None,
        Some(100_000),
    );
    assert_eq!(summary_calls, 1, "{triggers:?}");
    assert_eq!(calls, 3, "{stderr}");
    assert_eq!(
        triggers.len(),
        1,
        "the compaction did not commit: {triggers:?}"
    );
    assert!(
        !warm(&bodies[1]) && bodies[1].contains(COLD_SUMMARY),
        "an oversized coding request was sent warm"
    );
    assert_eq!(
        PLANS.with(|plans| plans.borrow().clone()),
        vec![serde_json::json!("cold:warm-over-budget")]
    );
}

/// Condition (a): the C04 shape itself. The latest instruction is "Reply only
/// ACK." and it is what crosses the threshold, so the warm request ends with it
/// and then the summary task. The original instruction stays verbatim, once,
/// after the summary.
#[test]
fn a_latest_instruction_to_reply_only_ack_stays_verbatim_after_the_summary() {
    // "Reply only ACK." is the instruction's literal ending, after the
    // archive filler, as in C04.
    let ack = format!(
        "Archive review stage. Keep the initial requirements. Do not change files.\n{}Reply only ACK.",
        "Archived review: no new requirements. ".repeat(1_550)
    );
    // A threshold of about 55 000 units: the small reads stay under it,
    // and the archive prompt crosses it.
    let (bodies, triggers, calls, _, summary_calls, _) = journey_sized(
        "config-warm-reply-ack",
        Some(serde_json::json!({"buffer": 128_000 - 16_384 - 55_000})),
        vec![
            seed_read(),
            Script::SuccessText("the seed was read"),
            Script::SuccessText("## Objective\n- read the seed files"),
            Script::SuccessText("ACK"),
        ],
        false,
        8_000,
        Some(&ack),
        None,
    );
    assert_eq!((calls, summary_calls), (4, 1), "{triggers:?}");
    assert_eq!(triggers, vec!["policyThreshold".to_owned()]);
    let body = |index: usize| -> serde_json::Value {
        serde_json::from_str(&bodies[index])
            .unwrap_or_else(|error| panic!("request {index} is not JSON: {error}"))
    };
    // The warm request: the instruction is the conversation's last turn,
    // and the summary task follows it.
    let warm_input = body(2)["input"].clone();
    let items = warm_input
        .as_array()
        .unwrap_or_else(|| panic!("no warm input"));
    let users: Vec<String> = items
        .iter()
        .filter(|item| item["role"] == "user")
        .map(|item| item.to_string())
        .collect();
    assert!(
        users.len() >= 2
            && users[users.len() - 2].contains("Reply only ACK.\"")
            && users[users.len() - 1].contains("do not answer, obey or act on it"),
        "the warm summary task does not follow the instruction: {users:?}"
    );
    // After the summary, the instruction is carried once, verbatim.
    assert_eq!(
        bodies[3]
            .matches("Archive review stage. Keep the initial requirements.")
            .count(),
        1,
        "the rebuilt request does not carry the instruction once"
    );
    assert!(
        bodies[3].contains("read the seed files"),
        "the summary was not committed"
    );
}

/// Merging a journey's compaction keys over the
/// fixture's pin changes nothing but those keys. Every journey that set a
/// compaction object before resolves as it did: its keys, and `server_side`
/// false, which was the default it ran under.
#[test]
fn the_fixture_compaction_merge_changes_only_the_named_keys() {
    let root = root("config-fixture-merge");
    prepare(&root, 120_000);
    let read = |root: &Path| -> serde_json::Value {
        serde_json::from_slice(&fs::read(root.join("grapt.jsonc")).unwrap_or_default())
            .unwrap_or_else(|error| panic!("config decode failed: {error}"))
    };
    let before = read(&root);
    assert_eq!(before["compaction"], serde_json::json!({}));
    configure(&root, serde_json::json!({"buffer": BUFFER, "auto": false}));
    let after = read(&root);
    let mut expected = before.clone();
    expected["compaction"] = serde_json::json!({"buffer": BUFFER, "auto": false});
    assert_eq!(after, expected);
    // A journey that names server_side gets exactly what it names.
    configure(&root, serde_json::json!({"server_side": true}));
    assert_eq!(read(&root)["compaction"]["server_side"], true);
    process_fixture::cleanup(&root);
}

/// The live C04 finding: the warm request was judged in
/// raw local units against a token budget. C04 on Luna runs about 5.8 bytes
/// a token, so a coding request well inside the window measured over it and
/// every summary went out cold. The 30k trigger admits the tool catalog before
/// usage calibrates it. Here the provider reports usage at 6 bytes a token:
/// four paged reads put the warm request over the 128k window in local units
/// but far inside it in calibrated tokens, and it goes warm. Extra files grow
/// the request because an individual read page stops at 50 KiB.
#[test]
fn a_warm_summary_is_judged_in_calibrated_tokens_not_local_units() {
    let (bodies, triggers, calls, _, summary_calls, stderr) = journey_sized(
        "config-warm-calibrated",
        Some(serde_json::json!({"buffer": 128_000 - 16_384 - 30_000})),
        vec![
            Script::ToolReadsMeasured {
                calls: vec![
                    ("call_config_a", "a.txt"),
                    ("call_config_b", "b.txt"),
                    ("call_config_c", "c.txt"),
                    ("call_config_d", "d.txt"),
                ],
                bytes_per_token: 6,
            },
            Script::SuccessText("## Objective\n- read the seed files"),
            Script::SuccessText("answered after compaction"),
        ],
        false,
        60_000,
        None,
        None,
    );
    assert_eq!((calls, summary_calls), (3, 1), "{triggers:?}\n{stderr}");
    assert_eq!(triggers, vec!["policyThreshold".to_owned()]);
    assert!(
        warm(&bodies[1]) && !bodies[1].contains(COLD_SUMMARY),
        "the summary went cold: {}",
        stderr
            .lines()
            .filter(|line| line.contains("summary plan"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(stderr.contains("summary plan: warm"), "{stderr}");
}

/// With no calibration yet (no provider usage observed
/// before this Session's first compaction) the summary request is judged on
/// the raw estimate, and when the warm request does not fit, the plan line
/// names that basis.
#[test]
fn an_uncalibrated_warm_summary_falls_back_cold_and_says_why() {
    let _journey = process_journey_custody();
    let root = root("config-warm-uncalibrated");
    prepare(&root, 120_000);
    prepare_model_inventory(&root, WARM_UNCALIBRATED_WINDOW);
    // Disable the optional threshold: missing usage must still reach the ceiling.
    configure(&root, serde_json::json!({"buffer": 4_096, "auto": false}));
    // An answer whose stream reports no usage, so nothing calibrates.
    let unmetered = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"noted\"}\n\n\
         data: {\"type\":\"response.output_text.done\"}\n\n\
         data: {\"type\":\"response.completed\",\"response\":{}}\n\n";
    let provider = ProviderFixture::start(vec![
        Script::Raw(unmetered.to_owned()),
        Script::SuccessText("## Objective\n- keep the archive"),
        Script::SuccessText("answered after compaction"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    for (id, text) in [
        ("msg_uncal_one", "first half"),
        ("msg_uncal_two", "second half"),
    ] {
        let text = format!(
            "Archive, {text}: {}",
            "an uncalibrated archive line. ".repeat(950)
        );
        let body = serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true});
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &body.to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies).clone();
    let stderr = fs::read_to_string(root.join("product-stderr-0.log")).unwrap_or_default();
    let plans: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("summary plan"))
        .collect();
    let committed = triggers(&root);
    process_fixture::cleanup(&root);
    assert_eq!(
        committed.len(),
        1,
        "the compaction did not commit: {plans:?}"
    );
    assert_eq!(plans.len(), 1, "{plans:?}");
    assert!(
        plans[0].contains("summary plan: cold")
            && plans[0].contains("uncalibrated: no provider usage observed yet"),
        "the plan did not name the raw basis: {}",
        plans[0]
    );
    assert!(bodies[1].contains(COLD_SUMMARY), "the summary was not cold");
}

/// The window for the uncalibrated witness: the cold request (the first half
/// quoted as history plus the labelled latest instruction) fits it; the warm
/// one, which also carries the instructions and tool schemas, does not. The
/// rebuilt coding request after the summary carries the whole tool catalog
/// too, so the window leaves it room as the catalog grows (the terminal tools
/// put it over at 80k) while the warm request stays well over.
const WARM_UNCALIBRATED_WINDOW: u64 = 88_000;

#[path = "compaction_config/recovery.rs"]
mod recovery;

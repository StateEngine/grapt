//! `compaction.server_side` through the real drain: the
//! same compaction decision sends a compaction_trigger on the Codex route,
//! commits the provider's opaque item, carries it in the next request and
//! shows a "compacted by provider" row; any failure falls back to SE's own
//! summary with the reason recorded; off by default.
use super::*;

const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";
const READ_BYTES: usize = 36_000;
const READS: [(&str, &str); 2] = [("call_sc_a", "a.txt"), ("call_sc_b", "b.txt")];
/// Three more rounds in the same turn, each compacting again: a long text a
/// summary must carry (tool output it projects to a prefix), then reads of
/// other files (rereading what a compaction summarized is refused as no
/// progress). Over SE's summary budget by the fourth compaction, the text
/// cut SE's own summary could carry stops advancing.
const MORE_ROUNDS: [[(&str, &str); 2]; 3] = [
    [("call_sc_c", "c.txt"), ("call_sc_d", "d.txt")],
    [("call_sc_e", "e.txt"), ("call_sc_f", "f.txt")],
    [("call_sc_g", "g.txt"), ("call_sc_h", "h.txt")],
];
const ROUND_TEXT_BYTES: usize = 60_000;
/// As compaction_config's buffer: the Step after the grouped reads compacts.
const BUFFER: u64 = 128_000 - 16_384 - 30_000;
const COMPACTED: &str = "SC_PROVIDER_COMPACTED";

fn compaction_stream() -> Script {
    compaction_stream_reporting(19_000)
}

/// The compaction stream, reporting `input` confirmed input tokens. Usage
/// calibrates later estimates; a report at or above the estimate leaves
/// them at the uncalibrated scale every round of a journey started at.
fn compaction_stream_reporting(input: u64) -> Script {
    Script::Raw(format!(
        "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"compaction\",\"id\":\"cmp_sc\",\"encrypted_content\":\"{COMPACTED}\"}}}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":{input},\"output_tokens\":300,\"total_tokens\":{}}}}}}}\n\n",
        input + 300
    ))
}

/// Four compaction items whose states total over the 320 KiB capture bound
/// only through a 100 000-byte field each that the replay ignores
/// (the review's counterexample).
fn padded_compaction_stream() -> Script {
    let padding = "p".repeat(100_000);
    let mut stream = String::new();
    for index in 0..4 {
        stream.push_str(&format!(
            "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"compaction\",\"id\":\"cmp_sc_{index}\",\"encrypted_content\":\"{COMPACTED}\",\"padding\":\"{padding}\"}}}}\n\n"
        ));
    }
    stream.push_str("data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":19000,\"output_tokens\":300,\"total_tokens\":19300}}}\n\n");
    Script::Raw(stream)
}

struct Journey {
    bodies: Vec<String>,
    /// Durable events outside compaction that name the compaction's tool call.
    tool_events: Vec<String>,
    /// (descriptor summary kind, selection serverSide JSON) per committed compaction.
    compactions: Vec<(String, serde_json::Value)>,
    /// The cut each committed compaction retains from, in commit order.
    froms: Vec<u64>,
    history: String,
    observed_calls: serde_json::Value,
}

fn journey(label: &str, compaction: serde_json::Value, scripts: Vec<Script>) -> Journey {
    journey_with_window(label, compaction, scripts, None)
}

fn journey_with_window(
    label: &str,
    compaction: serde_json::Value,
    scripts: Vec<Script>,
    window: Option<u64>,
) -> Journey {
    journey_with_guidance(label, compaction, scripts, window, None)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn journey_with_guidance(
    label: &str,
    compaction: serde_json::Value,
    scripts: Vec<Script>,
    window: Option<u64>,
    instructions: Option<&str>,
) -> Journey {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 120_000);
    if let Some(window) = window {
        prepare_model_inventory(&root, window);
        // This fixture's buffer is calculated from the admitted window,
        // rather than a listing's default 95 percent allowance.
        let listing = root.join("account-models.json");
        let mut account: serde_json::Value = serde_json::from_slice(
            &fs::read(&listing).unwrap_or_else(|error| panic!("listing read failed: {error}")),
        )
        .unwrap_or_else(|error| panic!("listing decode failed: {error}"));
        account["models"][0]["effective_context_window_percent"] = 100.into();
        fs::write(
            listing,
            serde_json::to_vec(&account)
                .unwrap_or_else(|error| panic!("listing encode failed: {error}")),
        )
        .unwrap_or_else(|error| panic!("listing write failed: {error}"));
    }
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("config read failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("config decode failed: {error}"));
    config["compaction"] = compaction;
    fs::write(&path, serde_json::to_vec(&config).unwrap_or_default())
        .unwrap_or_else(|error| panic!("config write failed: {error}"));
    // Every read the scripts ask for has its file, and is approved in order.
    let reads: Vec<(&str, &str)> = scripts
        .iter()
        .flat_map(|script| match script {
            Script::ToolReads { calls } | Script::TextThenReads { calls, .. } => calls.clone(),
            _ => Vec::new(),
        })
        .collect();
    for (_, file) in &reads {
        fs::write(root.join(file), text_lines(b'x', READ_BYTES))
            .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    }
    let calls: Vec<&str> = reads.iter().map(|(call, _)| *call).collect();
    let provider = ProviderFixture::start_compacting(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_sc_seed","text":"read the seed","delivery":"queue","resume":true}"#,
    );
    for call in calls {
        compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    if let Some(instructions) = instructions {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/compact"),
            &serde_json::json!({"instructions":instructions}).to_string(),
        );
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"continue after compact"}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?diagnostics=cache"),
        "",
    )["data"]
        .clone();
    let observed_calls = history
        .as_array()
        .and_then(|rows| {
            rows.iter().find_map(|row| {
                (row["type"] == "assistant")
                    .then(|| row["metadata"]["cacheObservedCalls"].clone())
                    .filter(|calls| !calls.is_null())
            })
        })
        .unwrap_or(serde_json::Value::Null);
    let history = history.to_string();
    child.stop_after_witness(&root);
    drop(child);
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("store open failed: {error}"));
    let mut query = db
        .prepare(
            "SELECT json_extract(data, '$.summary.kind'),
                    json_extract(data, '$.retained.descriptor.id')
             FROM event WHERE type = 'session.compaction.ended.2' ORDER BY seq",
        )
        .unwrap_or_else(|error| panic!("descriptor query failed: {error}"));
    let rows: Vec<(String, String)> = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .and_then(|rows| rows.collect())
        .unwrap_or_else(|error| panic!("descriptor read failed: {error}"));
    let events: Vec<String> = db
        .prepare("SELECT type || ' ' || substr(coalesce(data,''),1,300) FROM event WHERE type LIKE 'session.step.failed%' OR type LIKE 'session.compaction%' OR type LIKE 'session.execution.failed%' ORDER BY seq")
        .and_then(|mut query| query.query_map([], |row| row.get(0)).and_then(|rows| rows.collect()))
        .unwrap_or_else(|error| panic!("event read failed: {error}"));
    let history = format!("{history}\nEVENTS {events:#?}");
    let tool_events: Vec<String> = db
        .prepare("SELECT type FROM event WHERE data LIKE '%call_sc_compaction_tool%' AND type NOT LIKE 'session.compaction%'")
        .and_then(|mut query| query.query_map([], |row| row.get(0)).and_then(|rows| rows.collect()))
        // A failed read must not pass as "no tool effects".
        .unwrap_or_else(|error| panic!("tool event read failed: {error}"));
    let mut froms = Vec::new();
    let compactions = rows
        .into_iter()
        .map(|(kind, selection)| {
            let object = root
                .join("dogfood-artifacts")
                .join("objects")
                .join(&selection[..2])
                .join(&selection);
            let selection: serde_json::Value = serde_json::from_slice(
                &fs::read(&object).unwrap_or_else(|error| panic!("selection read failed: {error}")),
            )
            .unwrap_or_else(|error| panic!("selection decode failed: {error}"));
            froms.push(
                selection["from"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("selection has no cut: {selection}")),
            );
            (kind, selection["serverSide"].clone())
        })
        .collect();
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);
    Journey {
        bodies,
        tool_events,
        compactions,
        froms,
        history,
        observed_calls,
    }
}

fn seed_read() -> Script {
    Script::ToolReads {
        calls: READS.to_vec(),
    }
}

fn input(body: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(body)
        .unwrap_or_else(|error| panic!("body is not JSON: {error}"))["input"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The 2026-10-09 decision, witness (a): with no
/// `server_side` key, an OpenAI Responses route compacts on the provider.
#[test]
fn provider_compaction_is_on_by_default_on_an_openai_route() {
    let journey = journey(
        "sc-default-on",
        serde_json::json!({"buffer": BUFFER}),
        vec![
            seed_read(),
            compaction_stream(),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert!(
        journey.bodies[1].contains("compaction_trigger"),
        "the default did not compact on the provider"
    );
    assert_eq!(journey.compactions.len(), 1);
    assert_eq!(journey.compactions[0].0, "providerBound");
}

#[test]
fn manual_guidance_reaches_provider_compaction_without_entering_the_next_coding_request() {
    let journey = journey_with_guidance(
        "sc-manual-guidance",
        serde_json::json!({"auto":false}),
        vec![
            seed_read(),
            Script::SuccessText("seeded"),
            compaction_stream(),
            Script::SuccessText("continued"),
        ],
        None,
        Some("Preserve the provider decision"),
    );
    assert!(journey.bodies[2].contains("compaction_trigger"));
    assert!(journey.bodies[2].contains("Preserve the provider decision"));
    assert!(!journey.bodies[3].contains("Preserve the provider decision"));
    assert_eq!(journey.compactions.len(), 1);
    assert_eq!(journey.compactions[0].0, "providerBound");
}

/// The legacy opt-out cannot disable the provider-first policy.
#[test]
fn an_explicit_server_side_false_cannot_bypass_openai_compaction() {
    let journey = journey(
        "sc-explicit-off",
        serde_json::json!({"buffer": BUFFER, "server_side": false}),
        vec![
            seed_read(),
            compaction_stream(),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert!(journey.bodies[1].contains("compaction_trigger"));
    assert_eq!(journey.compactions[0].0, "providerBound");
}

/// Witness (c): by default a provider that refuses compaction falls back to
/// SE's own summary in the same Step, the reason recorded, and the turn
/// completes.
#[test]
fn a_refused_default_provider_compaction_falls_back_and_the_turn_completes() {
    let journey = journey(
        "sc-default-refused",
        serde_json::json!({"buffer": BUFFER}),
        vec![
            seed_read(),
            Script::Refused {
                status: 400,
                body: r#"{"error":{"message":"compaction unsupported","type":"invalid_request_error"}}"#,
            },
            Script::SuccessText("## Objective\n- the seed was read"),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert!(journey.bodies[1].contains("compaction_trigger"));
    assert!(
        journey.bodies[2].contains(SUMMARY_REQUEST),
        "SE did not fall back"
    );
    assert_eq!(journey.compactions.len(), 1);
    assert_eq!(journey.compactions[0].0, "clientReadable");
    assert!(
        journey.compactions[0].1.to_string().contains("fallback"),
        "the fallback reason was not recorded: {}",
        journey.compactions[0].1
    );
    assert_eq!(journey.bodies.len(), 4, "the turn did not continue");
    assert_eq!(
        journey.observed_calls["attempts"], 4,
        "coding, refused provider compaction, summary fallback and resumed coding each count once"
    );
    assert_eq!(
        journey.observed_calls["unreported"], 4,
        "these fixtures have no cache-read leaf, including the refused call"
    );
}

#[test]
fn provider_compaction_commits_its_item_and_the_next_request_carries_it() {
    let journey = journey(
        "sc-committed",
        serde_json::json!({"buffer": BUFFER, "server_side": true}),
        vec![
            seed_read(),
            compaction_stream(),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert_eq!(
        journey.bodies.len(),
        3,
        "reads, provider compaction, answer: {}",
        journey.history
    );
    let trigger = input(&journey.bodies[1]);
    assert_eq!(
        trigger.last(),
        Some(&serde_json::json!({"type":"compaction_trigger"})),
        "the compaction request ends with the trigger"
    );
    // The compaction request repeats the coding request: the first request's
    // input is its prefix, so the provider's cached prefix still applies.
    let first = input(&journey.bodies[0]);
    assert_eq!(trigger[..first.len()], first[..]);
    assert!(
        !journey
            .bodies
            .iter()
            .any(|body| body.contains("context_management")),
        "one trigger policy"
    );
    let tools = |body: &str| -> String {
        serde_json::from_str::<serde_json::Value>(body)
            .unwrap_or_else(|error| panic!("body is not JSON: {error}"))["tools"]
            .to_string()
    };
    assert_eq!(
        tools(&journey.bodies[1]),
        tools(&journey.bodies[0]),
        "the compaction request offers the coding request's tools byte for byte"
    );
    assert!(!journey.bodies[1].contains(SUMMARY_REQUEST));
    assert_eq!(journey.compactions.len(), 1);
    assert_eq!(journey.compactions[0].0, "providerBound");
    assert_eq!(journey.compactions[0].1["outcome"], "committed");
    assert_eq!(journey.compactions[0].1["inputTokens"], 19000);
    assert_eq!(
        journey.observed_calls["attempts"], 3,
        "provider-native compaction belongs beside its two coding calls"
    );
    assert_eq!(
        journey.observed_calls["unreported"], 3,
        "missing cache-read leaves stay unreported for all three calls"
    );
    let next = input(&journey.bodies[2]);
    assert!(
        next.contains(
            &serde_json::json!({"type":"compaction","id":"cmp_sc","encrypted_content":COMPACTED})
        ),
        "the next request carries the provider's item verbatim"
    );
    assert!(!journey.bodies[2].contains("compaction_trigger"));
    assert!(
        journey
            .history
            .contains("Compacted by the provider: 1 opaque item(s) from 19000 input tokens"),
        "history shows the provider compaction: {}",
        journey.history
    );
    assert!(
        !journey.history.contains(COMPACTED),
        "opaque content never shows"
    );
}

/// The Luna A/B: later provider compactions in a long
/// turn kept retaining from one old cut while input climbed. Each of four
/// retains from past the prior one's cut, and the request after the last
/// carries its item, not the text or reads the compactions covered.
#[test]
fn successive_provider_compactions_each_retain_past_the_prior_cut() {
    // Each full request fits the model; the 30k threshold still compacts
    // every round, including its 60k assistant text and grouped reads.
    const WINDOW: u64 = 200_000;
    let mut scripts = vec![seed_read(), compaction_stream()];
    for calls in MORE_ROUNDS {
        scripts.push(Script::TextThenReads {
            text_bytes: ROUND_TEXT_BYTES,
            calls: calls.to_vec(),
        });
        scripts.push(compaction_stream());
    }
    scripts.push(Script::SuccessText("answered after four compactions"));
    let journey = journey_with_window(
        "sc-successive",
        serde_json::json!({"buffer": WINDOW - 16_384 - 30_000, "server_side": true}),
        scripts,
        Some(WINDOW),
    );
    assert_eq!(journey.bodies.len(), 9, "{}", journey.history);
    for compaction in [1, 3, 5, 7] {
        assert_eq!(
            input(&journey.bodies[compaction]).last(),
            Some(&serde_json::json!({"type":"compaction_trigger"})),
            "request {compaction} is a provider compaction: {}",
            journey.history
        );
    }
    let kinds: Vec<&str> = journey
        .compactions
        .iter()
        .map(|(kind, _)| kind.as_str())
        .collect();
    assert_eq!(kinds, ["providerBound"; 4], "{}", journey.history);
    assert!(
        journey.froms.windows(2).all(|pair| pair[1] > pair[0]),
        "a provider compaction did not retain past the prior cut: {:?}",
        journey.froms
    );
    let last = &journey.bodies[8];
    assert_eq!(
        last.matches(&"x".repeat(99)).count(),
        0,
        "the request after the last compaction replays reads it covered"
    );
    assert!(
        !last.contains("Observed work."),
        "the request after the last compaction replays text it covered"
    );
}

/// Any provider failure falls back to SE's own summary in the same run, and
/// the reason is recorded with the descriptor it led to.
#[test]
fn a_refused_or_itemless_provider_compaction_falls_back_with_its_reason() {
    for (label, compaction, reason) in [
        (
            "sc-refused",
            Script::Refused {
                status: 400,
                body: r#"{"error":{"message":"compaction unsupported","type":"invalid_request_error"}}"#,
            },
            "refused",
        ),
        (
            "sc-no-item",
            Script::SuccessText("no compaction item"),
            "no item",
        ),
        // The item arrives but the stream ends without completing.
        (
            "sc-interrupted",
            Script::Raw(format!(
                "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"compaction\",\"id\":\"cmp_sc\",\"encrypted_content\":\"{COMPACTED}\"}}}}\n\n"
            )),
            "interrupted",
        ),
        // Four valid items, each carrying a field the replay ignores: the
        // normalized items fit, the envelope persisted as received does not.
        ("sc-oversize-envelope", padded_compaction_stream(), "oversize"),
        // The provider answers the trigger with a tool call: nothing runs.
        (
            "sc-tool-call",
            Script::Raw(
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"item_sc_tool\",\"call_id\":\"call_sc_compaction_tool\",\"name\":\"read\"}}\n\n\
                 data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_sc_tool\",\"delta\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}\n\n\
                 data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_sc_tool\"}\n\n\
                 data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                    .to_owned(),
            ),
            "tool call",
        ),
    ] {
        let journey = journey(
            label,
            serde_json::json!({"buffer": BUFFER, "server_side": true}),
            vec![
                seed_read(),
                compaction,
                Script::SuccessText("## Objective\n- the seed was read"),
                Script::SuccessText("answered after compaction"),
            ],
        );
        assert!(journey.bodies[1].contains("compaction_trigger"), "{label}");
        assert!(
            journey.bodies[2].contains(SUMMARY_REQUEST),
            "{label}: SE summary followed"
        );
        assert_eq!(journey.compactions.len(), 1, "{label}");
        assert_eq!(journey.compactions[0].0, "clientReadable", "{label}");
        assert_eq!(
            journey.compactions[0].1,
            serde_json::json!({"outcome":"fallback","reason":reason}),
            "{label}"
        );
        assert_eq!(journey.bodies.len(), 4, "{label}: the turn was answered");
        assert!(
            journey.tool_events.is_empty(),
            "{label}: a tool call during compaction left a tool effect: {:?}",
            journey.tool_events
        );
    }
}

/// A provider compaction refused on the first call leaves its receipt on
/// disk uncommitted; SE's fallback summary commits. After a restart the next
/// prompt is answered with no further summary call, and history is
/// consistent.
#[test]
fn after_a_restart_an_uncommitted_provider_compaction_is_not_retried() {
    let _journey = process_journey_custody();
    let root = root("sc-restart");
    prepare(&root, 120_000);
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("config read failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("config decode failed: {error}"));
    config["compaction"] = serde_json::json!({"buffer": BUFFER, "server_side": true});
    fs::write(&path, serde_json::to_vec(&config).unwrap_or_default())
        .unwrap_or_else(|error| panic!("config write failed: {error}"));
    for (_, file) in READS {
        fs::write(root.join(file), text_lines(b'x', READ_BYTES))
            .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    }
    let provider = ProviderFixture::start_compacting(vec![
        seed_read(),
        Script::Refused {
            status: 400,
            body: r#"{"error":{"message":"compaction unsupported","type":"invalid_request_error"}}"#,
        },
        Script::SuccessText("## Objective\n- the seed was read"),
        Script::SuccessText("answered after compaction"),
        Script::SuccessText("answered after restart"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_sc_restart_one","text":"read the seed","delivery":"queue","resume":true}"#,
    );
    for (call, _) in READS {
        compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_sc_restart_two","text":"carry on","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )["data"]
        .to_string();
    child.stop_after_witness(&root);
    drop(child);
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("store open failed: {error}"));
    let kinds: Vec<String> = db
        .prepare("SELECT json_extract(data, '$.summary.kind') FROM event WHERE type = 'session.compaction.ended.2' ORDER BY seq")
        .and_then(|mut query| query.query_map([], |row| row.get(0)).and_then(|rows| rows.collect()))
        .unwrap_or_else(|error| panic!("descriptor read failed: {error}"));
    let calls: i64 = db
        .query_row(
            "SELECT count(*) FROM event WHERE type LIKE 'session.compaction.call.started%'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("call count failed: {error}"));
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);
    assert_eq!(
        bodies.len(),
        5,
        "reads, refused trigger, SE summary, answer, answer"
    );
    assert!(bodies[1].contains("compaction_trigger"));
    assert!(bodies[2].contains(SUMMARY_REQUEST));
    assert!(
        !bodies[4].contains("compaction_trigger") && !bodies[4].contains(SUMMARY_REQUEST),
        "the restarted Session made no further summary call"
    );
    assert_eq!(calls, 2, "a provider call and one fallback, never a third");
    assert_eq!(kinds, vec!["clientReadable".to_owned()]);
    for answer in ["answered after compaction", "answered after restart"] {
        assert!(history.contains(answer), "history lost {answer}: {history}");
    }
}

/// A refused provider compaction falls back to SE's one summary. An empty
/// answer stops without publishing a descriptor and is named truthfully.
#[test]
fn an_empty_se_summary_after_provider_refusal_is_named_truthfully() {
    let _journey = process_journey_custody();
    let root = root("sc-unusable-no-retry");
    prepare(&root, 120_000);
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("config read failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("config decode failed: {error}"));
    config["compaction"] = serde_json::json!({"buffer": BUFFER, "server_side": true});
    fs::write(&path, serde_json::to_vec(&config).unwrap_or_default())
        .unwrap_or_else(|error| panic!("config write failed: {error}"));
    for (_, file) in READS {
        fs::write(root.join(file), text_lines(b'x', READ_BYTES))
            .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    }
    let provider = ProviderFixture::start_compacting(vec![
        seed_read(),
        Script::Refused {
            status: 400,
            body: r#"{"error":{"message":"compaction unsupported","type":"invalid_request_error"}}"#,
        },
        Script::SuccessText(""),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_sc_unusable","text":"read the seed","delivery":"queue","resume":true}"#,
    );
    for (call, _) in READS {
        compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    child.stop_after_witness(&root);
    drop(child);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "SE's one summary ran, with no repair"
    );
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[2].contains(SUMMARY_REQUEST),
        "SE's own summary was not the fallback"
    );
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
    process_fixture::cleanup(&root);
}

/// The actual adapter and fallback record classify the same padded envelope
/// regardless of HTTP chunking; per-frame SSE capacity must not preempt it.
#[test]
fn sc_oversize_envelope_classification_survives_transport_chunking() {
    let Script::Raw(body) = padded_compaction_stream() else {
        panic!("padded compaction fixture must supply raw bytes");
    };
    for chunk_size in [body.len(), 64 * 1024, 100_000] {
        let journey = journey(
            &format!("sc-coalesced-{chunk_size}"),
            serde_json::json!({"buffer": BUFFER, "server_side": true}),
            vec![
                seed_read(),
                Script::RawChunks(
                    body.as_bytes()
                        .chunks(chunk_size)
                        .map(<[u8]>::to_vec)
                        .collect(),
                ),
                Script::SuccessText("## Objective\n- the seed was read"),
                Script::SuccessText("answered after compaction"),
            ],
        );
        assert_eq!(
            journey.compactions,
            vec![(
                "clientReadable".to_owned(),
                serde_json::json!({"outcome":"fallback","reason":"oversize"})
            )],
            "chunk_size={chunk_size}: {}",
            journey.history
        );
        assert_eq!(journey.bodies.len(), 4, "chunk_size={chunk_size}");
        assert!(journey.tool_events.is_empty(), "chunk_size={chunk_size}");
    }
}

/// Without search on offer, text, reasoning items and an
/// empty message beside a provider's item are still ignored and the item commits, but the
/// descriptor counts the ignored text so row A can show how often it happens.
#[test]
fn ignored_text_beside_a_provider_compaction_is_counted() {
    let journey = journey(
        "sc-ignored-text",
        serde_json::json!({"buffer": BUFFER, "server_side": true}),
        vec![
            seed_read(),
            Script::Raw(format!(
                "data: {{\"type\":\"response.output_text.delta\",\"item_id\":\"msg_sc_text\",\"delta\":\"extra\"}}\n\n\
                 data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"reasoning\",\"id\":\"rs_sc_extra\",\"summary\":[],\"encrypted_content\":\"SC_REASONING\"}}}}\n\n\
                 data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"reasoning\",\"id\":\"rs_sc_bare\",\"summary\":[]}}}}\n\n\
                 data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"message\",\"id\":\"msg_sc_empty\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[]}}}}\n\n\
                 data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"compaction\",\"id\":\"cmp_sc\",\"encrypted_content\":\"{COMPACTED}\"}}}}\n\n\
                 data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":19000,\"output_tokens\":300,\"total_tokens\":19300}}}}}}\n\n"
            )),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert_eq!(journey.compactions.len(), 1, "{}", journey.history);
    assert_eq!(
        journey.compactions[0].0, "providerBound",
        "{}",
        journey.history
    );
    let server = &journey.compactions[0].1;
    assert_eq!(
        (
            &server["outcome"],
            &server["items"],
            &server["ignoredTextBytes"]
        ),
        (
            &serde_json::json!("committed"),
            &serde_json::json!(1),
            &serde_json::json!(5)
        ),
        "{server}"
    );
    assert_eq!(journey.bodies.len(), 3, "the turn was answered");
}

//! SC-CACHE-07: a coding request that offers hosted web search compacts.
use super::*;

/// Puts the next journey on `family` with hosted web search on.
fn hosted_search_platform(family: &'static str) {
    if family == "openai-responses" {
        THREAD_HEADERS.with(|threads| threads.set(false));
    }
    PLATFORM.with(|platform| {
        *platform.borrow_mut() = Some(Box::new(move |root: &Path| {
            let path = root.join("grapt.jsonc");
            let mut config: serde_json::Value = serde_json::from_slice(
                &fs::read(&path).unwrap_or_else(|error| panic!("config read failed: {error}")),
            )
            .unwrap_or_else(|error| panic!("config decode failed: {error}"));
            config["provider"]["protocol_family"] = serde_json::json!(family);
            config["web_search"] = serde_json::json!("cached");
            // The fixture composes the ChatGPT catalog; a Responses route
            // composes the OpenAI one, names its account's models, and sends
            // no thread headers.
            if family == "openai-responses" {
                config["catalog"]["source"] = serde_json::json!("models.dev-openai");
                let inventory = root.join("model-catalog.json");
                let mut models: serde_json::Value = serde_json::from_slice(
                    &fs::read(&inventory)
                        .unwrap_or_else(|error| panic!("inventory read failed: {error}")),
                )
                .unwrap_or_else(|error| panic!("inventory decode failed: {error}"));
                models["openai"]["models"]["gpt-live"]["limit"]["context"] =
                    serde_json::json!(128_000);
                fs::write(&inventory, serde_json::to_vec(&models).unwrap_or_default())
                    .unwrap_or_else(|error| panic!("inventory write failed: {error}"));
                fs::write(
                    root.join("account-models.json"),
                    br#"{"data":[{"id":"gpt-live"}]}"#,
                )
                .unwrap_or_else(|error| panic!("account models write failed: {error}"));
            }
            fs::write(&path, serde_json::to_vec(&config).unwrap_or_default())
                .unwrap_or_else(|error| panic!("config write failed: {error}"));
        }));
    });
}

/// Whether a request body offers hosted web search.
fn offers_search(body: &serde_json::Value) -> bool {
    body["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["type"] == "web_search"))
}

/// A hosted-search journey: the seed read, then `scripts`. Returns the
/// bodies as JSON, the provider and summary call counts, and stderr.
fn hosted_search_journey(
    family: &'static str,
    server_side: bool,
    label: &str,
    scripts: Vec<Script>,
) -> (Vec<serde_json::Value>, usize, usize, String) {
    hosted_search_platform(family);
    let mut all = vec![seed_read()];
    all.extend(scripts);
    let (bodies, triggers, calls, _, summary_calls, stderr) = journey_sized(
        &format!("config-hosted-{label}"),
        Some(serde_json::json!({"buffer": BUFFER, "server_side": server_side})),
        all,
        false,
        READ_BYTES,
        None,
        None,
    );
    // One compaction, and the turn went on: no HiddenTool refusal and no
    // CompactionFailed.
    assert_eq!(
        triggers,
        vec!["policyThreshold".to_owned()],
        "product stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("HiddenTool") && !stderr.contains("CompactionFailed"),
        "product stderr:\n{stderr}"
    );
    let bodies: Vec<serde_json::Value> = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| {
            serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("request {index} is not JSON: {error}"))
        })
        .collect();
    assert!(
        offers_search(&bodies[0]),
        "the coding request did not offer hosted search: {}",
        bodies[0]
    );
    (bodies, calls, summary_calls, stderr)
}

/// SC-CACHE-07: a Session with web search on
/// compacts warm. The summary extends the coding request with its tools,
/// hosted search included and granted, byte for byte, and `tool_choice:
/// none`; the descriptor counts it warm.
fn hosted_search_summary_is_warm(family: &'static str) {
    let (bodies, calls, summary_calls, stderr) = hosted_search_journey(
        family,
        false,
        &format!("warm-{family}"),
        vec![
            Script::SuccessText("## Objective\n- read the seed files"),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert_eq!((calls, summary_calls), (3, 1), "product stderr:\n{stderr}");
    let summary = &bodies[1];
    assert!(
        summary.to_string().contains(SUMMARY_REQUEST)
            && !summary.to_string().contains(COLD_SUMMARY),
        "the summary is not the warm request: {summary}"
    );
    assert_eq!(
        summary["tools"], bodies[0]["tools"],
        "the warm summary changed the offered tools"
    );
    assert!(offers_search(summary), "{summary}");
    assert_eq!(summary["tool_choice"], "none", "{summary}");
    assert_eq!(
        PLANS.with(|plans| plans.borrow().clone()),
        vec![serde_json::json!("warm")]
    );
    assert_eq!(
        SERVER.with(|server| server.borrow().clone()),
        vec![serde_json::json!({"outcome":"fallback","reason":"refused"})]
    );
}

#[test]
fn hosted_search_on_openai_responses_compacts_warm() {
    hosted_search_summary_is_warm("openai-responses");
}

#[test]
fn hosted_search_on_chatgpt_codex_compacts_warm() {
    hosted_search_summary_is_warm("chatgpt-codex");
}

/// `prefix`, then `items` provider compaction items, then the completion.
fn compaction_items(prefix: &str, items: usize) -> Script {
    let mut stream = prefix.to_owned();
    for index in 0..items {
        stream.push_str(&format!(
            "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"compaction\",\"id\":\"cmp_hosted_{index}\",\"encrypted_content\":\"HOSTED_COMPACTED\"}}}}\n\n"
        ));
    }
    stream.push_str("data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":19000,\"output_tokens\":300,\"total_tokens\":19300}}}\n\n");
    Script::Raw(stream)
}

/// SC-CACHE-07: with server_side on, hosted search
/// rides the provider compaction: the
/// coding request's tools unchanged, `tool_choice: auto`, and one
/// compaction_trigger. One compaction item commits.
#[test]
fn hosted_search_rides_a_provider_compaction() {
    let (bodies, calls, summary_calls, stderr) = hosted_search_journey(
        "chatgpt-codex",
        true,
        "provider",
        vec![
            compaction_items("", 1),
            Script::SuccessText("answered after compaction"),
        ],
    );
    assert_eq!((calls, summary_calls), (3, 1), "product stderr:\n{stderr}");
    let compaction = &bodies[1];
    assert!(
        compaction.to_string().contains("compaction_trigger"),
        "{compaction}"
    );
    assert_eq!(compaction["tools"], bodies[0]["tools"]);
    assert!(offers_search(compaction), "{compaction}");
    assert_eq!(compaction["tool_choice"], "auto", "{compaction}");
    let server = SERVER.with(|server| server.borrow().clone());
    assert_eq!(server.len(), 1);
    assert_eq!(
        (&server[0]["outcome"], &server[0]["items"]),
        (&serde_json::json!("committed"), &serde_json::json!(1)),
        "{server:?}"
    );
}

/// SC-CACHE-07: with hosted search on offer, a
/// provider compaction answered with anything but exactly one compaction item
/// (a search call, text, text past the summary bound, a reasoning item with
/// or without replay state, an empty message, or a second item) is
/// discarded. The text fallback retains the cached coding prefix, and both
/// the rejected call's terminal usage and the warm fallback are recorded.
#[test]
fn a_rejected_hosted_compaction_keeps_usage_and_the_cached_summary_prefix() {
    let search = "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_hosted\",\"status\":\"completed\",\"action\":{\"type\":\"search\",\"queries\":[\"fixture question\"]}}}\n\n";
    let text = "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_hosted\",\"delta\":\"extra\"}\n\n";
    // Seventeen 4096-byte deltas pass the summary owner's 64 KiB bound.
    let oversized = format!(
        "data: {{\"type\":\"response.output_text.delta\",\"item_id\":\"msg_hosted\",\"delta\":\"{}\"}}\n\n",
        "x".repeat(4096)
    )
    .repeat(17);
    let reasoning = "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_extra\",\"summary\":[],\"encrypted_content\":\"HOSTED_REASONING\"}}\n\n";
    // Items the decoder would drop: reasoning with nothing to replay, and an
    // empty assistant message.
    let bare = "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_extra\",\"summary\":[]}}\n\n";
    let empty = "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"id\":\"msg_extra\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[]}}\n\n";
    for (label, compaction) in [
        ("search", compaction_items(search, 1)),
        ("text", compaction_items(text, 1)),
        ("oversized", compaction_items(&oversized, 1)),
        ("reasoning", compaction_items(reasoning, 1)),
        ("bare-reasoning", compaction_items(bare, 1)),
        ("empty-message", compaction_items(empty, 1)),
        ("two", compaction_items("", 2)),
    ] {
        let (bodies, calls, summary_calls, stderr) = hosted_search_journey(
            "chatgpt-codex",
            true,
            &format!("not-single-{label}"),
            vec![
                compaction,
                Script::SuccessText("## Objective\n- read the seed files"),
                Script::SuccessText("answered after compaction"),
            ],
        );
        assert_eq!(
            (calls, summary_calls),
            (4, 2),
            "{label}: product stderr:\n{stderr}"
        );
        assert_eq!(
            SUMMARY_USAGE_CALLS.with(std::cell::Cell::get),
            2,
            "{label}: rejected provider output lost reported terminal usage"
        );
        let summary = bodies[2].to_string();
        assert_eq!(bodies[2]["tools"], bodies[0]["tools"]);
        assert_eq!(bodies[2]["prompt_cache_key"], bodies[0]["prompt_cache_key"]);
        let prefix = bodies[0]["input"]
            .as_array()
            .unwrap_or_else(|| panic!("coding input"));
        let extended = bodies[2]["input"]
            .as_array()
            .unwrap_or_else(|| panic!("summary input"));
        assert!(
            extended.starts_with(prefix),
            "{label}: summary changed conversation prefix"
        );
        assert!(
            !summary.contains(COLD_SUMMARY) && warm(&summary),
            "{label}: the fallback summary discarded its cached prefix: {summary}"
        );
        assert_eq!(
            SERVER.with(|server| server.borrow().clone()),
            vec![
                serde_json::json!({"outcome":"fallback","reason":"compaction-response-not-single-item"})
            ],
            "{label}"
        );
        assert_eq!(
            PLANS.with(|plans| plans.borrow().clone()),
            vec![serde_json::json!("warm")],
            "{label}"
        );
    }
}

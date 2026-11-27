//! CODING-QUALITY-01 prompt caching: every request of one Session must extend
//! the one before it byte-for-byte, or a provider prefix cache cannot hit.
//! A scripted provider proves the bytes, never a real cache hit.
use super::*;

/// Set to a directory to keep the captured bodies for inspection.
const CAPTURE: &str = "GRAPT_PROMPT_CACHE_CAPTURE";

/// `later` repeats `earlier`: every field but `input` is equal, and the
/// earlier body's bytes up to the end of its last input item are a byte
/// prefix of the later body.
fn assert_extends(earlier: &str, later: &str, label: &str) {
    let parse = |body: &str| -> serde_json::Value {
        serde_json::from_str(body)
            .unwrap_or_else(|error| panic!("{label}: body is not JSON: {error}"))
    };
    let (mut first, mut second) = (parse(earlier), parse(later));
    let items = |body: &mut serde_json::Value| -> Vec<serde_json::Value> {
        body.as_object_mut()
            .and_then(|object| object.remove("input"))
            .and_then(|input| input.as_array().cloned())
            .unwrap_or_else(|| panic!("{label}: body has no input array"))
    };
    let (before, after) = (items(&mut first), items(&mut second));
    assert_eq!(first, second, "{label}: a field outside input changed");
    for (index, item) in before.iter().enumerate() {
        assert_eq!(
            Some(item),
            after.get(index),
            "{label}: input item {index} changed between consecutive requests"
        );
    }
    let open = earlier
        .find("\"input\":[")
        .unwrap_or_else(|| panic!("{label}: no input array in the bytes"));
    let close = earlier[open..]
        .find("],")
        .or_else(|| earlier[open..].rfind(']'))
        .map(|end| open + end)
        .unwrap_or_else(|| panic!("{label}: input array is not closed"));
    assert!(
        later.as_bytes().starts_with(&earlier.as_bytes()[..close]),
        "{label}: the later body does not start with the earlier body's bytes"
    );
}

/// Three user turns, the first two each reading a file through a tool.
#[test]
fn consecutive_requests_extend_the_previous_request_byte_for_byte() {
    let _journey = process_journey_custody();
    let root = root("prompt-cache-prefix");
    prepare(&root, 120_000);
    for name in ["a.txt", "b.txt"] {
        fs::write(root.join(name), format!("contents of {name}\n"))
            .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_cache_a",
            path: "a.txt",
        },
        Script::SuccessText("read a"),
        Script::ToolRead {
            call: "call_cache_b",
            path: "b.txt",
        },
        Script::SuccessText("read b"),
        Script::SuccessText("answered"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let turns: [(&str, &str, Option<&str>); 3] = [
        ("msg_cache_one", "read a.txt", Some("call_cache_a")),
        ("msg_cache_two", "read b.txt", Some("call_cache_b")),
        ("msg_cache_three", "now answer", None),
    ];
    for (id, text, call) in turns {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id": id, "text": text, "delivery": "queue", "resume": true})
                .to_string(),
        );
        if let Some(call) = call {
            compaction_continuity::approve(address, &session, call);
        }
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);
    if let Some(directory) = std::env::var_os(CAPTURE) {
        let directory = Path::new(&directory);
        for (index, body) in bodies.iter().enumerate() {
            fs::write(directory.join(format!("request-{index}.json")), body)
                .unwrap_or_else(|error| panic!("capture write failed: {error}"));
        }
    }
    assert_eq!(bodies.len(), 5, "three turns make five provider requests");
    for index in 1..bodies.len() {
        assert_extends(
            &bodies[index - 1],
            &bodies[index],
            &format!("request {index} after request {}", index - 1),
        );
    }
}

/// A reasoning item, then a `read` call, as one Codex stream.
fn reasoning_then_read(call: &str, path: &str) -> Script {
    let arguments = serde_json::json!({ "path": path }).to_string();
    Script::Raw(format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"reasoning\",\"id\":\"rs_cache\",\"summary\":[]}}}}\n\n\
         data: {{\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_cache\",\"delta\":\"plan\"}}\n\n\
         data: {{\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_cache\"}}\n\n\
         data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"reasoning\",\"id\":\"rs_cache\",\"status\":\"completed\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"plan\"}}],\"encrypted_content\":\"CACHE_ENCRYPTED\"}}}}\n\n\
         data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"read\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"))
    ))
}

/// The continuation after a tool call carries the model's reasoning item back
/// verbatim, in place before its call, so the request still extends the one
/// before it byte-for-byte.
#[test]
fn a_reasoning_item_is_carried_back_before_its_call_and_the_prefix_holds() {
    let _journey = process_journey_custody();
    let root = root("prompt-cache-reasoning");
    prepare(&root, 120_000);
    fs::write(root.join("a.txt"), "contents of a.txt\n")
        .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        reasoning_then_read("call_cache_reasoning", "a.txt"),
        Script::SuccessText("read a"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id": "msg_cache_reasoning", "text": "read a.txt", "delivery": "queue", "resume": true})
            .to_string(),
    );
    compaction_continuity::approve(address, &session, "call_cache_reasoning");
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);
    assert_eq!(bodies.len(), 2, "a tool call and its continuation");
    let first: serde_json::Value = serde_json::from_str(&bodies[0])
        .unwrap_or_else(|error| panic!("first body is not JSON: {error}"));
    assert_eq!(
        first["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
    let second: serde_json::Value = serde_json::from_str(&bodies[1])
        .unwrap_or_else(|error| panic!("second body is not JSON: {error}"));
    let input = second["input"]
        .as_array()
        .unwrap_or_else(|| panic!("continuation has no input: {second}"));
    let at = input
        .iter()
        .position(|item| item["type"] == "reasoning")
        .unwrap_or_else(|| panic!("the reasoning item was not carried back: {second}"));
    assert_eq!(
        input[at],
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_cache",
            "summary": [{"type": "summary_text", "text": "plan"}],
            "encrypted_content": "CACHE_ENCRYPTED",
        })
    );
    assert_eq!(input[at + 1]["type"], "function_call", "{second}");
    assert_eq!(input[at + 1]["call_id"], "call_cache_reasoning");
    assert_eq!(input[at + 2]["type"], "function_call_output");
    assert_extends(
        &bodies[0],
        &bodies[1],
        "continuation after a reasoning tool call",
    );
}

/// A model its account catalog marks `use_responses_lite` keeps every turn's
/// reasoning in context (`reasoning.context: "all_turns"`); the same model
/// without the mark sends no reasoning context.
#[test]
fn a_responses_lite_model_asks_to_keep_reasoning_from_all_turns() {
    for lite in [true, false] {
        let _journey = process_journey_custody();
        let root = root(if lite {
            "prompt-cache-lite"
        } else {
            "prompt-cache-not-lite"
        });
        prepare(&root, 120_000);
        let mut row = serde_json::json!({"slug":"gpt-live", "visibility":"list",
            "context_window":128_000, "supported_reasoning_levels":[]});
        if lite {
            row["use_responses_lite"] = serde_json::json!(true);
            // Supporting summaries is not accepting the parameter: no summary.
            row["supports_reasoning_summaries"] = serde_json::json!(true);
            row["supports_reasoning_summary_parameter"] = serde_json::json!(false);
        }
        fs::write(
            root.join("account-models.json"),
            serde_json::json!({"models":[row]}).to_string(),
        )
        .unwrap_or_else(|error| panic!("account catalog write failed: {error}"));
        let provider = ProviderFixture::start(vec![Script::SuccessText("answered")]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id": "msg_lite", "text": "hello", "delivery": "queue", "resume": true})
                .to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
        child.stop_after_witness(&root);
        drop(child);
        let bodies = locked(&provider.bodies).clone();
        process_fixture::cleanup(&root);
        assert_eq!(bodies.len(), 1, "one prompt, one request");
        let body: serde_json::Value = serde_json::from_str(&bodies[0])
            .unwrap_or_else(|error| panic!("body is not JSON: {error}"));
        if lite {
            assert_eq!(
                body["reasoning"],
                serde_json::json!({"context":"all_turns"}),
                "a Responses Lite model keeps all turns' reasoning, and no summary \
                 where the route does not accept the parameter: {body}"
            );
        } else {
            assert!(body.get("reasoning").is_none(), "{body}");
        }
        assert!(body.get("parallel_tool_calls").is_none(), "{body}");
    }
}

/// Luna's shape on the Codex route (a reasoning-context capture): a
/// model whose catalog supports reasoning summaries is asked for one, and only
/// then answers a tool continuation with an encrypted reasoning item. That
/// item is carried back verbatim, before the answer it preceded, in the next
/// prompt's request.
#[test]
fn a_summary_model_carries_its_reasoning_into_the_next_request() {
    let _journey = process_journey_custody();
    let root = root("prompt-cache-summary");
    prepare(&root, 120_000);
    fs::write(
        root.join("account-models.json"),
        serde_json::json!({"models":[{"slug":"gpt-live", "visibility":"list",
            "context_window":128_000, "supported_reasoning_levels":[],
            "use_responses_lite": true, "supports_reasoning_summaries": true,
            "supports_reasoning_summary_parameter": true}]})
        .to_string(),
    )
    .unwrap_or_else(|error| panic!("account catalog write failed: {error}"));
    fs::write(root.join("a.txt"), "contents of a.txt\n")
        .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    let answer = Script::Raw(
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_luna\",\"summary\":[],\"encrypted_content\":\"LUNA_ENCRYPTED\"}}\n\n\
         data: {\"type\":\"response.output_text.delta\",\"delta\":\"read a\"}\n\n\
         data: {\"type\":\"response.output_text.done\"}\n\n\
         data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
            .to_owned(),
    );
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_summary_a",
            path: "a.txt",
        },
        answer,
        Script::SuccessText("answered"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    for (id, text, call) in [
        ("msg_summary_one", "read a.txt", Some("call_summary_a")),
        ("msg_summary_two", "now answer", None),
    ] {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id": id, "text": text, "delivery": "queue", "resume": true})
                .to_string(),
        );
        if let Some(call) = call {
            compaction_continuity::approve(address, &session, call);
        }
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);
    assert_eq!(
        bodies.len(),
        3,
        "a tool call, its continuation, the next prompt"
    );
    for body in &bodies {
        let body: serde_json::Value =
            serde_json::from_str(body).unwrap_or_else(|error| panic!("body is not JSON: {error}"));
        assert_eq!(
            body["reasoning"],
            serde_json::json!({"context":"all_turns","summary":"auto"}),
            "constant within the Session: {body}"
        );
    }
    let next: serde_json::Value = serde_json::from_str(&bodies[2])
        .unwrap_or_else(|error| panic!("body is not JSON: {error}"));
    let input = next["input"]
        .as_array()
        .unwrap_or_else(|| panic!("next request has no input: {next}"));
    let at = input
        .iter()
        .position(|item| item["type"] == "reasoning")
        .unwrap_or_else(|| panic!("the reasoning item was not carried back: {next}"));
    assert_eq!(
        input[at],
        serde_json::json!({"type":"reasoning","id":"rs_luna","summary":[],"encrypted_content":"LUNA_ENCRYPTED"})
    );
    assert_eq!(input[at + 1]["role"], "assistant", "{next}");
    assert_extends(
        &bodies[1],
        &bodies[2],
        "next prompt after a summarized answer",
    );
}

/// Codex's sticky routing (x-codex-turn-state) through the product: the
/// first response of an execution carries the token, the same execution's
/// continuation sends it back, and the next prompt's execution starts
/// without it.
#[test]
fn an_execution_sends_back_its_turn_state_and_the_next_does_not() {
    let _journey = process_journey_custody();
    let root = root("prompt-cache-turn-state");
    prepare(&root, 120_000);
    fs::write(root.join("a.txt"), "contents of a.txt\n")
        .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    let arguments = serde_json::json!({"path": "a.txt"}).to_string();
    let read = format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_ts\",\"call_id\":\"call_turn_state\",\"name\":\"read\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_ts\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_ts\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"))
    );
    let provider = ProviderFixture::start(vec![
        Script::RawWithHeaders {
            headers: vec![("x-codex-turn-state", "TS_PRODUCT_TOKEN".to_owned())],
            body: read,
        },
        Script::SuccessText("read a"),
        Script::SuccessText("answered"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    for (id, text, call) in [
        ("msg_ts_one", "read a.txt", Some("call_turn_state")),
        ("msg_ts_two", "now answer", None),
    ] {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id": id, "text": text, "delivery": "queue", "resume": true})
                .to_string(),
        );
        if let Some(call) = call {
            compaction_continuity::approve(address, &session, call);
        }
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    let heads = locked(&provider.heads).clone();
    process_fixture::cleanup(&root);
    assert_eq!(
        heads.len(),
        3,
        "a tool call, its continuation, the next prompt"
    );
    let sends = |head: &String| {
        head.to_ascii_lowercase()
            .contains("x-codex-turn-state: ts_product_token")
    };
    assert!(
        !sends(&heads[0]),
        "the first request of an execution has no token"
    );
    assert!(
        sends(&heads[1]),
        "the continuation sends the execution's token back"
    );
    assert!(
        !sends(&heads[2]),
        "the next prompt's execution starts without it"
    );
}

/// Two fresh Sessions of one project send byte-identical tools and system
/// text up to the one line that names the Session, so a provider can reuse
/// that prefix across Sessions (an offline capture
/// found the Session ID line inside the environment block as the only
/// difference).
#[test]
fn two_fresh_sessions_share_everything_before_the_session_line() {
    let _journey = process_journey_custody();
    let root = root("prompt-cache-cross-session");
    prepare(&root, 120_000);
    // Shared project text the prefix must cover: instructions and skills.
    fs::write(
        root.join("AGENTS.md"),
        "XSESSION-PROJECT-RULES: keep tests green.\n",
    )
    .unwrap_or_else(|error| panic!("project instructions write failed: {error}"));
    let home = skill_activation::install_skills(&root);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first"),
        Script::SuccessText("second"),
    ]);
    let mut child = spawn_with(&root, provider.address, &[("GRAPT_TEST_HOME", &home)]);
    let address = await_ready(&root, &mut child);
    for id in ["msg_cross_one", "msg_cross_two"] {
        let session = create(address);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id": id, "text": "hello", "delivery": "queue", "resume": true})
                .to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);
    assert_eq!(bodies.len(), 2, "one request per Session");
    let parse = |body: &str| -> serde_json::Value {
        serde_json::from_str(body).unwrap_or_else(|error| panic!("body is not JSON: {error}"))
    };
    let (first, second) = (parse(&bodies[0]), parse(&bodies[1]));
    assert_eq!(
        first["tools"], second["tools"],
        "tools differ between Sessions"
    );
    let input = |body: &serde_json::Value| body["input"].as_array().cloned().unwrap_or_default();
    let (one, two) = (input(&first), input(&second));
    let line = |items: &[serde_json::Value]| {
        items
            .iter()
            .position(|item| {
                item["role"] == "system"
                    && item["content"]
                        .as_str()
                        .is_some_and(|text| text.starts_with("Session ID: "))
            })
            .unwrap_or_else(|| panic!("no Session line: {items:?}"))
    };
    let at = line(&one);
    assert_eq!(at, line(&two));
    assert_eq!(
        one[..at],
        two[..at],
        "the system text before the Session line differs between Sessions"
    );
    assert_ne!(one[at], two[at], "the Session line names each Session");
    // The Session line stands alone, after the environment it used to open.
    assert!(
        one[at]["content"]
            .as_str()
            .is_some_and(|text| !text.contains('\n')),
        "the Session line carries nothing else: {}",
        one[at]
    );
    for (shared, what) in [
        ("Working directory: ", "the environment"),
        ("XSESSION-PROJECT-RULES", "the project's instructions"),
        (
            "use the skill tool to discover it",
            "skill discovery guidance",
        ),
    ] {
        assert!(
            one[..at].iter().any(|item| item["content"]
                .as_str()
                .is_some_and(|text| text.contains(shared))),
            "{what} must be shared, before the Session line: {one:?}"
        );
    }
    let shared = serde_json::to_string(&first["tools"]).map_or(0, |tools| tools.len())
        + one[..at]
            .iter()
            .map(|item| item["content"].as_str().map_or(0, str::len))
            .sum::<usize>();
    eprintln!("cross-session identical prefix: {shared} bytes before the Session line");
}

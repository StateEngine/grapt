//! Actual loopback wire assertions, never a provider cache-hit measurement.
//! Set GRAPT_CACHE_HISTORY_CAPTURE to export each request for offline budgeting.
#![cfg(unix)]
use super::*;
use serde_json::{json as value, Value};

const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";
const SUMMARY: &str = "## Objective\n- Preserve SC_CACHE05_STEER and SC_CACHE05_ATTACHMENT. SC_CACHE05_SUMMARY.\n## Next Steps\n- Continue the requested checks.";

#[derive(Clone, Copy)]
enum Family {
    ChatGpt,
    Anthropic,
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Self::ChatGpt => "chatgpt",
            Self::Anthropic => "anthropic",
        }
    }

    fn tool(self, call: &str, code: &str) -> Script {
        let args = value!({"code":code});
        let raw = match self {
            Self::ChatGpt => skill_activation::tool_call(call, "execute", args),
            Self::Anthropic => context_quality::anthropic_stream(
                &[value!({"type":"tool_use", "id":call, "name":"execute", "input":args})],
                "tool_use",
                Value::Null,
            ),
        };
        without_usage(raw)
    }

    fn text(self, text: &str) -> Script {
        let raw = match self {
            Self::ChatGpt => format!(
                "data: {}\n\ndata: {}\n\ndata: {}\n\n",
                value!({"type":"response.output_text.delta", "delta":text}),
                value!({"type":"response.output_text.done"}),
                value!({"type":"response.completed", "response":{}}),
            ),
            Self::Anthropic => context_quality::anthropic_stream(
                &[value!({"type":"text", "text":text})],
                "end_turn",
                Value::Null,
            ),
        };
        without_usage(raw)
    }
}

/// Reuse the existing protocol stubs, deliberately omitting every usage field.
fn without_usage(raw: String) -> Script {
    let mut frames = String::new();
    for frame in raw.split("\n\n").filter(|frame| !frame.is_empty()) {
        let payload = frame
            .strip_prefix("data: ")
            .unwrap_or_else(|| panic!("unexpected SSE frame"));
        let mut frame: Value =
            serde_json::from_str(payload).unwrap_or_else(|error| panic!("fixture SSE: {error}"));
        let object = frame
            .as_object_mut()
            .unwrap_or_else(|| panic!("fixture SSE is not an object"));
        object.remove("usage");
        for key in ["message", "response"] {
            if let Some(nested) = object.get_mut(key).and_then(Value::as_object_mut) {
                nested.remove("usage");
            }
        }
        frames.push_str(&format!("data: {frame}\n\n"));
    }
    Script::Raw(frames)
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str, delivery: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &value!({"id":id,"text":text,"delivery":delivery,"resume":true}).to_string(),
    );
}

fn wait_for_tool_approval(address: SocketAddr, session: &str) -> Value {
    let wait = PollDeadline::new();
    while wait.pending() {
        let page = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        if let Some(row) = page["data"].as_array().and_then(|rows| rows.first()) {
            assert_eq!(row["action"], "auxiliary_tool_119");
            return row.clone();
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("the final tool from the complete catalog never reached approval");
}

fn conversation(family: Family, request: &Value) -> Vec<Value> {
    match family {
        Family::ChatGpt => request["input"].as_array(),
        Family::Anthropic => request["messages"].as_array(),
    }
    .unwrap_or_else(|| panic!("conversation missing"))
    .iter()
    .cloned()
    .map(context_quality::unmarked)
    .collect()
}

fn assert_extends(family: Family, before: &Value, after: &Value) {
    let (before, after) = (conversation(family, before), conversation(family, after));
    assert!(
        after.len() > before.len(),
        "continuation added no conversation items"
    );
    assert_eq!(
        before.as_slice(),
        &after[..before.len()],
        "cached conversation content changed"
    );
}

fn assert_fixed_prefix(family: Family, initial: &Value, later: &Value) {
    let (mut before, mut after) = (initial.clone(), later.clone());
    let field = match family {
        Family::ChatGpt => "input",
        Family::Anthropic => "messages",
    };
    before
        .as_object_mut()
        .unwrap_or_else(|| panic!("request object missing"))
        .remove(field);
    after
        .as_object_mut()
        .unwrap_or_else(|| panic!("request object missing"))
        .remove(field);
    assert_eq!(before, after, "fixed tool/System request prefix changed");
    if let Family::ChatGpt = family {
        let prefix: Vec<_> = initial["input"]
            .as_array()
            .unwrap_or_else(|| panic!("input missing"))
            .iter()
            .take_while(|item| item["role"] == "system")
            .cloned()
            .collect();
        assert!(!prefix.is_empty(), "no canonical System prefix");
        let next = later["input"]
            .as_array()
            .unwrap_or_else(|| panic!("input missing"));
        assert_eq!(
            prefix.as_slice(),
            &next[..prefix.len()],
            "compaction rewrote initial System items"
        );
    }
}

fn capture(family: Family, bodies: &[String]) {
    if let Some(directory) = std::env::var_os("GRAPT_CACHE_HISTORY_CAPTURE") {
        let directory = PathBuf::from(directory).join(family.name());
        fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("wire capture directory: {error}"));
        for (index, body) in bodies.iter().enumerate() {
            fs::write(directory.join(format!("request-{index}.json")), body)
                .unwrap_or_else(|error| panic!("wire capture: {error}"));
        }
    }
}

#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn journey(family: Family) {
    let root = root(&format!("cache05-history-{}", family.name()));
    prepare(&root, 60_000);
    unattended(&root);
    first_request_budget::install_large_catalog(&root);
    fs::write(
        root.join("AGENTS.md"),
        "SC_CACHE05_REQUIRED_PROJECT_CONTEXT\n".repeat(400),
    )
    .unwrap_or_else(|error| panic!("project context: {error}"));
    for index in 0..20 {
        let directory = root.join(format!(".agents/skills/fixture-{index}"));
        fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("skill directory: {error}"));
        fs::write(directory.join("SKILL.md"), format!(
            "---\nname: Fixture {index}\ndescription: SC_CACHE05_OPTIONAL_METADATA {}\n---\nSC_CACHE05_SKILL_BODY", "metadata ".repeat(100)))
            .unwrap_or_else(|error| panic!("skill: {error}"));
    }
    if let Family::Anthropic = family {
        context_quality::configure_anthropic(&root, 200_000);
    } else {
        let path = root.join("grapt.jsonc");
        let mut config: Value = serde_json::from_slice(
            &fs::read(&path).unwrap_or_else(|error| panic!("fixture config: {error}")),
        )
        .unwrap_or_else(|error| panic!("fixture config: {error}"));
        config["provider"]["protocol_family"] = value!("chatgpt-codex");
        fs::write(path, config.to_string())
            .unwrap_or_else(|error| panic!("fixture config: {error}"));
    }
    let provider = ProviderFixture::start(vec![
        family.tool(
            "call_history_discover",
            "return catalog.describe('auxiliary_tool_119');",
        ),
        family.tool(
            "call_history_invoke",
            "return await tools.auxiliary_tool_119({value:'boundary-ok'});",
        ),
        family.text("SC_CACHE05_STEER handled"),
        family.text(SUMMARY),
        family.tool(
            "call_history_rediscover",
            "return catalog.describe('auxiliary_tool_119');",
        ),
        family.tool(
            "call_history_reinvoke",
            "return await tools.auxiliary_tool_119({value:'boundary-ok'});",
        ),
        family.text("SC_CACHE05_POST_COMPACT handled"),
        family.text("SC_CACHE05_FOLLOWUP handled"),
    ]);
    let mut service = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    if let Family::Anthropic = family {
        context_quality::select_anthropic(address, &session);
    }
    json(address, "POST", &format!("/api/session/{session}/prompt"), &value!({
        "id":"msg_history_initial", "text":"Inspect the complete catalog.", "delivery":"queue", "resume":true,
        "files":[{"name":"fixture.txt","uri":format!("data:text/plain,{}", "SC_CACHE05_ATTACHMENT_".repeat(100))}]
    }).to_string());
    let permission = wait_for_tool_approval(address, &session);
    prompt(
        address,
        &session,
        "msg_history_steer",
        "SC_CACHE05_STEER: preserve the newer instruction.",
        "steer",
    );
    permission_boundaries::reply(address, &session, &permission, "once");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "steer did not join the continuation"
    );
    let admitted = json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    assert_eq!(admitted["data"]["type"], "compaction");
    prompt(
        address,
        &session,
        "msg_history_compact",
        "SC_CACHE05_POST_COMPACT: continue the catalog check.",
        "queue",
    );
    let permission = wait_for_tool_approval(address, &session);
    permission_boundaries::reply(address, &session, &permission, "once");
    permission_residuals::wait_session_idle(address, &session);
    prompt(
        address,
        &session,
        "msg_history_followup",
        "SC_CACHE05_FOLLOWUP: finish.",
        "queue",
    );
    assert!(
        permission_residuals::wait_until(
            client_allowance::CLI_REQUEST_ALLOWANCE,
            permission_residuals::SETTLE_POLL,
            || provider.calls.load(Ordering::SeqCst) >= 8,
        )
        .is_some(),
        "the queued followup never reached the provider"
    );
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=100"),
        "",
    );
    let assistants: Vec<_> = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history absent"))
        .iter()
        .filter(|row| row["type"] == "assistant")
        .collect();
    assert!(assistants.len() >= 5, "missing settled assistant history");
    for row in assistants {
        assert!(
            row.get("observedUsage")
                .is_none_or(|usage| usage.is_null() || usage == &value!({})),
            "missing fixture usage became observed: {row}"
        );
        assert!(
            row.get("tokens").is_none_or(Value::is_null),
            "missing fixture usage became complete compatibility counts: {row}"
        );
    }
    service.stop_after_witness(&root);
    let bodies = locked(&provider.bodies).clone();
    capture(family, &bodies);
    assert_eq!(
        bodies.len(),
        8,
        "discovery, steer, summary and rebuilt coding calls must all run"
    );
    let wire: Vec<Value> = bodies
        .iter()
        .map(|body| {
            serde_json::from_str(body).unwrap_or_else(|error| panic!("actual wire JSON: {error}"))
        })
        .collect();
    for index in [1, 2, 4, 5, 6, 7] {
        assert_fixed_prefix(family, &wire[0], &wire[index]);
    }
    for (before, after) in [(0, 1), (1, 2), (4, 5), (5, 6), (6, 7)] {
        assert_extends(family, &wire[before], &wire[after]);
    }
    assert!(
        bodies[3].contains(SUMMARY_REQUEST),
        "requested compaction never called the summarizer"
    );
    assert!(
        bodies[4].contains("SC_CACHE05_SUMMARY") && bodies[4].contains("SC_CACHE05_POST_COMPACT")
    );
    assert!(
        bodies[2].contains("SC_CACHE05_STEER")
            && bodies[2].contains("LARGE_CATALOG_CALL_SUCCEEDED")
    );
    assert!(
        bodies[1].contains(&"x".repeat(2300)) && bodies[5].contains(&"x".repeat(2300)),
        "complete schema was lost"
    );
    assert!(bodies[6].contains("LARGE_CATALOG_CALL_SUCCEEDED"));
    assert!(
        bodies[0].contains("SC_CACHE05_REQUIRED_PROJECT_CONTEXT")
            && bodies[0].contains("SC_CACHE05_ATTACHMENT_")
    );
    assert!(
        !bodies[0].contains("SC_CACHE05_OPTIONAL_METADATA")
            && !bodies[0].contains("SC_CACHE05_SKILL_BODY")
    );
    let tools = wire[0]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools absent"));
    assert!(tools.iter().any(|tool| tool["name"] == "execute"));
    assert!(!tools.iter().any(|tool| tool["name"]
        .as_str()
        .is_some_and(|name| name.starts_with("auxiliary_"))));
    drop(service);
    process_fixture::cleanup(&root);
}

#[test]
fn chatgpt_discovery_steer_and_compaction_keep_the_cache_prefix() {
    let _journey = process_journey_custody();
    journey(Family::ChatGpt);
}

#[test]
fn anthropic_discovery_steer_and_compaction_keep_the_cache_prefix() {
    let _journey = process_journey_custody();
    journey(Family::Anthropic);
}

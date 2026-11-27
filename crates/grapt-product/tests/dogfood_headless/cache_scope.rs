//! The hidden `prompt_cache_scope` diagnostic on the loopback route:
//! two fresh Sessions of one project send the
//! same body key under a project scope, and the `session-id` header follows
//! the key only under `project-key-and-header`. `session` (and no key at
//! all) is today's per-Session behaviour.
use super::*;

pub(super) fn assert_thread_headers(head: &str, session: &str) {
    for name in ["thread-id", "x-client-request-id"] {
        let identities: Vec<_> = head
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
            })
            .collect();
        assert_eq!(
            identities,
            [session],
            "{name} did not name exactly this Session"
        );
    }
}

/// (body prompt_cache_key, session-id header) of each Session's first call.
fn first_calls(label: &str, scope: Option<&str>) -> Vec<(String, String)> {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 120_000);
    if let Some(scope) = scope {
        let path = root.join("service.conf");
        let mut config = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("service config read failed: {error}"));
        config.push_str(&format!("prompt_cache_scope={scope}\n"));
        fs::write(&path, config)
            .unwrap_or_else(|error| panic!("service config write failed: {error}"));
    }
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first"),
        Script::SuccessText("second"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let mut sessions = Vec::new();
    for id in ["msg_scope_one", "msg_scope_two"] {
        let session = create(address);
        sessions.push(session.clone());
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
    let heads = locked(&provider.heads).clone();
    process_fixture::cleanup(&root);
    assert_eq!(bodies.len(), 2, "{label}: one request per Session");
    bodies
        .iter()
        .zip(&heads)
        .zip(&sessions)
        .map(|((body, head), session)| {
            let body: serde_json::Value = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("{label}: body is not JSON: {error}"));
            let key = body["prompt_cache_key"]
                .as_str()
                .unwrap_or_else(|| panic!("{label}: no prompt_cache_key: {body}"))
                .to_owned();
            let header = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("session-id")
                        .then(|| value.trim().to_owned())
                })
                .unwrap_or_else(|| panic!("{label}: no session-id header: {head}"));
            assert_thread_headers(head, session);
            (key, header)
        })
        .collect()
}

#[test]
fn prompt_cache_scope_shares_the_body_key_and_optionally_the_header() {
    for (label, scope, key_shared, header_shared) in [
        ("cache-scope-absent", None, false, false),
        ("cache-scope-session", Some("session"), false, false),
        ("cache-scope-project-key", Some("project-key"), true, false),
        (
            "cache-scope-project-key-and-header",
            Some("project-key-and-header"),
            true,
            true,
        ),
    ] {
        let calls = first_calls(label, scope);
        let ((key_one, header_one), (key_two, header_two)) = (&calls[0], &calls[1]);
        assert_eq!(
            key_one == key_two,
            key_shared,
            "{label}: body keys {calls:?}"
        );
        assert_eq!(
            header_one == header_two,
            header_shared,
            "{label}: session-id headers {calls:?}"
        );
        // Under `session` and `project-key-and-header` the header IS the key;
        // under `project-key` it stays the Session's own.
        assert_eq!(
            key_one == header_one,
            key_shared == header_shared,
            "{label}: header and key {calls:?}"
        );
    }
}

#[test]
fn an_unknown_prompt_cache_scope_refuses_the_launch() {
    let _journey = process_journey_custody();
    let root = root("cache-scope-unknown");
    prepare(&root, 120_000);
    let path = root.join("service.conf");
    let mut config = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("service config read failed: {error}"));
    config.push_str("prompt_cache_scope=everything\n");
    fs::write(&path, config).unwrap_or_else(|error| panic!("service config write failed: {error}"));
    let provider = ProviderFixture::start(Vec::new());
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .arg("serve")
        .arg("--service")
        .arg(&root)
        .arg("--port")
        .arg("0")
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("refusal launch failed: {error}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let ready = root.join("serve-ready.json").exists();
    process_fixture::cleanup(&root);
    assert!(
        !output.status.success(),
        "an unknown scope launched: {text}"
    );
    assert!(!ready, "an unknown scope published readiness");
    assert!(
        text.contains("prompt_cache_scope")
            && text.contains("expected session|project-key|project-key-and-header"),
        "{text}"
    );
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn serialized_request_diagnostics_follow_the_exact_attempt_and_keep_live_fields_volatile() {
    let _journey = process_journey_custody();
    let root = root("cache-wire-diagnostics");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first"),
        Script::SuccessText("second"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    for id in ["msg_diagnostic_first", "msg_diagnostic_second"] {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id":id,"text":"hello","delivery":"queue","resume":true})
                .to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50&diagnostics=cache"),
        "",
    );
    let rows = messages["data"]
        .as_array()
        .unwrap_or_else(|| panic!("message data absent"));
    let assistants: Vec<_> = rows
        .iter()
        .filter(|row| row["type"] == "assistant")
        .collect();
    assert_eq!(assistants.len(), 2);
    let first = &assistants[0]["metadata"];
    let second = &assistants[1]["metadata"];
    assert_eq!(first["cacheRequestChange"], "first_observation");
    assert_eq!(second["cacheObservedCalls"]["attempts"], 2);
    assert_eq!(
        second["cacheObservedCalls"]["unreported"], 2,
        "the fixture reports no cache-read leaf; this cannot become a complete ratio"
    );
    assert!(first["cacheRequestPreviousAttempt"].is_null());
    assert_eq!(second["cacheRequestChange"], "append_only");
    assert!(second["cacheRequestPreviousAttempt"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 2);
    for (assistant, body) in assistants.iter().zip(&bodies) {
        assert_eq!(
            assistant["metadata"]["cacheRequestBodyBytes"].as_u64(),
            Some(body.len() as u64)
        );
        assert!(assistant["metadata"]["cacheRequestComparison"]
            .as_str()
            .is_some_and(|text| text.contains("provider cause unreported")));
    }
    // Export the same volatile comparisons before the benchmark retires the service.
    let mut exported = Vec::new();
    let written = permission_residuals::wait_until(
        client_allowance::CLI_REQUEST_ALLOWANCE,
        permission_residuals::SETTLE_POLL,
        || {
            let path = root.join("product-stderr-0.log");
            assert!(fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= 1024 * 1024));
            let output = fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("dispatch diagnostic read: {error}"));
            exported = output
                .split_inclusive('\n')
                .filter(|line| line.ends_with('\n'))
                .filter_map(|line| line.split_once(" cache_request="))
                .map(|(_, document)| {
                    serde_json::from_str::<serde_json::Value>(document)
                        .unwrap_or_else(|error| panic!("dispatch comparison JSON: {error}"))
                })
                .collect();
            exported.len() == 2
        },
    );
    assert!(
        written.is_some(),
        "both actual attempts must export their comparisons"
    );
    for (exported, assistant) in exported.iter().zip(&assistants) {
        assert_eq!(exported["session"], session);
        for key in [
            "cacheRequestChange",
            "cacheRequestChangedItem",
            "cacheRequestPreviousAttempt",
            "cacheRequestBodyBytes",
            "cacheRequestToolsBytes",
            "cacheRequestInstructionsBytes",
        ] {
            assert_eq!(exported["metadata"][key], assistant["metadata"][key]);
        }
        assert!(exported["metadata"].get("cacheObservedCalls").is_none());
    }
    // Rehydrating another Session cannot borrow the most recent observation.
    let other = create(address);
    let other = json(
        address,
        "GET",
        &format!("/api/session/{other}/message?order=asc"),
        "",
    );
    assert_eq!(other["data"], serde_json::json!([]));
    let durable = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let durable_rows = durable["data"]
        .as_array()
        .unwrap_or_else(|| panic!("durable message data absent"));
    for row in durable_rows {
        if row["type"] == "assistant" {
            let metadata = row["metadata"]
                .as_object()
                .unwrap_or_else(|| panic!("assistant cost metadata absent"));
            assert_eq!(metadata.keys().map(String::as_str).collect::<Vec<_>>(), vec!["estimatedCost"],
                "ordinary history must retain pinned cost but exclude process-local request diagnostics");
            assert!(
                metadata["estimatedCost"].is_object(),
                "missing usage or prices must still produce an explicit cost status"
            );
        } else {
            assert!(row.get("metadata").is_none());
        }
    }
    child.stop_after_witness(&root);
    drop(child);
    // A new process retains provider usage and pinned cost, but never invents live request observations.
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    assert_eq!(
        json(
            address,
            "GET",
            &format!("/api/session/{session}/message?order=asc&limit=50"),
            ""
        ),
        durable,
        "live diagnostics must not change restart-stable history"
    );
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50&diagnostics=cache"),
        "",
    );
    let rows = messages["data"]
        .as_array()
        .unwrap_or_else(|| panic!("reopened message data absent"));
    for assistant in rows.iter().filter(|row| row["type"] == "assistant") {
        let metadata = assistant["metadata"]
            .as_object()
            .unwrap_or_else(|| panic!("reopened assistant cost metadata absent"));
        assert_eq!(
            metadata.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["estimatedCost"],
            "restart must not resurrect process-local diagnostics"
        );
        assert!(assistant.get("observedUsage").is_some());
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn observed_call_totals_include_the_provider_summary_as_a_separate_attempt() {
    let _journey = process_journey_custody();
    let root = root("cache-compaction-observations");
    prepare(&root, 120_000);
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("summary config read failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("summary config decode failed: {error}"));
    config["compaction"] = serde_json::json!({});
    fs::write(
        &path,
        serde_json::to_vec(&config)
            .unwrap_or_else(|error| panic!("summary config encode failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("summary config write failed: {error}"));
    let measured = |text: &str, input: u64, read: u64| {
        let frames = [
            serde_json::json!({"type":"response.output_text.delta","delta":text}),
            serde_json::json!({"type":"response.output_text.done"}),
            serde_json::json!({"type":"response.completed","response":{"usage":{
                "input_tokens":input,"input_tokens_details":{"cached_tokens":read},
                "output_tokens":1,"total_tokens":input+1,
            }}}),
        ];
        Script::Raw(
            frames
                .iter()
                .map(|frame| format!("data: {frame}\n\n"))
                .collect(),
        )
    };
    let provider = ProviderFixture::start(vec![
        measured("first answer", 100, 50),
        measured("## Objective\n- continue the task", 200, 0),
        measured("answer after summary", 300, 250),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    cache_prompt(
        address,
        &session,
        "msg_cache_before_summary",
        "first request",
    );
    permission_residuals::wait_session_idle(address, &session);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    cache_prompt(
        address,
        &session,
        "msg_cache_after_summary",
        "continue after summary",
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "two coding requests and one provider summary"
    );
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50&diagnostics=cache"),
        "",
    );
    let rows = page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("message page absent"));
    let assistants: Vec<_> = rows
        .iter()
        .filter(|row| row["type"] == "assistant")
        .collect();
    assert_eq!(
        assistants.len(),
        2,
        "summary calls must not masquerade as coding messages"
    );
    let calls = &assistants
        .last()
        .unwrap_or_else(|| panic!("last coding message absent"))["metadata"]["cacheObservedCalls"];
    assert_eq!(
        calls["attempts"], 4,
        "coding, refused provider compaction and SE summary calls"
    );
    assert_eq!(
        calls["unreported"], 1,
        "the refused provider compaction has no usage"
    );
    assert_eq!(calls["pairedInput"], 600);
    assert_eq!(
        calls["pairedRead"], 300,
        "the summary's uncached input belongs in the denominator"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

fn cache_prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    );
}

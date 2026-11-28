use super::*;

fn approve_next(address: SocketAddr, session: &str, action: &str) {
    let wait = PollDeadline::new();
    while wait.pending() {
        let page = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        if let Some(row) = page["data"].as_array().and_then(|rows| rows.first()) {
            assert_eq!(row["action"], action, "unexpected fixture permission");
            super::super::permission_boundaries::reply(address, session, row, "once");
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("CodeMode permission never appeared: {action} ({wait})");
}

#[test]
fn default_stateengine_resumes_then_discovers_reads_and_acks_mail() {
    let _journey = process_journey_custody();
    let root = root("native-codemode-mail");
    prepare(&root, 90_000);
    unattended(&root);
    configure_exposure(&root, "codemode-mail", None);
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_mail_discovery",
            tool: "execute",
            arguments: serde_json::json!({"code":
                "return {read: catalog.describe('stateengine_stateengine_msg_read'), ack: catalog.describe('stateengine_stateengine_msg_ack')};"
            }),
        },
        Script::ToolInput {
            call: "call_mail_effects",
            tool: "execute",
            arguments: serde_json::json!({"code":
                "const read = await tools['stateengine_stateengine_msg_read']({msg_id:'msg_native_long'}); const ack = await tools['stateengine_stateengine_msg_ack']({msg_id:'msg_native_long'}); return {read, ack};"
            }),
        },
        Script::SuccessText("Full fixture mail read and acknowledged through CodeMode."),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_native(address, &root);
    bind(address, &session, true);
    let run = thread::scope(|scope| {
        let run = scope.spawn(|| run_native(&root, &session));
        // The default maintainer preset allows the bounded JavaScript host;
        // nested MCP effects still require their ordinary individual approvals.
        for action in [
            "stateengine_stateengine_msg_read",
            "stateengine_stateengine_msg_ack",
        ] {
            approve_next(address, &session, action);
        }
        run.join().unwrap_or_else(|_| panic!("mail CLI panicked"))
    });
    assert_eq!(run.code, Some(0), "CodeMode mail failed: {}", run.stderr);
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 3);
    let first: serde_json::Value =
        serde_json::from_str(&bodies[0]).unwrap_or_else(|error| panic!("provider body: {error}"));
    let tools = first["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("missing tool catalog"));
    assert!(tools.iter().any(|tool| tool["name"] == "execute"));
    assert!(!tools.iter().any(|tool| tool["name"]
        .as_str()
        .is_some_and(|name| name.starts_with("stateengine_"))));
    assert!(
        bodies[0].contains("NATIVE_SOURCE_VICTOR6284"),
        "native resume missing"
    );
    assert!(
        !bodies[0].contains("msg_id"),
        "MCP schemas offered before discovery"
    );
    assert!(
        bodies[1].contains("msg_id"),
        "discovered schemas absent from history"
    );
    assert!(!bodies[1].contains("FULL_BODY_END"));
    for evidence in ["FULL_BODY_END", "delivery_acknowledgement", "recorded"] {
        assert!(
            bodies[2].contains(evidence),
            "missing mail evidence: {evidence}"
        );
    }
    for body in bodies.iter() {
        assert!(!body.contains("NATIVE_FIXTURE_CAPABILITY"));
        assert!(!body.contains("NATIVE_FIXTURE_PRIVATE_CURSOR"));
    }
    drop(bodies);
    crate::dispatch_shadow::assert_nested_mail_dispatches(&root);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn codemode_guest_throw_reaches_model_and_transcript_with_safe_type() {
    let _journey = process_journey_custody();
    let root = root("native-codemode-guest-throw");
    prepare(&root, 90_000);
    unattended(&root);
    configure_exposure(&root, "codemode-mail", None);
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_guest_throw",
            tool: "execute",
            arguments: serde_json::json!({"code":
                "const shape = {}; if (!shape.items) throw new RangeError('items missing from shape');"
            }),
        },
        Script::SuccessText("Saw the guest's own error."),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_native(address, &root);
    bind(address, &session, true);
    let run = run_native(&root, &session);
    assert_eq!(run.code, Some(0), "CodeMode run failed: {}", run.stderr);
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 2);
    let next: serde_json::Value =
        serde_json::from_str(&bodies[1]).unwrap_or_else(|error| panic!("provider body: {error}"));
    let outputs = next["input"]
        .as_array()
        .unwrap_or_else(|| panic!("model input absent"))
        .iter()
        .filter(|row| row["type"] == "function_call_output")
        .map(|row| row["output"].to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        outputs.contains("CodeMode [range_error]"),
        "typed cause absent"
    );
    assert!(
        !outputs.contains("items missing from shape"),
        "private guest suffix replayed"
    );
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=20"),
        "",
    );
    let errors = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history absent"))
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["state"]["error"]["message"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(errors.contains("CodeMode [range_error]"));
    assert!(!errors.contains("items missing from shape"));
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn uncaught_mcp_census_failures_name_the_server_tool_and_actual_cause() {
    let _journey = process_journey_custody();
    let root = root("native-codemode-census-failures");
    prepare(&root, 90_000);
    unattended(&root);
    configure_exposure(&root, "census-failures", None);
    let config_path = root.join("grapt.jsonc");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap_or_else(|e| panic!("config: {e}")))
            .unwrap_or_else(|e| panic!("JSON: {e}"));
    config["permission"] = serde_json::json!({"stateengine_*":"allow"});
    fs::write(&config_path, config.to_string()).unwrap_or_else(|e| panic!("config: {e}"));
    let mut deep = serde_json::json!("NATIVE_FIXTURE_DEEP");
    for _ in 0..20 {
        deep = serde_json::json!({"child": deep});
    }
    let cases = [
        (
            "call_census_refs",
            "stateengine_msg_send",
            serde_json::json!({"to":"agt_native_sender","kind":"task-pointer","body":"fixture only"}),
            "requires at least one ref",
        ),
        (
            "call_census_limit_200",
            "stateengine_msg_list",
            serde_json::json!({"unread_only":true,"limit":200}),
            "1048576",
        ),
        (
            "call_census_limit_100",
            "stateengine_msg_list",
            serde_json::json!({"unread_only":true,"limit":100}),
            "1048576",
        ),
        (
            "call_census_before",
            "stateengine_msg_list",
            serde_json::json!({"unread_only":true,"before":"2026-10-30T06:01:22.149Z","limit":20}),
            "require dismiss=true",
        ),
        (
            "call_nested_mutation",
            "stateengine_msg_send",
            serde_json::json!({"nested":{"object":"NATIVE_FIXTURE_PRIVATE_NESTED","array":["NATIVE_FIXTURE_PRIVATE_ARRAY"]}}),
            "nested request refused",
        ),
        (
            "call_private_bound",
            "stateengine_msg_send",
            serde_json::json!({"nested":deep}),
            "private argument bound",
        ),
        (
            "call_protocol_escape",
            "stateengine_msg_send",
            serde_json::json!({"protocol":true,"nested":{"object":"NATIVE_FIXTURE_ESCAPED:\"line\nrest"}}),
            "nested protocol refused",
        ),
    ];
    let mut scripts = cases.iter().map(|(call, tool, input, _)| Script::ToolInput {
        call,
        tool: "execute",
        arguments: serde_json::json!({"code": if *call == "call_nested_mutation" {
            format!("try {{ return await tools.stateengine_{tool}({input}); }} catch(e) {{ e.message='FORGED_GUEST_CAUSE'; e.tool='FORGED_TOOL'; e.error='authority_unavailable'; throw e; }}")
        } else { format!("return await tools.stateengine_{tool}({input});") }}),
    }).collect::<Vec<_>>();
    scripts.push(Script::SuccessText(
        "Observed the bounded fixture failures.",
    ));
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_native(address, &root);
    bind(address, &session, true);
    let run = run_native(&root, &session);
    assert_eq!(run.code, Some(0), "fixture CLI: {}", run.stderr);
    let bodies = locked(&provider.bodies);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=20"),
        "",
    );
    let history_text = history.to_string();
    for (index, (call, tool, _, cause)) in cases.iter().enumerate() {
        let body: serde_json::Value =
            serde_json::from_str(&bodies[index + 1]).unwrap_or_else(|e| panic!("provider: {e}"));
        let output = body["input"]
            .as_array()
            .unwrap_or_else(|| panic!("input"))
            .iter()
            .find(|row| row["type"] == "function_call_output" && row["call_id"] == *call)
            .unwrap_or_else(|| panic!("missing output {call}"))
            .to_string();
        for expected in ["stateengine", tool, cause] {
            assert!(
                output.contains(expected),
                "model lost actual cause {expected}: {output}"
            );
            assert!(
                history_text.contains(expected),
                "transcript lost actual cause {expected}"
            );
        }
        assert!(!output.contains("NATIVE_FIXTURE_CAPABILITY"));
        for private in [
            "NATIVE_FIXTURE_PRIVATE_CURSOR",
            "NATIVE_FIXTURE_PRIVATE_NESTED",
            "NATIVE_FIXTURE_PRIVATE_ARRAY",
            "NATIVE_FIXTURE_DEEP",
            "FORGED_GUEST_CAUSE",
            "FORGED_TOOL",
            "NATIVE_FIXTURE_ESCAPED",
        ] {
            assert!(
                !output.contains(private),
                "private or forged cause reached model: {output}"
            );
        }
        if *call == "call_protocol_escape" {
            assert!(
                output.contains("-32041"),
                "server error code lost: {output}"
            );
        }
        if *call == "call_nested_mutation" {
            assert!(
                output.contains("[private]"),
                "nested values were not redacted: {output}"
            );
        }
    }
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

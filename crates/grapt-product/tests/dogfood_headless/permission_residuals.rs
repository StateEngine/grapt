use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn permission_http_closes_location_session_status_and_agent_policy_residuals() {
    let _journey = process_journey_custody();
    let root = root("permission-residuals");
    prepare(&root, 6_000);
    attended(&root);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");

    let (default_status, _) = request(address, "GET", "/api/permission/request", "");
    assert_eq!(
        default_status, 200,
        "default location must select the product root"
    );
    let explicit_path = format!(
        "/api/permission/request?location%5Bdirectory%5D={}",
        percent_encode(&root.to_string_lossy())
    );
    let (explicit_status, _) = request(address, "GET", &explicit_path, "");
    assert_eq!(
        explicit_status, 200,
        "explicit current location must be accepted"
    );
    let (foreign_status, _) = request(
        address,
        "GET",
        "/api/permission/request?location%5Bdirectory%5D=%2Fforeign",
        "",
    );
    assert_eq!(
        foreign_status, 400,
        "foreign location must not fall back to default"
    );
    let (malformed_status, _) = request(
        address,
        "GET",
        "/api/permission/request?location=malformed",
        "",
    );
    assert_eq!(
        malformed_status, 400,
        "malformed location must be a typed refusal"
    );

    let missing = "ses_permission_missing";
    let (missing_list, missing_list_body) = request(
        address,
        "GET",
        &format!("/api/session/{missing}/permission"),
        "",
    );
    let (missing_get, missing_get_body) = request(
        address,
        "GET",
        &format!("/api/session/{missing}/permission/per_missing"),
        "",
    );
    let reply_body = r#"{"reply":"once","requestID":"req_missing","nonce":"nonce_missing"}"#;
    let (missing_reply, missing_reply_body) = request(
        address,
        "POST",
        &format!("/api/session/{missing}/permission/per_missing/reply"),
        reply_body,
    );
    assert_eq!(
        missing_list, 404,
        "permission list must distinguish a missing session from an empty list"
    );
    assert_eq!(
        missing_get, 404,
        "permission get must distinguish a missing session from a missing request"
    );
    assert_eq!(
        missing_reply, 404,
        "permission reply must distinguish a missing session before decision lookup"
    );
    for (operation, evidence, body) in [
        (
            "list",
            "permission list must distinguish a missing session from an empty list",
            missing_list_body,
        ),
        (
            "get",
            "permission get must distinguish a missing session from a missing request",
            missing_get_body,
        ),
        (
            "reply",
            "permission reply must distinguish a missing session before decision lookup",
            missing_reply_body,
        ),
    ] {
        let error = serde_json::from_slice::<serde_json::Value>(&body)
            .unwrap_or_else(|cause| panic!("missing-session {operation} was not JSON: {cause}"));
        assert_eq!(error["_tag"], "SessionNotFoundError", "{evidence}");
        assert_eq!(
            error["sessionID"], missing,
            "permission {operation} must preserve the missing-session coordinate"
        );
    }

    let session = json(address, "POST", "/api/session", "{}");
    let session_id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session response carried no id"));
    let create_path = format!("/api/session/{session_id}/permission");
    let configured = json(
        address,
        "POST",
        &create_path,
        r#"{"id":"per_configured","agent":"agt_dogfood_allow","action":"write","resources":["/work/configured"]}"#,
    );
    assert_eq!(
        configured["data"]["effect"], "allow",
        "configured agent policy must be evaluated rather than discarded"
    );
    let asked = json(
        address,
        "POST",
        &create_path,
        r#"{"id":"per_endpoint","agent":"agt_live_headless1","action":"write","resources":["/work/ask"]}"#,
    );
    assert_eq!(asked["data"]["effect"], "ask");
    let reply_path = format!("/api/session/{session_id}/permission/per_endpoint/reply");
    for malformed in [
        r#"{"reply":"once","requestID":"req_partial"}"#,
        r#"{"reply":"once","nonce":"nonce_partial"}"#,
        r#"{"reply":"once","requestID":null,"nonce":null}"#,
    ] {
        let (status, _) = request(address, "POST", &reply_path, malformed);
        assert_eq!(
            status, 400,
            "partial/null decision identity reached settlement"
        );
    }
    let first_reply = r#"{"reply":"once"}"#;
    let (settled, _) = request(address, "POST", &reply_path, first_reply);
    assert_eq!(settled, 204, "first live reply must settle with no content");
    let second_reply =
        r#"{"reply":"reject","requestID":"req_endpoint_second","nonce":"nonce_endpoint_second"}"#;
    let (conflict, _) = request(address, "POST", &reply_path, second_reply);
    assert_eq!(
        conflict, 409,
        "a conflicting terminal reply must be a conflict"
    );

    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("permission product wait failed: {error}"));
    assert!(
        output.status.success(),
        "permission product refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(&root)
        .unwrap_or_else(|error| panic!("permission root cleanup failed: {error}"));
}

#[test]
fn preparation_failure_detail_reaches_the_next_real_provider_request() {
    let _journey = process_journey_custody();
    let root = root("tool-preparation-detail");
    prepare(&root, 8_000);
    prepare_model_inventory(&root, 128_000);
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_missing_read",
            // Missing control paths still fail preparation rather than list.
            path: ".git/missing.txt",
        },
        Script::Success,
    ]);
    let provider_bodies = Arc::clone(&provider.bodies);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session_id = create(address);
    let prompt = json(
        address,
        "POST",
        &format!("/api/session/{session_id}/prompt"),
        r#"{"id":"msg_tool_preparation_detail","text":"read the missing file","delivery":"queue","resume":true}"#,
    );
    assert!(prompt.to_string().contains("msg_tool_preparation_detail"));
    wait_session_idle(address, &session_id);

    let bodies = locked(&provider_bodies).clone();
    assert_eq!(bodies.len(), 2);
    let continued: serde_json::Value = serde_json::from_str(&bodies[1])
        .unwrap_or_else(|error| panic!("continued provider request was not JSON: {error}"));
    let output = continued["input"]
        .as_array()
        .and_then(|input| input.last())
        .and_then(|item| item.get("output"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("continued request carried no tool failure output: {continued}"));
    assert!(output.contains("tool preparation failed"));
    assert!(output.contains("missing.txt"));

    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("preparation-detail product wait failed: {error}"));
    assert!(
        output.status.success(),
        "preparation-detail product refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(&root)
        .unwrap_or_else(|error| panic!("preparation-detail root cleanup failed: {error}"));
}

/// The ask is durable work: observe its named tool, allowing the same
/// admission time as the client instead of guessing from 100 quick polls.
pub(super) fn wait_tool_ask(
    call: &str,
    mut list: impl FnMut() -> serde_json::Value,
) -> serde_json::Value {
    let mut ask = None;
    let settled = wait_until(client_allowance::CLI_REQUEST_ALLOWANCE, SETTLE_POLL, || {
        let listed = list();
        let rows = listed["data"]
            .as_array()
            .unwrap_or_else(|| panic!("permission list was not an array: {listed}"));
        ask = rows
            .iter()
            .find(|row| row["source"]["type"] == "tool" && row["source"]["callID"] == call)
            .cloned();
        ask.is_some()
    });
    assert!(
        settled.is_some(),
        "tool permission ask was not displayed: {call}"
    );
    ask.unwrap_or_else(|| panic!("named permission ask was not observed: {call}"))
}

#[test]
fn a_named_tool_ask_can_arrive_after_the_old_poll_bound() {
    let unrelated =
        serde_json::json!({"id":"per_other", "source":{"type":"tool", "callID":"other"}});
    let expected =
        serde_json::json!({"id":"per_named", "source":{"type":"tool", "callID":"named"}});
    let mut polls = 0;
    let ask = wait_tool_ask("named", || {
        polls += 1;
        let mut rows = vec![unrelated.clone()];
        if polls > 100 {
            rows.push(expected.clone());
        }
        serde_json::json!({"data":rows})
    });
    assert_eq!(polls, 101, "an unrelated ask settled the witness");
    assert_eq!(ask, expected);
}

#[test]
fn rejected_tool_permission_gives_the_model_specific_retry_guidance() {
    let _journey = process_journey_custody();
    let root = root("tool-rejection-detail");
    prepare(&root, 8_000);
    attended(&root);
    prepare_model_inventory(&root, 128_000);
    // Replacing an existing file asks under author.
    fs::write(root.join("tool.txt"), b"must not be replaced")
        .unwrap_or_else(|error| panic!("tool fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        write_call("call_rejected_read", "tool.txt", "replaced\n"),
        Script::Success,
    ]);
    let provider_bodies = Arc::clone(&provider.bodies);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session_id = create(address);
    let prompt_path = format!("/api/session/{session_id}/prompt");
    let prompt = thread::spawn(move || {
        request(
            address,
            "POST",
            &prompt_path,
            r#"{"id":"msg_tool_rejection_detail","text":"read tool.txt","delivery":"queue","resume":true}"#,
        )
    });
    let list_path = format!("/api/session/{session_id}/permission");
    let permission = wait_tool_ask("call_rejected_read", || {
        json(address, "GET", &list_path, "")
    });
    let permission_id = permission["id"]
        .as_str()
        .unwrap_or_else(|| panic!("tool permission carried no id"));
    let reply_path = format!("/api/session/{session_id}/permission/{permission_id}/reply");
    let (reply_status, _) = request(address, "POST", &reply_path, r#"{"reply":"reject"}"#);
    assert_eq!(reply_status, 204);
    let (prompt_status, prompt_body) = prompt
        .join()
        .unwrap_or_else(|_| panic!("tool prompt thread panicked"));
    assert_eq!(
        prompt_status,
        200,
        "rejected tool prompt failed: {}",
        String::from_utf8_lossy(&prompt_body)
    );
    wait_session_idle(address, &session_id);

    let bodies = locked(&provider_bodies).clone();
    assert_eq!(bodies.len(), 2);
    let continued: serde_json::Value = serde_json::from_str(&bodies[1])
        .unwrap_or_else(|error| panic!("continued provider request was not JSON: {error}"));
    let output = continued["input"]
        .as_array()
        .and_then(|input| input.last())
        .and_then(|item| item.get("output"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("continued request carried no rejection output: {continued}"));
    assert!(output.contains("permission request was rejected"));
    assert!(output.contains("do not repeat the call unless the decision changes"));
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session_id}/message"),
        "",
    );
    let rejected = page["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .find(|part| part["id"] == "call_rejected_read")
        .unwrap_or_else(|| panic!("public history omitted the rejected tool call: {page}"));
    assert_eq!(rejected["state"]["status"], "error");
    assert!(rejected["state"]["content"][0]["text"]
        .as_str()
        .is_some_and(
            |detail| detail.contains("do not repeat the call unless the decision changes")
        ));
    assert_eq!(
        fs::read(root.join("tool.txt")).unwrap_or_default(),
        b"must not be replaced",
        "a rejected overwrite replaced the file"
    );

    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("rejection-detail product wait failed: {error}"));
    assert!(
        output.status.success(),
        "rejection-detail product refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(&root)
        .unwrap_or_else(|error| panic!("rejection-detail root cleanup failed: {error}"));
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn provider_tool_call_waits_for_once_reply_then_runs_the_project_read() {
    let _journey = process_journey_custody();
    let root = root("tool-permission-once");
    prepare(&root, 8_000);
    attended(&root);
    fs::write(root.join("tool.txt"), b"permission-gated read")
        .unwrap_or_else(|error| panic!("tool fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        // Replacing an existing file asks under author.
        write_call("call_read_once", "tool.txt", "permission-gated write\n"),
        Script::Success,
        Script::Success,
    ]);
    let provider_calls = Arc::clone(&provider.calls);
    let provider_bodies = Arc::clone(&provider.bodies);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");

    let session = json(address, "POST", "/api/session", "{}");
    let session_id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session response carried no id"))
        .to_owned();
    let prompt_path = format!("/api/session/{session_id}/prompt");
    let prompt_address = address;
    let prompt = thread::spawn(move || {
        request(
            prompt_address,
            "POST",
            &prompt_path,
            r#"{"id":"msg_tool_permission_once","text":"read tool.txt","delivery":"queue","resume":true}"#,
        )
    });

    let list_path = format!("/api/session/{session_id}/permission");
    let permission = wait_tool_ask("call_read_once", || json(address, "GET", &list_path, ""));
    // A real provider request has crossed its send boundary and is waiting
    // for a human. Missing billing must not evict the Session during hydration.
    let hydrated = json(address, "GET", &format!("/api/session/{session_id}"), "");
    assert_eq!(hydrated["data"]["id"], session_id);
    assert!(hydrated["data"].get("cost").is_none());
    assert_eq!(hydrated["data"]["costStatus"], "unavailable");
    assert_eq!(permission["action"], "fs.overwrite");
    assert_eq!(permission["source"]["type"], "tool");
    assert_eq!(permission["source"]["callID"], "call_read_once");
    assert_eq!(
        permission["source"]["messageID"],
        "msg_tool_permission_once"
    );
    let permission_id = permission["id"]
        .as_str()
        .unwrap_or_else(|| panic!("tool permission carried no id"));
    let reply_path = format!("/api/session/{session_id}/permission/{permission_id}/reply");
    let (reply_status, _) = request(address, "POST", &reply_path, r#"{"reply":"once"}"#);
    assert_eq!(
        reply_status, 204,
        "live once reply must settle the tool ask"
    );

    let (prompt_status, prompt_body) = prompt
        .join()
        .unwrap_or_else(|_| panic!("tool prompt thread panicked"));
    assert_eq!(
        prompt_status,
        200,
        "permission-settled tool prompt failed: {}",
        String::from_utf8_lossy(&prompt_body)
    );
    wait_session_idle(address, &session_id);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    let settled = json(address, "GET", &list_path, "");
    assert_eq!(settled["data"], serde_json::json!([]));
    let messages_path = format!("/api/session/{session_id}/message");
    let first_run_messages = json(address, "GET", &messages_path, "");
    assert!(
        first_run_messages
            .to_string()
            .contains("Hello from the live provider."),
        "the tool continuation did not commit its final assistant completion"
    );

    let bodies = locked(&provider_bodies).clone();
    assert_eq!(bodies.len(), 2);
    let initial: serde_json::Value = serde_json::from_str(&bodies[0])
        .unwrap_or_else(|error| panic!("initial tool request was not JSON: {error}"));
    let continued: serde_json::Value = serde_json::from_str(&bodies[1])
        .unwrap_or_else(|error| panic!("continued tool request was not JSON: {error}"));
    let initial_cache_key = initial["prompt_cache_key"]
        .as_str()
        .unwrap_or_else(|| panic!("initial tool request omitted its prompt cache key"));
    assert!(!initial_cache_key.is_empty());
    assert!(initial_cache_key.len() <= 64);
    assert_eq!(continued["prompt_cache_key"], initial["prompt_cache_key"]);
    assert_eq!(continued["model"], initial["model"]);
    assert_eq!(continued["store"], initial["store"]);
    assert_eq!(continued["stream"], initial["stream"]);
    assert_eq!(continued["tool_choice"], initial["tool_choice"]);
    assert_eq!(continued["tools"], initial["tools"]);
    let initial_items = initial["input"]
        .as_array()
        .unwrap_or_else(|| panic!("initial request omitted input: {initial}"));
    let continued_items = continued["input"]
        .as_array()
        .unwrap_or_else(|| panic!("continued request omitted input: {continued}"));
    assert!(
        continued_items.starts_with(initial_items),
        "continuation changed its cached prefix"
    );
    let user = initial_items
        .last()
        .unwrap_or_else(|| panic!("initial input is empty"));
    assert_eq!(user["role"], "user");
    assert_eq!(user["content"], "read tool.txt");
    let effects = &continued_items[initial_items.len()..];
    let call = effects
        .iter()
        .find(|item| item["type"] == "function_call" && item["call_id"] == "call_read_once")
        .unwrap_or_else(|| panic!("continuation omitted the approved call"));
    assert_eq!(call["name"], "write");
    assert_eq!(
        call["arguments"],
        serde_json::json!({"path":"tool.txt","content":"permission-gated write\n"}).to_string()
    );
    let output = effects
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "call_read_once")
        .unwrap_or_else(|| panic!("continuation omitted the approved result"));
    let written = output["output"]
        .as_str()
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| panic!("the approved write returned no output: {continued}"))
        .to_owned();
    assert_eq!(
        fs::read(root.join("tool.txt")).unwrap_or_default(),
        b"permission-gated write\n",
        "the approved overwrite did not replace the file"
    );

    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("tool product wait failed: {error}"));
    assert!(
        output.status.success(),
        "tool product refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut reopened = process_fixture::spawn_controlled(&root, provider.address);
    let reopened_address = await_ready(&root, &mut reopened);
    assert_eq!(
        json(reopened_address, "GET", &messages_path, ""),
        first_run_messages,
        "reopened durable history drifted before new work"
    );
    let reopened_prompt_path = format!("/api/session/{session_id}/prompt");
    let reopened_prompt = json(
        reopened_address,
        "POST",
        &reopened_prompt_path,
        r#"{"id":"msg_tool_permission_reopen","text":"new work after reopen","delivery":"queue","resume":true}"#,
    );
    wait_session_idle(reopened_address, &session_id);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 3);
    assert!(
        reopened_prompt
            .to_string()
            .contains("msg_tool_permission_reopen"),
        "reopened prompt did not settle its own input"
    );
    let bodies = locked(&provider_bodies).clone();
    let reopened_body: serde_json::Value = serde_json::from_str(&bodies[2])
        .unwrap_or_else(|error| panic!("reopened provider request was not JSON: {error}"));
    assert_eq!(
        reopened_body["prompt_cache_key"], initial["prompt_cache_key"],
        "one durable Session changed cache buckets after restart"
    );
    assert_eq!(
        reopened_body["tools"], initial["tools"],
        "restart changed the stable tool-definition prefix"
    );
    let mut expected_context = continued["input"]
        .as_array()
        .unwrap_or_else(|| panic!("continued input was not an array"))
        .clone();
    expected_context
        .push(serde_json::json!({"role":"assistant","content":"Hello from the live provider."}));
    expected_context.push(serde_json::json!({"role":"user","content":"new work after reopen"}));
    assert_eq!(
        reopened_body["input"],
        serde_json::json!(expected_context),
        "reopening lost the prior durable tool exchange or assistant context"
    );
    let reopened_messages = json(reopened_address, "GET", &messages_path, "");
    assert!(
        reopened_messages
            .to_string()
            .contains("Hello from the live provider."),
        "the reopened Session lost its final assistant completion"
    );
    let reopened_output = reopened
        .wait_with_output()
        .unwrap_or_else(|error| panic!("reopened tool product wait failed: {error}"));
    assert!(
        reopened_output.status.success(),
        "reopened tool product refused: {}",
        String::from_utf8_lossy(&reopened_output.stderr)
    );

    let store = EventStore::open(&root.join("database.sqlite3"), "live-headless1")
        .unwrap_or_else(|error| panic!("tool witness store reopen failed: {error}"));
    let tool_terminals: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = 'session.tool.terminal.1'",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("tool terminal count failed: {error}"));
    assert_eq!(
        tool_terminals, 1,
        "restart or continuation replayed the governed tool effect"
    );
    drop(store);

    let objects = root.join("dogfood-artifacts/objects");
    let mut captured_read = false;
    for shard in fs::read_dir(&objects)
        .unwrap_or_else(|error| panic!("tool artifact objects were unreadable: {error}"))
    {
        let shard =
            shard.unwrap_or_else(|error| panic!("tool artifact shard was unreadable: {error}"));
        for object in fs::read_dir(shard.path())
            .unwrap_or_else(|error| panic!("tool artifact shard contents were unreadable: {error}"))
        {
            let object = object
                .unwrap_or_else(|error| panic!("tool artifact object was unreadable: {error}"));
            let bytes = fs::read(object.path())
                .unwrap_or_else(|error| panic!("tool artifact bytes were unreadable: {error}"));
            captured_read |= bytes == written.as_bytes();
        }
    }
    assert!(
        captured_read,
        "the permission-settled write did not capture its exact output artifact"
    );
    fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("tool root cleanup failed: {error}"));
}

#[test]
fn interrupt_cancels_pending_tool_permission_and_restart_cannot_resurrect_it() {
    let _journey = process_journey_custody();
    let root = root("tool-permission-interrupt");
    prepare(&root, 4_000);
    attended(&root);
    fs::write(root.join("tool.txt"), b"must not be read")
        .unwrap_or_else(|error| panic!("tool fixture write failed: {error}"));
    // Replacing an existing file asks under author.
    let provider = ProviderFixture::start(vec![write_call(
        "call_read_cancel",
        "tool.txt",
        "must not be written\n",
    )]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = json(address, "POST", "/api/session", "{}");
    let id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session response carried no id"));
    let prompt_path = format!("/api/session/{id}/prompt");
    let prompt = json(
        address,
        "POST",
        &prompt_path,
        r#"{"id":"msg_tool_cancel","text":"read tool.txt","delivery":"queue","resume":true}"#,
    );
    assert!(prompt.to_string().contains("msg_tool_cancel"));
    let list_path = format!("/api/session/{id}/permission");
    let wait = PollDeadline::new();
    let permission = wait
        .poll(Duration::from_millis(10), || {
            let listed = json(address, "GET", &list_path, "");
            listed["data"]
                .as_array()
                .and_then(|rows| rows.first())
                .cloned()
        })
        .unwrap_or_else(|| panic!("tool permission never became pending"));
    assert_eq!(permission["source"]["callID"], "call_read_cancel");
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{id}/interrupt"),
        "{}",
    );
    assert_eq!(status, 204);
    wait_session_idle(address, id);
    assert_eq!(
        json(address, "GET", &list_path, "")["data"],
        serde_json::json!([])
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "cancelled tool continued the provider"
    );
    let permission_id = permission["id"]
        .as_str()
        .unwrap_or_else(|| panic!("pending permission omitted id"));
    let reply_path = format!("/api/session/{id}/permission/{permission_id}/reply");
    let (late, _) = request(address, "POST", &reply_path, r#"{"reply":"once"}"#);
    assert_eq!(late, 409, "late approval resurrected the interrupted tool");
    let messages_path = format!("/api/session/{id}/message");
    let messages = json(address, "GET", &messages_path, "");
    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("cancel product wait failed: {error}"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reopened = process_fixture::spawn_controlled(&root, provider.address);
    let reopened_address = await_ready(&root, &mut reopened);
    assert_eq!(
        json(reopened_address, "GET", &list_path, "")["data"],
        serde_json::json!([])
    );
    assert_eq!(json(reopened_address, "GET", &messages_path, ""), messages);
    let output = reopened
        .wait_with_output()
        .unwrap_or_else(|error| panic!("reopened cancel product wait failed: {error}"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = EventStore::open(&root.join("database.sqlite3"), "live-headless1")
        .unwrap_or_else(|error| panic!("cancel store reopen failed: {error}"));
    let cancellations: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = 'permission.cancelled.1'",
        [id], |row| row.get(0)).unwrap_or_else(|error| panic!("cancel evidence unreadable: {error}"));
    assert_eq!(cancellations, 1);
    drop(store);
    fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("cancel root cleanup failed: {error}"));
}

/// Wait for `session` to settle: no longer listed as active. The bound is
/// the CLI's own request allowance, as long as a client waits for a prompt;
/// it was 200 polls of 10 ms (2 s), which a loaded box running a shell child
/// and two provider round trips overran, failing journeys whose product was
/// still working. Polling stays at 10 ms, so a fast
/// settle returns as fast as before.
pub(super) fn wait_session_idle(address: SocketAddr, session: &str) {
    let bound = super::client_allowance::CLI_REQUEST_ALLOWANCE;
    let settled = wait_until(bound, SETTLE_POLL, || {
        let active = json(address, "GET", "/api/session/active", "");
        let active = active["data"]
            .as_object()
            .unwrap_or_else(|| panic!("active execution response was not an object"));
        !active.contains_key(session)
    });
    if settled.is_none() {
        panic!("the admitted Session did not settle within the witness bound ({bound:?})");
    }
}

/// How often a settle wait polls.
pub(super) const SETTLE_POLL: Duration = Duration::from_millis(10);

/// Poll `done` every `poll` until it holds or `bound` passes: how long it
/// took, or `None` when it never held.
pub(super) fn wait_until(
    bound: Duration,
    poll: Duration,
    mut done: impl FnMut() -> bool,
) -> Option<Duration> {
    let wait = PollDeadline::within(bound);
    wait.poll(poll, || done().then(|| wait.elapsed()))
}

/// The longer bound adds no latency: a Session that is
/// already idle is seen on the first poll.
#[test]
fn a_settled_session_is_seen_without_waiting_out_the_bound() {
    let took = wait_until(Duration::from_secs(30), SETTLE_POLL, || true)
        .unwrap_or_else(|| panic!("an immediate condition was not seen"));
    assert!(took < SETTLE_POLL, "{took:?}");
    let mut polls = 0;
    let took = wait_until(Duration::from_secs(30), SETTLE_POLL, || {
        polls += 1;
        polls == 3
    })
    .unwrap_or_else(|| panic!("the condition was not seen"));
    assert!(took < SETTLE_POLL * 10, "{took:?}");
    assert!(wait_until(Duration::from_millis(30), SETTLE_POLL, || false).is_none());
}

#[test]
fn coding_chain_continues_past_two_tools_to_a_final_answer() {
    let _journey = process_journey_custody();
    let root = root("coding-chain-allowance");
    // Service lifetime bounds cleanup; completing three tool calls is the witness.
    // A five-second expiry could close the socket while that witness was running.
    prepare(&root, 120_000);
    fs::write(root.join("tool.txt"), b"chain verification")
        .unwrap_or_else(|error| panic!("chain fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_chain_1",
            path: "tool.txt",
        },
        Script::ToolRead {
            call: "call_chain_2",
            path: "tool.txt",
        },
        Script::ToolRead {
            call: "call_chain_3",
            path: "tool.txt",
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = json(address, "POST", "/api/session", "{}");
    let id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{id}/prompt"),
        r#"{"id":"msg_coding_chain","text":"complete the coding checks","delivery":"queue","resume":true}"#,
    );
    // Reads run unasked under the default preset; each call still has to
    // run before the chain can reach its answer.
    for ordinal in 1..=3 {
        super::compaction_continuity::approve(address, id, &format!("call_chain_{ordinal}"));
    }
    wait_session_idle(address, id);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "coding chain omitted the final answer"
    );
    assert!(
        json(address, "GET", &format!("/api/session/{id}/message"), "")
            .to_string()
            .contains("Hello from the live provider.")
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

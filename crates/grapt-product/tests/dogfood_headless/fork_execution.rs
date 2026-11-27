//! FORK-SESSION-EXECUTION-01: a prompt in a forked Session executes through the
//! ordinary product path and carries the parent conversation up to the fork
//! point, against the real product.

use super::permission_residuals::wait_session_idle;
use super::*;

#[path = "fork_execution/instruction_epoch.rs"]
mod instruction_epoch;

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    let body = format!(r#"{{"id":"{id}","text":"{text}","delivery":"queue","resume":true}}"#);
    let admitted = json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &body,
    );
    assert_eq!(admitted["data"]["id"], id, "prompt identity was not kept");
    wait_session_idle(address, session);
}

fn fork(address: SocketAddr, session: &str) -> String {
    let forked = json(
        address,
        "POST",
        &format!("/api/session/{session}/fork"),
        "{}",
    );
    assert_eq!(
        forked["data"]["parentID"], session,
        "the fork names no parent"
    );
    forked["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the fork named no Session"))
        .to_owned()
}

/// A Session's durable history, oldest first: (type, id, text) per row.
fn turns(address: SocketAddr, session: &str) -> Vec<(String, String, String)> {
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=200"),
        "",
    );
    page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("{session} history is not a list"))
        .iter()
        .map(|row| {
            let text = match row["type"].as_str() {
                Some("assistant") => row["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|item| {
                        // A settled tool item carries its durable terminal
                        // output in state.content; a text item in item.text.
                        if item["type"] == "tool" {
                            item["state"]["content"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|part| part["text"].as_str())
                                .collect::<String>()
                        } else {
                            item["text"].as_str().unwrap_or_default().to_owned()
                        }
                    })
                    .collect::<String>(),
                _ => row["text"].as_str().unwrap_or_default().to_owned(),
            };
            (
                row["type"].as_str().unwrap_or_default().to_owned(),
                row["id"].as_str().unwrap_or_default().to_owned(),
                text,
            )
        })
        .collect()
}

fn texts(turns: &[(String, String, String)]) -> Vec<(&str, &str)> {
    turns
        .iter()
        .map(|(kind, _, text)| (kind.as_str(), text.as_str()))
        .collect()
}

fn request_body(provider: &ProviderFixture, call: usize) -> String {
    provider
        .bodies
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(call)
        .cloned()
        .unwrap_or_else(|| panic!("provider call {call} was never made"))
}

/// A prompt in a fork reaches the provider with the parent's conversation up
/// to the fork point and nothing after it; the parent is unaffected; a fork of
/// the fork carries both.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_prompt_in_a_fork_carries_the_parent_conversation_and_settles() {
    let _journey = process_journey_custody();
    let root = root("fork-execution");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("parent answer"),
        Script::SuccessText("parent later answer"),
        Script::SuccessText("fork answer"),
        Script::SuccessText("grandchild answer"),
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let _approver = approval::Attachment::open(address, "*");

    let parent = create(address);
    prompt(address, &parent, "msg_parent_prompt", "parent task");
    manual_compaction::assert_carried(&root, "msg_parent_prompt", "session.execution.succeeded.1");
    wait_session_idle(address, &parent);
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("read fork source: {error}"));
    let through: i64 = db
        .query_row(
            "SELECT MAX(seq) FROM event WHERE aggregate_family='session' AND aggregate_id=?1",
            [&parent],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("read fork cutoff: {error}"));
    drop(db);
    // Branch after the source advances: only the selected earlier prefix is copied.
    prompt(address, &parent, "msg_parent_later", "parent later task");
    manual_compaction::assert_carried(&root, "msg_parent_later", "session.execution.succeeded.1");
    wait_session_idle(address, &parent);
    let forked = json(
        address,
        "POST",
        &format!("/api/session/{parent}/fork"),
        &serde_json::json!({"throughSeq":through}).to_string(),
    );
    assert_eq!(forked["data"]["parentID"], parent);
    let forked = forked["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("selected fork named no Session"))
        .to_owned();
    let by_prompt = json(
        address,
        "POST",
        &format!("/api/session/{parent}/fork"),
        r#"{"beforeMessageID":"msg_parent_later"}"#,
    );
    let by_prompt = by_prompt["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("prompt-selected fork named no Session"));
    assert_eq!(
        texts(&turns(address, by_prompt)),
        vec![("user", "parent task"), ("assistant", "parent answer")]
    );
    prompt(address, &forked, "msg_fork_prompt", "fork task");
    manual_compaction::assert_carried(&root, "msg_fork_prompt", "session.execution.succeeded.1");
    wait_session_idle(address, &forked);

    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "FSE101 the forked prompt never reached the provider"
    );
    let fork_request = request_body(&provider, 2);
    assert!(
        fork_request.contains("parent task") && fork_request.contains("parent answer"),
        "FSE102/FSE103 the fork's provider request lost the parent conversation"
    );
    assert!(
        !fork_request.contains("parent later"),
        "the fork's provider request carries parent turns after the fork point"
    );
    let fork_turns = turns(address, &forked);
    assert_eq!(
        texts(&fork_turns),
        vec![
            ("user", "parent task"),
            ("assistant", "parent answer"),
            ("user", "fork task"),
            ("assistant", "fork answer"),
        ],
        "the fork's history is not its parent's conversation up to the fork point, then its own"
    );
    let parent_turns = turns(address, &parent);
    assert_eq!(
        texts(&parent_turns),
        vec![
            ("user", "parent task"),
            ("assistant", "parent answer"),
            ("user", "parent later task"),
            ("assistant", "parent later answer"),
        ],
        "the fork changed its parent's history"
    );
    // Copied messages are the fork's own: fresh identities, never the parent's.
    for copied in &fork_turns[..2] {
        assert!(
            parent_turns.iter().all(|turn| turn.1 != copied.1),
            "the fork reused the parent's message identity {}",
            copied.1
        );
    }

    let grandchild = fork(address, &forked);
    prompt(
        address,
        &grandchild,
        "msg_grandchild_prompt",
        "grandchild task",
    );
    manual_compaction::assert_carried(
        &root,
        "msg_grandchild_prompt",
        "session.execution.succeeded.1",
    );
    wait_session_idle(address, &grandchild);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let grandchild_request = request_body(&provider, 3);
    let requests: Vec<serde_json::Value> = (0..4)
        .map(|index| {
            serde_json::from_str(&request_body(&provider, index))
                .unwrap_or_else(|error| panic!("request JSON: {error}"))
        })
        .collect();
    let key = requests[0]["prompt_cache_key"]
        .as_str()
        .unwrap_or_else(|| panic!("parent request has no cache key"));
    assert!(!key.is_empty());
    for request in &requests[1..] {
        assert_eq!(
            request["prompt_cache_key"], key,
            "same-model resume/history fork rotated request affinity"
        );
    }
    let heads = locked(&provider.heads);
    for (head, session) in heads.iter().zip([&parent, &parent, &forked, &grandchild]) {
        super::cache_scope::assert_thread_headers(head, session);
        let session_header = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("session-id")
                    .then(|| value.trim())
            })
            .unwrap_or_else(|| panic!("missing session-id header"));
        assert_eq!(
            session_header, key,
            "history fork changed the provider's actual cache-affinity header"
        );
    }
    drop(heads);
    let original = requests[0]["input"]
        .as_array()
        .unwrap_or_else(|| panic!("request input is not an array"));
    for request in &requests[1..] {
        let input = request["input"]
            .as_array()
            .unwrap_or_else(|| panic!("request input is not an array"));
        assert!(
            input.starts_with(original),
            "fork/resume rewrote the retained request prefix"
        );
    }

    assert!(
        ["parent task", "parent answer", "fork task", "fork answer"]
            .iter()
            .all(|text| grandchild_request.contains(text)),
        "a fork of a fork lost the conversation it forked"
    );
    assert_eq!(
        texts(&turns(address, &grandchild)),
        vec![
            ("user", "parent task"),
            ("assistant", "parent answer"),
            ("user", "fork task"),
            ("assistant", "fork answer"),
            ("user", "grandchild task"),
            ("assistant", "grandchild answer"),
        ]
    );
    assert_eq!(
        texts(&turns(address, &forked)),
        texts(&fork_turns),
        "forking the fork changed it"
    );
    product.stop_after_witness(&root);
}

/// A failure in a fork settles as the truthful failure it is, in the fork only.
#[test]
fn a_failure_in_a_fork_settles_truthfully_and_leaves_the_parent_alone() {
    let _journey = process_journey_custody();
    let root = root("fork-execution-failure");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("parent answer"),
        Script::Unauthorized,
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let _approver = approval::Attachment::open(address, "*");

    let parent = create(address);
    prompt(address, &parent, "msg_parent_prompt", "parent task");
    let before = turns(address, &parent);
    let forked = fork(address, &parent);
    prompt(address, &forked, "msg_fork_prompt", "fork task");

    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "the failing forked prompt never reached the provider"
    );
    let history = json(
        address,
        "GET",
        &format!("/api/session/{forked}/message?order=asc&limit=200"),
        "",
    );
    let failed = history["data"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|row| row["metadata"]["executionFailure"] == true || !row["error"].is_null());
    assert!(
        failed,
        "the fork's provider failure is not in its durable history: {history}"
    );
    assert_eq!(
        turns(address, &parent),
        before,
        "a failure in the fork changed its parent"
    );
    product.stop_after_witness(&root);
}

fn pending(address: SocketAddr, session: &str) -> Vec<String> {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/pending"),
        "",
    )["data"]
        .as_array()
        .unwrap_or_else(|| panic!("{session} pending is not a list"))
        .iter()
        .map(|row| row.to_string())
        .collect()
}

/// A fork carries the parent's conversation, never its queue: a parent input
/// admitted but not yet executed is not copied, so the fork never runs the
/// parent's prompt; the parent still does.
#[test]
fn a_fork_never_inherits_the_parents_queued_input() {
    let _journey = process_journey_custody();
    let root = root("fork-execution-queue");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("parent answer"),
        Script::SuccessText("fork answer"),
        Script::SuccessText("parent queued answer"),
        Script::SuccessText("parent wake answer"),
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let _approver = approval::Attachment::open(address, "*");

    let parent = create(address);
    prompt(address, &parent, "msg_parent_prompt", "parent task");
    // Queued without a wake: admitted, not executed.
    let queued = json(
        address,
        "POST",
        &format!("/api/session/{parent}/prompt"),
        r#"{"id":"msg_parent_queued","text":"queued parent work","delivery":"queue","resume":false}"#,
    );
    assert_eq!(queued["data"]["id"], "msg_parent_queued");
    let parent_pending = pending(address, &parent);
    assert_eq!(parent_pending.len(), 1, "the parent's input was not queued");

    let forked = fork(address, &parent);
    assert!(
        pending(address, &forked).is_empty(),
        "FSE105 the fork inherited the parent's queued input"
    );
    assert_eq!(
        pending(address, &parent),
        parent_pending,
        "forking changed the parent's queue"
    );

    prompt(address, &forked, "msg_fork_prompt", "fork task");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(
        !request_body(&provider, 1).contains("queued parent work"),
        "the fork executed the parent's queued prompt"
    );
    assert!(
        texts(&turns(address, &forked))
            .iter()
            .all(|(_, text)| !text.contains("queued parent work")),
        "the parent's queued prompt entered the fork's history"
    );

    // The parent still executes its own queued input: the wake runs it, then
    // the wake prompt, one turn each.
    prompt(address, &parent, "msg_parent_wake", "parent wake");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    assert!(
        request_body(&provider, 2).contains("queued parent work"),
        "the parent lost its queued input"
    );
    assert!(pending(address, &parent).is_empty());
    product.stop_after_witness(&root);
}

/// Let the parent's tool call run, so the turn can settle.
fn approve_call_once(address: SocketAddr, session: &str, call: &str) {
    super::compaction_continuity::approve(address, session, call);
}

/// A tool-using parent can fork, and the child executes its own plan on the
/// inherited conversation: the parent's successful tool call carries its
/// identity and its durable terminal result into the child, so the child's
/// first turn is not refused the parent's work. The parent's live authority
/// is never carried. The child's rows survive a service restart, and its
/// session reopens.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_tool_using_parent_forks_and_the_child_executes_its_own_plan() {
    let _journey = process_journey_custody();
    let root = root("fork-execution-tools");
    prepare(&root, 120_000);
    fs::write(root.join("fork-notes.txt"), "parent notes payload\n")
        .unwrap_or_else(|error| panic!("fork fixture file failed: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_cp01_parent",
            path: "fork-notes.txt",
        },
        Script::SuccessText("parent answer"),
        Script::SuccessText("fork answer"),
        Script::SuccessText("fork reopened answer"),
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let _approver = approval::Attachment::open(address, "*");

    let parent = create(address);
    let body = r#"{"id":"msg_cp01_parent_prompt","text":"parent tool task","delivery":"queue","resume":true}"#;
    let admitted = json(
        address,
        "POST",
        &format!("/api/session/{parent}/prompt"),
        body,
    );
    assert_eq!(
        admitted["data"]["id"], "msg_cp01_parent_prompt",
        "prompt identity was not kept"
    );
    approve_call_once(address, &parent, "call_cp01_parent");
    wait_session_idle(address, &parent);
    let parent_turns = turns(address, &parent);
    assert_eq!(
        texts(&parent_turns),
        vec![
            ("user", "parent tool task"),
            (
                "assistant",
                numbered_read("parent notes payload\n").as_str()
            ),
            ("assistant", "parent answer"),
        ],
        "the parent's tool turn did not settle: {parent_turns:?}"
    );

    let forked = fork(address, &parent);
    prompt(address, &forked, "msg_cp01_fork_prompt", "fork task");
    wait_session_idle(address, &forked);

    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "CPK101 the fork's first turn never reached the provider: the parent's tool history was refused"
    );
    let fork_request = request_body(&provider, 2);
    let fork_input: serde_json::Value = serde_json::from_str(&fork_request)
        .unwrap_or_else(|error| panic!("fork request is not JSON: {error}"));
    assert!(
        fork_request.contains("parent tool task") && fork_request.contains("parent answer"),
        "CPK101 the fork's provider request lost the parent conversation"
    );
    assert!(
        fork_request.contains("parent notes payload"),
        "CPK101 the fork's provider request lost the parent's tool result"
    );
    let inherited_read = fork_input["input"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|item| item["type"] == "function_call" && item["name"] == "read");
    assert!(
        inherited_read,
        "CPK102 the inherited tool call lost its declared name: {fork_input}"
    );
    assert!(
        !fork_request.contains("fork answer"),
        "the fork answered its own prompt twice"
    );
    let fork_turns = turns(address, &forked);
    assert_eq!(
        texts(&fork_turns),
        vec![
            ("user", "parent tool task"),
            (
                "assistant",
                numbered_read("parent notes payload\n").as_str()
            ),
            ("assistant", "parent answer"),
            ("user", "fork task"),
            ("assistant", "fork answer"),
        ],
        "the fork's history is not the parent's tool conversation, then its own"
    );
    // Copied rows are the fork's own: fresh message identities, never the parent's.
    for (parent_row, fork_row) in parent_turns.iter().zip(fork_turns.iter()) {
        assert_ne!(
            parent_row.1, fork_row.1,
            "the fork reused the parent's message identity {}",
            parent_row.1
        );
    }
    assert_eq!(
        turns(address, &parent),
        parent_turns,
        "the fork changed its parent's history"
    );
    let child_pending = json(
        address,
        "GET",
        &format!("/api/session/{forked}/permission"),
        "",
    )["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        child_pending.is_empty(),
        "the fork inherited or re-raised the parent's permission: {child_pending:?}"
    );

    // The child's rows are durable: a restarted service reopens its session.
    product.stop_after_witness(&root);
    drop(product);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let _approver = approval::Attachment::open(address, "*");
    prompt(address, &forked, "msg_cp01_fork_reopen", "fork reopen task");
    wait_session_idle(address, &forked);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "the reopened fork's prompt never reached the provider"
    );
    let reopen_request = request_body(&provider, 3);
    assert!(
        reopen_request.contains("fork answer") && reopen_request.contains("parent notes payload"),
        "the reopened fork lost its settled history"
    );
    product.stop_after_witness(&root);
    drop(product);
    drop(provider);
    process_fixture::cleanup(&root);
}

#[test]
fn fresh_sessions_keep_independent_cache_affinity_and_actual_thread_headers() {
    let _journey = process_journey_custody();
    let root = root("fresh-cache-affinity");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("parent"),
        Script::SuccessText("fresh Session"),
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    let fresh = create(address);
    prompt(address, &parent, "msg_empty_parent", "parent task");
    prompt(address, &fresh, "msg_empty_child", "independent task");
    product.stop_after_witness(&root);
    let bodies = locked(&provider.bodies);
    let heads = locked(&provider.heads);
    assert_eq!(bodies.len(), 2);
    let mut keys = Vec::new();
    for ((body, head), session) in bodies.iter().zip(heads.iter()).zip([&parent, &fresh]) {
        let body: serde_json::Value =
            serde_json::from_str(body).unwrap_or_else(|error| panic!("request JSON: {error}"));
        let key = grapt_provider::PromptCacheKey::for_session(
            &session
                .parse()
                .unwrap_or_else(|error| panic!("session ID: {error}")),
        )
        .unwrap_or_else(|error| panic!("cache key: {error}"));
        assert_eq!(body["prompt_cache_key"], key.as_str());
        let headers: Vec<_> = head
            .lines()
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("session-id")
                    .then(|| value.trim())
            })
            .collect();
        assert_eq!(headers, [key.as_str()]);
        super::cache_scope::assert_thread_headers(head, session);
        keys.push(key);
    }
    assert_ne!(
        keys[0], keys[1],
        "fresh Sessions reused an unrelated cache bucket"
    );
    drop(heads);
    drop(bodies);
    process_fixture::cleanup(&root);
}

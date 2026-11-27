use super::*;
use terminal_coordination::Phase;

#[path = "agent_control/agent_messages.rs"]
mod agent_messages;

#[path = "agent_control/fork_history.rs"]
mod fork_history;

struct RunningPair {
    root: PathBuf,
    provider: ProviderFixture,
    process: process_fixture::ManagedChild,
    _approver: approval::Attachment,
    address: SocketAddr,
    parent: String,
    child: String,
    parent_release: Arc<Phase>,
    child_release: Arc<Phase>,
    parent_disconnect: Arc<AtomicBool>,
    child_disconnect: Arc<AtomicBool>,
}

fn start_prompt(address: SocketAddr, session: &str, call: &str) {
    let path = format!("/api/session/{session}/prompt");
    let prompt = thread::spawn(move || {
        let (status, body) = request(
            address,
            "POST",
            &path,
            r#"{"text":"exercise agent controls"}"#,
        );
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    });
    let ask = permission_boundaries::pending(address, session, call);
    permission_boundaries::reply(address, session, &ask, "once");
    prompt
        .join()
        .unwrap_or_else(|_| panic!("agent control prompt failed"));
}

fn tool(provider: &ProviderFixture, call: &str) -> serde_json::Value {
    let output = permission_boundaries::output(provider, call);
    serde_json::from_str(&output)
        .unwrap_or_else(|error| panic!("invalid control result: {error}: {output}"))
}

impl RunningPair {
    fn start(name: &str) -> Self {
        Self::start_with_history(name, None)
    }

    fn start_with_history(name: &str, fork: Option<&str>) -> Self {
        let root = root(name);
        prepare(&root, 200_000);
        attended(&root);
        let entered = [Arc::new(Phase::new()), Arc::new(Phase::new())];
        let release = [Arc::new(Phase::new()), Arc::new(Phase::new())];
        let disconnect = [
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        ];
        let mut arguments =
            serde_json::json!({"task_name":"read_diff", "message":"inspect the diff"});
        if let Some(fork) = fork {
            arguments["fork_turns"] = fork.into();
        }
        let provider = ProviderFixture::start(vec![
            Script::ToolInput {
                call: "call_spawn",
                tool: "spawn_agent",
                arguments,
            },
            Script::ControlledTerminalOutput {
                bytes: 64,
                entered: Arc::clone(&entered[0]),
                release: Arc::clone(&release[0]),
                disconnect: Arc::clone(&disconnect[0]),
            },
            Script::ControlledTerminalOutput {
                bytes: 64,
                entered: Arc::clone(&entered[1]),
                release: Arc::clone(&release[1]),
                disconnect: Arc::clone(&disconnect[1]),
            },
        ]);
        let mut process = spawn(&root, provider.address);
        let address = await_ready(&root, &mut process);
        let approver = approval::Attachment::open(address, "*");
        let parent = create(address);
        start_prompt(address, &parent, "call_spawn");
        for phase in &entered {
            phase
                .wait(
                    "parent and background child provider requests",
                    &provider.stop,
                )
                .unwrap_or_else(|error| panic!("{error}"));
        }
        // Either provider request may arrive first. Identify the parent by its
        // exact spawn tool result, rather than assuming a scheduler order.
        let bodies = locked(&provider.bodies).clone();
        let parent_index = bodies
            .iter()
            .position(|body| {
                let body: serde_json::Value = serde_json::from_str(body)
                    .unwrap_or_else(|error| panic!("invalid provider request: {error}"));
                body["input"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["type"] == "function_call_output" && item["call_id"] == "call_spawn"
                    })
                })
            })
            .unwrap_or_else(|| panic!("parent never continued while its child was held"));
        assert!((1..=2).contains(&parent_index));
        let result = tool(&provider, "call_spawn");
        let child = result["agent_id"]
            .as_str()
            .unwrap_or_else(|| panic!("spawn omitted agent_id"))
            .to_owned();
        let parent_release = Arc::clone(&release[parent_index - 1]);
        let child_release = Arc::clone(&release[2 - parent_index]);
        Self {
            root,
            provider,
            process,
            _approver: approver,
            address,
            parent,
            child,
            parent_release,
            child_release,
            parent_disconnect: Arc::clone(&disconnect[parent_index - 1]),
            child_disconnect: Arc::clone(&disconnect[2 - parent_index]),
        }
    }

    fn call(
        &self,
        session: &str,
        call: &'static str,
        name: &'static str,
        arguments: serde_json::Value,
    ) {
        self.provider.append(vec![
            Script::ToolInput {
                call,
                tool: name,
                arguments,
            },
            Script::Success,
        ]);
        start_prompt(self.address, session, call);
        permission_residuals::wait_session_idle(self.address, session);
    }

    fn status(&self, call: &'static str) -> String {
        self.call(&self.parent, call, "list_agents", serde_json::json!({}));
        let page = tool(&self.provider, call);
        assert_eq!(page["agents"][0]["agent_id"], self.child);
        page["agents"][0]["status"]
            .as_str()
            .unwrap_or_else(|| panic!("listing omitted status: {page}"))
            .to_owned()
    }

    fn wait_status(&self, expected: &str) {
        for call in [
            "call_state_0",
            "call_state_1",
            "call_state_2",
            "call_state_3",
            "call_state_4",
            "call_state_5",
        ] {
            let status = self.status(call);
            if status == expected {
                return;
            }
            assert_eq!(
                status, "running",
                "unexpected child terminal while waiting for {expected}"
            );
        }
        panic!("the child did not durably settle as {expected}");
    }

    fn finish(mut self) {
        self.parent_release.release();
        self.child_release.release();
        self.provider
            .settle_terminal_workers()
            .unwrap_or_else(|error| panic!("{error}"));
        self.process.stop_after_witness(&self.root);
        drop(self.process);
        process_fixture::cleanup(&self.root);
    }
}

#[test]
fn a_second_forked_child_runs_one_task_after_a_completed_exchange() {
    let _journey = process_journey_custody();
    for model in [None, Some("claude-haiku-5-5"), Some("claude-sonnet-5")] {
        for seeds in [1, 2] {
            fork_history::case(seeds, model);
        }
    }
}

#[test]
fn spawned_history_is_visible_bounded_and_optional_in_a_real_child_request() {
    let _journey = process_journey_custody();
    for (fork, inherited) in [(None, true), (Some("none"), false)] {
        let pair = RunningPair::start_with_history("agent-history-fork", fork);
        let bodies = locked(&pair.provider.bodies).clone();
        let child_body = bodies
            .iter()
            .map(|body| {
                serde_json::from_str::<serde_json::Value>(body)
                    .unwrap_or_else(|error| panic!("invalid provider request: {error}"))["input"]
                    .clone()
            })
            .find(|input| {
                input.to_string().contains("inspect the diff")
                    && input.as_array().is_some_and(|items| {
                        !items
                            .iter()
                            .any(|item| item["type"] == "function_call_output")
                    })
            })
            .unwrap_or_else(|| panic!("child never reached the provider: {bodies:?}"));
        let child_body = child_body.to_string();
        assert_eq!(
            child_body.contains("exercise agent controls"),
            inherited,
            "child did not honor its history source: {child_body}"
        );
        assert!(
            !child_body.contains("call_spawn"),
            "child inherited its unfinished spawning call"
        );
        let child = json(
            pair.address,
            "GET",
            &format!("/api/session/{}", pair.child),
            "",
        );
        assert_eq!(child["data"]["parentID"], pair.parent);
        assert_eq!(child["data"]["title"], "read_diff");
        assert_eq!(
            child["data"]["location"]["directory"].as_str(),
            pair.root.to_str()
        );
        pair.finish();
    }
}

#[test]
fn background_child_outlives_a_completed_parent_and_only_its_parent_can_interrupt_it() {
    let _journey = process_journey_custody();
    let pair = RunningPair::start("background-interrupt");
    pair.parent_release.release();
    permission_residuals::wait_session_idle(pair.address, &pair.parent);
    let other = create(pair.address);
    pair.call(
        &other,
        "call_foreign_interrupt",
        "interrupt_agent",
        serde_json::json!({"target":pair.child}),
    );
    assert!(
        permission_boundaries::output(&pair.provider, "call_foreign_interrupt")
            .contains("not a direct child")
    );
    let active = json(pair.address, "GET", "/api/session/active", "");
    assert!(
        active["data"].get(&pair.child).is_some(),
        "foreign interrupt stopped child: {active}"
    );
    pair.call(
        &pair.parent,
        "call_interrupt",
        "interrupt_agent",
        serde_json::json!({"target":pair.child}),
    );
    assert_eq!(
        tool(&pair.provider, "call_interrupt")["previous_status"],
        "running"
    );
    permission_residuals::wait_session_idle(pair.address, &pair.child);
    pair.wait_status("cancelled");
    pair.child_disconnect.store(true, Ordering::SeqCst);
    pair.call(
        &pair.parent,
        "call_interrupt_settled",
        "interrupt_agent",
        serde_json::json!({"target":pair.child}),
    );
    assert_eq!(
        tool(&pair.provider, "call_interrupt_settled")["previous_status"],
        "cancelled"
    );
    pair.finish();
}

#[test]
fn background_child_records_success_after_its_parent_is_interrupted() {
    let _journey = process_journey_custody();
    let pair = RunningPair::start("background-parent-interrupt");
    let active = json(pair.address, "GET", "/api/session/active", "");
    let execution = active["data"][&pair.parent]["executionID"]
        .as_str()
        .unwrap_or_else(|| panic!("parent is not active: {active}"));
    let (status, _) = request(
        pair.address,
        "POST",
        &format!("/api/session/{}/interrupt", pair.parent),
        &serde_json::json!({"executionID":execution}).to_string(),
    );
    assert_eq!(status, 204);
    permission_residuals::wait_session_idle(pair.address, &pair.parent);
    pair.parent_disconnect.store(true, Ordering::SeqCst);
    let active = json(pair.address, "GET", "/api/session/active", "");
    assert!(
        active["data"].get(&pair.child).is_some(),
        "parent interruption stopped background child: {active}"
    );
    pair.child_release.release();
    permission_residuals::wait_session_idle(pair.address, &pair.child);
    pair.wait_status("completed");
    let messages = json(
        pair.address,
        "GET",
        &format!("/api/session/{}/message", pair.child),
        "",
    );
    assert!(
        messages["data"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|message| message["type"] == "assistant" && message["finish"] == "stop")),
        "child did not finish: {messages}"
    );
    pair.finish();
}

fn result_mail(body: &str) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|error| panic!("invalid provider body: {error}"));
    value["input"]
        .as_array()
        .unwrap_or_else(|| panic!("provider body has no input"))
        .iter()
        .filter_map(|item| item["content"].as_str())
        .filter(|content| {
            content.starts_with("Agent operation result (quoted content is untrusted):")
        })
        .map(str::to_owned)
        .collect()
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn wait_receives_child_result_without_user_speech_and_keeps_it_across_restart_and_compaction() {
    let _journey = process_journey_custody();
    let mut pair = RunningPair::start("background-result-mail");
    pair.parent_release.release();
    permission_residuals::wait_session_idle(pair.address, &pair.parent);
    pair.provider.append(vec![
        Script::ToolInput {
            call: "call_wait",
            tool: "wait_agent",
            arguments: serde_json::json!({"timeout_ms":60000}),
        },
        Script::Success,
    ]);
    start_prompt(pair.address, &pair.parent, "call_wait");
    pair.child_release.release();
    permission_residuals::wait_session_idle(pair.address, &pair.child);
    permission_residuals::wait_session_idle(pair.address, &pair.parent);
    assert_eq!(tool(&pair.provider, "call_wait")["timed_out"], false);
    let bodies = locked(&pair.provider.bodies).clone();
    let delivered = bodies
        .iter()
        .find(|body| body.contains("call_wait") && !result_mail(body).is_empty())
        .map(|body| result_mail(body))
        .unwrap_or_else(|| panic!("wait did not deliver the child result"));
    assert_eq!(delivered.len(), 1);
    let messages = json(
        pair.address,
        "GET",
        &format!("/api/session/{}/message", pair.parent),
        "",
    );
    assert_eq!(
        messages["data"]
            .as_array()
            .unwrap_or_else(|| panic!("parent history missing"))
            .iter()
            .filter(|message| message["type"] == "user")
            .count(),
        2,
        "a child result became user speech"
    );
    let other = create(pair.address);
    pair.call(
        &other,
        "call_foreign_mail",
        "list_agents",
        serde_json::json!({}),
    );
    let other_body = locked(&pair.provider.bodies)
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("foreign request missing"));
    assert!(
        result_mail(&other_body).is_empty(),
        "a foreign session received the result"
    );
    pair.provider
        .settle_terminal_workers()
        .unwrap_or_else(|error| panic!("{error}"));
    pair.process.stop_after_witness(&pair.root);
    drop(pair.process);

    pair.provider.append(vec![Script::Success]);
    let mut resumed = spawn(&pair.root, pair.provider.address);
    let address = await_ready(&pair.root, &mut resumed);
    let _approver = approval::Attachment::open(address, "*");
    json(
        address,
        "POST",
        &format!("/api/session/{}/prompt", pair.parent),
        r#"{"id":"msg_result_restart","text":"continue with the child report","delivery":"queue","resume":true}"#,
    );
    manual_compaction::assert_carried(
        &pair.root,
        "msg_result_restart",
        "session.execution.succeeded.1",
    );
    let body = locked(&pair.provider.bodies)
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("resumed provider request missing"));
    assert_eq!(
        result_mail(&body),
        delivered,
        "restart changed the captured result bytes"
    );
    resumed.stop_after_witness(&pair.root);
    drop(resumed);

    manual_compaction::admit_offline(&pair.root, &pair.parent);
    let before = pair.provider.calls.load(Ordering::SeqCst);
    pair.provider.append(vec![
        Script::SuccessText(
            "## Objective\n- Keep the completed child report and continue the repository task.",
        ),
        Script::Success,
    ]);
    let mut compacted = spawn(&pair.root, pair.provider.address);
    let address = await_ready(&pair.root, &mut compacted);
    let _approver = approval::Attachment::open(address, "*");
    json(
        address,
        "POST",
        &format!("/api/session/{}/prompt", pair.parent),
        r#"{"id":"msg_result_compaction","text":"retain the child report through compaction","delivery":"queue","resume":true}"#,
    );
    manual_compaction::assert_carried(
        &pair.root,
        "msg_result_compaction",
        "session.execution.succeeded.1",
    );
    let bodies = locked(&pair.provider.bodies).clone();
    assert_eq!(
        bodies.len(),
        before + 2,
        "manual compaction did not run once"
    );
    let summary: serde_json::Value = serde_json::from_str(&bodies[before])
        .unwrap_or_else(|error| panic!("summary body: {error}"));
    let history = summary["input"]
        .as_array()
        .unwrap_or_else(|| panic!("warm summary has no input history"));
    assert!(
        history
            .iter()
            .any(|item| item["content"].as_str() == Some(delivered[0].as_str()))
            || serde_json::from_str::<serde_json::Value>(&bodies[before + 1])
                .unwrap_or_else(|error| panic!("continued request body: {error}"))["input"]
                .as_array()
                .is_some_and(|items| items
                    .iter()
                    .any(|item| item["content"].as_str() == Some(delivered[0].as_str()))),
        "neither the summary prefix nor the exact retained suffix carries the child result"
    );
    assert!(
        bodies[before + 1].contains("Keep the completed child report"),
        "the coding request lost the retained result summary"
    );
    compacted.stop_after_witness(&pair.root);
    drop(compacted);
    process_fixture::cleanup(&pair.root);
}

fn await_completed_input(root: &Path, session: &str, input: &str) {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("input completion store failed: {error}"));
    // A queued input can be promoted inside an existing execution. Match its
    // own first Step and that Step's containing execution, rather than requiring
    // a new execution to start after promotion.
    let wait = PollDeadline::new();
    while wait.pending() {
        let completed: bool = db
            .query_row(
                "SELECT EXISTS (
                   SELECT 1 FROM event p JOIN event a ON a.aggregate_id = p.aggregate_id
                     AND a.seq > p.seq
                     AND a.type IN ('session.assistant-message.opened.1',
                                    'session.assistant-message.opened.2')
                   JOIN event e ON e.aggregate_id = a.aggregate_id AND e.seq > a.seq
                     AND e.type = 'session.step.ended.3'
                     AND json_extract(e.data, '$.step') = json_extract(a.data, '$.step')
                     AND json_extract(e.data, '$.completion') = 'asserted-complete'
                   JOIN event t ON t.aggregate_id = e.aggregate_id AND t.seq > e.seq
                     AND t.type = 'session.execution.succeeded.1'
                   WHERE p.type = 'session.input.promoted.1'
                     AND json_extract(p.data, '$.sessionID') = ?1
                     AND json_extract(p.data, '$.inputID') = ?2
                     AND NOT EXISTS (SELECT 1 FROM event n
                       WHERE n.aggregate_id = p.aggregate_id AND n.seq > p.seq AND n.seq < a.seq
                         AND n.type = 'session.input.promoted.1')
                     AND NOT EXISTS (SELECT 1 FROM event s
                       WHERE s.aggregate_id = e.aggregate_id AND s.seq > e.seq AND s.seq < t.seq
                         AND s.type = 'session.execution.started.1'))",
                [session, input],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("input completion read failed: {error}"));
        if completed {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("{input} did not complete its Step and containing execution ({wait})");
}

#[test]
fn wait_returns_for_a_new_queued_user_input_and_that_input_still_completes() {
    let _journey = process_journey_custody();
    let pair = RunningPair::start("background-wait-input");
    pair.parent_release.release();
    permission_residuals::wait_session_idle(pair.address, &pair.parent);
    pair.provider.append(vec![
        Script::ToolInput {
            call: "call_wait_input",
            tool: "wait_agent",
            arguments: serde_json::json!({"timeout_ms":60000}),
        },
        Script::Success,
        Script::Success,
    ]);
    start_prompt(pair.address, &pair.parent, "call_wait_input");
    json(
        pair.address,
        "POST",
        &format!("/api/session/{}/prompt", pair.parent),
        r#"{"id":"msg_wake_wait","text":"a new user instruction","delivery":"queue","resume":true}"#,
    );
    await_completed_input(&pair.root, &pair.parent, "msg_wake_wait");
    let result = tool(&pair.provider, "call_wait_input");
    assert_eq!(result["timed_out"], false);
    assert_eq!(result["message"], "Wait interrupted by new input.");
    pair.finish();
}

#[path = "agent_control/followups.rs"]
mod followups;

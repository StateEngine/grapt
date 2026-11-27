//! Forked-child history, first input and compaction over the public product.
use super::*;

// Keep the same semantic assertions over the actual Responses and Anthropic
// wire bodies, including native System and legacy contextual User boundaries.
fn fork_request_items(request: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(items) = request["input"].as_array() {
        return items.clone();
    }
    let messages = request["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("provider messages absent: {request}"));
    let mut items = match &request["system"] {
        serde_json::Value::String(text) => {
            vec![serde_json::json!({"role":"system", "content":text})]
        }
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .map(|block| serde_json::json!({"role":"system", "content":block["text"]}))
            .collect::<Vec<_>>(),
        _ => panic!("provider System prefix absent: {request}"),
    };
    items.extend(messages.iter().flat_map(|message| {
        message["content"].as_array()
            .unwrap_or_else(|| panic!("provider message content absent: {message}"))
            .iter().map(move |block| match block["type"].as_str() {
                Some("text") => serde_json::json!({"role":message["role"], "content":block["text"]}),
                Some("tool_use") => serde_json::json!({"type":"function_call", "call_id":block["id"]}),
                Some("tool_result") => serde_json::json!({"type":"function_call_output", "call_id":block["tool_use_id"], "output":block["content"]}),
                _ => block.clone(),
            })
    }));
    items
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn case(seeds: usize, model: Option<&str>) {
    const FIRST_PARENT: &str = "Use spawn_agent exactly once with task_name fork_first, fork_turns none, and message: first-child-task. Then wait for it to finish.";
    const SECOND_PARENT: &str = "Use spawn_agent exactly once with task_name fork_second, fork_turns all, and message: second-child-task; do not spawn agents. Then wait for it to finish. Do not repeat the first child.";
    let root = root(&format!(
        "second-child-history-{}-{seeds}",
        model.unwrap_or("responses")
    ));
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    attended(&root);
    if let Some(model) = model {
        context_quality::configure_anthropic(&root, 200_000);
        for name in ["model-catalog.json", "anthropic-account-models.json"] {
            let path = root.join(name);
            let value = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("model fixture read: {error}"));
            fs::write(&path, value.replace("claude-live", model))
                .unwrap_or_else(|error| panic!("model fixture write: {error}"));
        }
    }
    fs::write(root.join("AGENTS.md"), "old-fork-rule")
        .unwrap_or_else(|error| panic!("parent instructions: {error}"));
    fn reply(body: &str) -> Script {
        let request: serde_json::Value =
            serde_json::from_str(body).unwrap_or_else(|error| panic!("provider request: {error}"));
        let items = fork_request_items(&request);
        let success = |text: &'static str| {
            if request.get("messages").is_some() {
                context_quality::anthropic_text(text, serde_json::json!({"input_tokens":20}))
            } else {
                Script::SuccessText(text)
            }
        };
        if body.contains("Output exactly the Markdown structure shown inside <template>") {
            return success("## Objective\n- Finish the child task and its addressed followups.");
        }
        if items.iter().any(|item| {
            item["role"] == "user"
                && item["content"].as_str().is_some_and(|text| {
                    text.starts_with("first-child-task") || text.starts_with("second-child-task")
                })
        }) {
            return success("child completed");
        }
        let prompts = items
            .iter()
            .filter(|item| {
                item["role"] == "user"
                    && matches!(item["content"].as_str(), Some(FIRST_PARENT | SECOND_PARENT))
            })
            .count();
        let (call, task, fork) = match prompts {
            1 => ("call_first_child", "first-child-task", "none"),
            2 => (
                "call_second_child",
                "second-child-task; do not spawn agents",
                "all",
            ),
            _ => return success("parent seeded"),
        };
        if items
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == call)
        {
            success("parent completed")
        } else {
            let arguments =
                serde_json::json!({"task_name":"inspect", "message":task, "fork_turns":fork});
            if request.get("messages").is_some() {
                Script::Raw(context_quality::anthropic_stream(
                    &[
                        serde_json::json!({"type":"tool_use", "id":call, "name":"spawn_agent", "input":arguments}),
                    ],
                    "tool_use",
                    serde_json::json!({"input_tokens":20}),
                ))
            } else {
                Script::ToolInput {
                    call,
                    tool: "spawn_agent",
                    arguments,
                }
            }
        }
    }
    let provider = ProviderFixture::start((0..16).map(|_| Script::ForRequest(reply)).collect());
    let mut product = spawn(&root, provider.address);
    let mut address = await_ready(&root, &mut product);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    if let Some(model) = model {
        let (status, body) = request(
            address,
            "POST",
            &format!("/api/session/{parent}/model"),
            &serde_json::json!({"model":{"providerID":"anthropic","id":model}}).to_string(),
        );
        assert_eq!(
            status,
            204,
            "model selection: {}",
            String::from_utf8_lossy(&body)
        );
    }
    for _ in 0..seeds {
        json(
            address,
            "POST",
            &format!("/api/session/{parent}/prompt"),
            &serde_json::json!({
                "text":"seed the parent history ".repeat(256),
                "delivery":"queue", "resume":true
            })
            .to_string(),
        );
        permission_residuals::wait_session_idle(address, &parent);
    }
    fs::write(root.join("AGENTS.md"), "current-fork-rule")
        .unwrap_or_else(|error| panic!("updated parent instructions: {error}"));
    let mut children = Vec::new();
    for call in ["call_first_child", "call_second_child"] {
        let path = format!("/api/session/{parent}/prompt");
        let text = if call == "call_first_child" {
            FIRST_PARENT
        } else {
            SECOND_PARENT
        };
        let prompt = thread::spawn(move || {
            let (status, body) = request(
                address,
                "POST",
                &path,
                &serde_json::json!({"text":text}).to_string(),
            );
            assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        });
        let ask = permission_boundaries::pending(address, &parent, call);
        permission_boundaries::reply(address, &parent, &ask, "once");
        prompt
            .join()
            .unwrap_or_else(|_| panic!("parent spawn prompt failed"));
        permission_residuals::wait_session_idle(address, &parent);
        let bodies = locked(&provider.bodies);
        let output = bodies
            .iter()
            .rev()
            .find_map(|body| {
                let request: serde_json::Value = serde_json::from_str(body)
                    .unwrap_or_else(|error| panic!("provider request: {error}"));
                fork_request_items(&request).into_iter().find_map(|item| {
                    (item["type"] == "function_call_output" && item["call_id"] == call)
                        .then(|| item["output"].clone())
                })
            })
            .unwrap_or_else(|| panic!("spawn result absent: {call}"));
        let result: serde_json::Value = serde_json::from_str(
            output
                .as_str()
                .unwrap_or_else(|| panic!("spawn result is not text: {output}")),
        )
        .unwrap_or_else(|error| panic!("spawn result JSON: {error}"));
        drop(bodies);
        let child = result["agent_id"]
            .as_str()
            .unwrap_or_else(|| panic!("child admission failed: {result}"));
        permission_residuals::wait_session_idle(address, child);
        // Followup input requires the initial operation's durable terminal too.
        followups::settled(&root, child, 1);
        children.push(child.to_owned());
    }
    let child_history = json(
        address,
        "GET",
        &format!("/api/session/{}/message?order=asc&limit=200", children[1]),
        "",
    );
    let bodies = locked(&provider.bodies).clone();
    if model.is_some() {
        let raw = bodies
            .iter()
            .map(|body| {
                serde_json::from_str::<serde_json::Value>(body)
                    .unwrap_or_else(|error| panic!("provider request: {error}"))
            })
            .find(|request| {
                fork_request_items(request).iter().any(|item| {
                    item["role"] == "user"
                        && item["content"] == "second-child-task; do not spawn agents"
                })
            })
            .unwrap_or_else(|| panic!("native child request absent: {bodies:?}"));
        let system = raw["system"]
            .as_array()
            .unwrap_or_else(|| panic!("native System blocks absent: {raw}"));
        let identity = system
            .last()
            .unwrap_or_else(|| panic!("native identity absent: {raw}"));
        let context = identity["text"]
            .as_str()
            .unwrap_or_else(|| panic!("native child context is not text: {raw}"));
        let expected_identity = format!("Session ID: {}", children[1]);
        assert_eq!(
            context.lines().next(),
            Some(expected_identity.as_str()),
            "the child's uncached System identity still names an ancestor"
        );
        assert!(
            context.contains(&format!("You are a subagent of Session {parent}."))
                && context.contains("You have already been spawned")
                && context.contains("Follow your own child task"),
            "the top-level System context does not distinguish the child from its parent: {raw}"
        );
        assert!(
            identity.get("cache_control").is_none(),
            "live identity became cached: {raw}"
        );
        assert!(
            !system[0].to_string().contains(&children[1]),
            "the live child identity rewrote the shared cached prefix: {raw}"
        );
        assert!(
            !system[0]
                .to_string()
                .contains("You are a subagent of Session"),
            "the per-child role entered the shared cached prefix: {raw}"
        );
    }
    let requests: Vec<Vec<serde_json::Value>> = bodies
        .iter()
        .map(|body| {
            let request: serde_json::Value = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("provider request: {error}"));
            fork_request_items(&request)
        })
        .collect();
    assert_eq!(
        requests
            .iter()
            .filter(|items| items.iter().any(|item| item["role"] == "user"
                && item["content"] == "second-child-task; do not spawn agents"))
            .count(),
        1,
        "the second task did not reach the provider exactly once: {child_history}"
    );
    let child = requests
        .iter()
        .find(|items| {
            items.iter().any(|item| {
                item["role"] == "user"
                    && item["content"] == "second-child-task; do not spawn agents"
            })
        })
        .unwrap_or_else(|| panic!("second child never reached the provider: {bodies:?}"));
    let items = child;
    assert_eq!(
        items
            .iter()
            .filter(|item| item["role"] == "user"
                && item["content"] == "second-child-task; do not spawn agents")
            .count(),
        1
    );
    let text = serde_json::to_string(child)
        .unwrap_or_else(|error| panic!("child request serialization: {error}"));
    assert!(text.contains("current-fork-rule"), "{text}");
    assert!(
        text.contains("call_first_child") && !text.contains("call_second_child"),
        "{text}"
    );
    let task_position = items
        .iter()
        .position(|item| {
            item["role"] == "user" && item["content"] == "second-child-task; do not spawn agents"
        })
        .unwrap_or_else(|| panic!("the child's own task is absent: {text}"));
    let parent_position = items.iter().position(|item| {
        item["role"] == "user" && item["content"] == format!(
            "Historical user message to the parent session, before this child was created:\n<parent_message>\n{SECOND_PARENT}\n</parent_message>"
        )
    }).unwrap_or_else(|| panic!("the pending parent spawn is not marked as historical context: {text}"));
    assert!(
        parent_position < task_position,
        "parent context followed the child's task: {text}"
    );
    assert!(
        !items
            .iter()
            .any(|item| item["role"] == "user" && item["content"] == SECOND_PARENT),
        "the unfinished parent spawn is still presented as a live User instruction: {text}"
    );
    let boundary = items
        .iter()
        .position(|item| {
            item["role"]
                == if model == Some("claude-sonnet-5") {
                    "user"
                } else {
                    "system"
                }
                && item["content"].as_str().is_some_and(|text| {
                    text.contains("newly spawned subagent")
                        && text.contains("historical parent message")
                        && text.contains("inherited parent history")
                        && text.contains("already been carried out")
                })
        })
        .unwrap_or_else(|| panic!("the child has no role boundary: {text}"));
    assert_eq!(
        boundary,
        task_position + 1,
        "the role boundary must follow the child's own task, after inherited history: {text}"
    );
    let page = json(
        address,
        "GET",
        &format!("/api/session/{}/message?order=asc&limit=200", children[1]),
        "",
    );
    let turns = page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("child history: {page}"));
    assert_eq!(
        turns
            .iter()
            .filter(|row| row["type"] == "user" && row["text"] == SECOND_PARENT)
            .count(),
        1,
        "the request-only parent label changed durable/public history: {page}"
    );
    assert_eq!(
        turns
            .iter()
            .filter(|row| row["type"] == "user"
                && row["text"] == "second-child-task; do not spawn agents")
            .count(),
        1
    );
    let initial_task = turns
        .iter()
        .find(|row| {
            row["type"] == "user" && row["text"] == "second-child-task; do not spawn agents"
        })
        .unwrap_or_else(|| panic!("original child task absent: {page}"))["id"]
        .clone();
    let own_inputs = [
        "second-child-task; do not spawn agents",
        "second-child-task later input",
        "second-child-task after compaction",
        "second-child-task replacement after rewind",
    ];
    for (index, text) in own_inputs.iter().enumerate().skip(1) {
        if index == 2 {
            product.stop_after_witness(&root);
            drop(product);
            manual_compaction::admit_offline(&root, &children[1]);
            product = spawn(&root, provider.address);
            address = await_ready(&root, &mut product);
        }
        if index == 3 {
            let preview = json(
                address,
                "POST",
                &format!("/api/session/{}/rewind/preview", children[1]),
                r#"{"prompts":3}"#,
            );
            assert_eq!(
                preview["data"]["messageID"], initial_task,
                "rewind did not select the original child task: {preview}"
            );
            json(
                address,
                "POST",
                &format!("/api/session/{}/rewind", children[1]),
                &serde_json::json!({"messageID":initial_task,"tip":preview["data"]["tip"]})
                    .to_string(),
            );
            let page = json(
                address,
                "GET",
                &format!("/api/session/{}/message?order=asc&limit=200", children[1]),
                "",
            );
            assert!(
                !page["data"]
                    .as_array()
                    .unwrap_or_else(|| panic!("rewound history absent: {page}"))
                    .iter()
                    .any(|row| row["id"] == initial_task),
                "rewind retained the original child task: {page}"
            );
        }
        let id = format!("msg_child_followup_{index}");
        json(
            address,
            "POST",
            &format!("/api/session/{}/prompt", children[1]),
            &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
        );
        permission_residuals::wait_session_idle(address, &children[1]);
        manual_compaction::assert_carried(&root, &id, "session.execution.succeeded.1");
        let bodies = locked(&provider.bodies).clone();
        let projected = bodies
            .iter()
            .rev()
            .find_map(|body| {
                let request: serde_json::Value = serde_json::from_str(body)
                    .unwrap_or_else(|error| panic!("followup provider request: {error}"));
                let items = fork_request_items(&request);
                items
                    .iter()
                    .any(|item| item["role"] == "user" && item["content"] == *text)
                    .then_some(items)
            })
            .unwrap_or_else(|| panic!("child followup never reached its provider: {text}"));
        for own in &own_inputs[..=index] {
            let labelled = format!(
                "Historical user message to the parent session, before this child was created:\n<parent_message>\n{own}\n</parent_message>"
            );
            assert!(
                !projected
                    .iter()
                    .any(|item| item["role"] == "user" && item["content"] == labelled),
                "the child's own task became parent history after input/compaction: {projected:?}"
            );
        }
        if index == 3 {
            let parent = format!("Historical user message to the parent session, before this child was created:\n<parent_message>\n{SECOND_PARENT}\n</parent_message>");
            assert!(
                projected
                    .iter()
                    .any(|item| item["role"] == "user" && item["content"] == parent),
                "rewind moved the parent-history boundary: {projected:?}"
            );
            assert!(
                !projected
                    .iter()
                    .any(|item| item["role"] == "user" && item["content"] == own_inputs[0]),
                "the original child task resurrected after rewind: {projected:?}"
            );
        }
        if index == 2 {
            let db = rusqlite::Connection::open_with_flags(
                root.join("database.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap_or_else(|error| panic!("compaction evidence read: {error}"));
            let compacted: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_id=?1 AND type='session.compaction.ended.2')",
                [&children[1]], |row| row.get(0))
                .unwrap_or_else(|error| panic!("child compaction evidence: {error}"));
            let failures: Vec<String> = db.prepare(
                "SELECT data FROM event WHERE aggregate_id=?1 AND type='session.compaction.failed.1' ORDER BY seq LIMIT 16",
            ).unwrap_or_else(|error| panic!("compaction failure query: {error}"))
                .query_map([&children[1]], |row| row.get(0))
                .unwrap_or_else(|error| panic!("compaction failure read: {error}"))
                .map(|row| row.unwrap_or_else(|error| panic!("compaction failure row: {error}")))
                .collect();
            assert!(
                compacted,
                "the post-compaction child projection was not exercised: {failures:?}"
            );
        }
    }
    product.stop_after_witness(&root);
}

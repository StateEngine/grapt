//! Model calls settle through the existing HTTP forms and provider continuation.
use super::*;

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn questions_answer_decline_disconnect_and_restart_without_replaying_a_wait(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for mode in ["headless", "answer", "cancel", "disconnect", "restart"] {
        let root = root(&format!("ask-user-{mode}"));
        prepare(&root, 200_000);
        let hooks = matches!(mode, "headless" | "answer" | "cancel")
            .then(|| question_hooks(&root, mode))
            .transpose()?;
        let questions = serde_json::json!({"questions":[
            {"id":"color","header":"Color","question":"Which color?","options":[{"label":"Blue","description":"Cool"},{"label":"Red","description":"Warm"}]},
            {"id":"features","header":"Features","question":"Which features?","multi_select":true,"options":[{"label":"Fast","description":"Speed"},{"label":"Small","description":"Size"}]}
        ]});
        let mut scripts = vec![Script::ToolInput {
            call: "call_question",
            tool: "request_user_input",
            arguments: questions.clone(),
        }];
        if mode == "restart" {
            scripts.push(Script::ToolInput {
                call: "call_after_loss",
                tool: "request_user_input",
                arguments: questions,
            });
        }
        scripts.push(Script::Success);
        let provider = ProviderFixture::start(scripts);
        let mut child = spawn(&root, provider.address);
        let mut address = await_ready(&root, &mut child);
        let session = create(address);
        let mut attachment =
            (mode != "headless").then(|| approval::Attachment::open(address, &session));
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"ask my preferences"}"#,
        );
        let forms = format!("/api/session/{session}/form");
        if mode == "headless" {
            permission_residuals::wait_session_idle(address, &session);
            assert_eq!(
                json(address, "GET", &forms, "")["data"],
                serde_json::json!([])
            );
        } else {
            let wait = PollDeadline::new();
            let row = loop {
                let rows = json(address, "GET", &forms, "");
                if let Some(row) = rows["data"].as_array().and_then(|rows| rows.first()) {
                    break row.clone();
                }
                assert!(wait.pending(), "question did not appear: {mode}");
                thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(row["metadata"]["kind"], "request_user_input");
            if let Some(log) = &hooks {
                let wait = PollDeadline::new();
                loop {
                    let events = match fs::read_to_string(log) {
                        Ok(events) => events,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                        Err(error) => return Err(error.into()),
                    };
                    if events.contains("Notification") {
                        break;
                    }
                    assert!(wait.pending(), "question observers did not finish");
                    thread::sleep(Duration::from_millis(5));
                }
            }
            let id = row["id"].as_str().ok_or("question id")?;
            let form = format!("{forms}/{id}");
            assert_eq!(
                provider.calls.load(Ordering::SeqCst),
                1,
                "model continued before an answer"
            );
            match mode {
                "answer" => {
                    let (status, body) = request(
                        address,
                        "POST",
                        &format!("{form}/reply"),
                        r#"{"answer":{"color":"Violet","features":["Fast","Small"]}}"#,
                    );
                    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
                }
                "cancel" => {
                    let (status, body) = request(address, "POST", &format!("{form}/cancel"), "{}");
                    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
                }
                "disconnect" => drop(attachment.take()),
                "restart" => {
                    child.stop_after_witness(&root);
                    drop(child);
                    drop(attachment.take());
                    retain_abandoned_questions(&root, &session, &row)?;
                    child = spawn(&root, provider.address);
                    address = await_ready(&root, &mut child);
                    let state = json(address, "GET", &format!("{form}/state"), "");
                    assert_eq!(state["data"]["status"], "cancelled", "{state}");
                    assert_eq!(
                        json(address, "GET", &forms, "")["data"],
                        serde_json::json!([])
                    );
                    let (status, body) = request(
                        address,
                        "POST",
                        &format!("{form}/reply"),
                        r#"{"answer":{"color":"late","features":["Fast"]}}"#,
                    );
                    assert_eq!(status, 409, "{}", String::from_utf8_lossy(&body));
                    permission_residuals::wait_session_idle(address, &session);
                    assert_eq!(
                        provider.calls.load(Ordering::SeqCst),
                        1,
                        "a late answer resurrected the lost waiter"
                    );
                    let next = create(address);
                    attachment = Some(approval::Attachment::open(address, &next));
                    json(
                        address,
                        "POST",
                        &format!("/api/session/{next}/prompt"),
                        r#"{"text":"ask again"}"#,
                    );
                    let wait = PollDeadline::new();
                    let next_form = loop {
                        let rows = json(address, "GET", &format!("/api/session/{next}/form"), "");
                        if let Some(id) = rows["data"]
                            .as_array()
                            .and_then(|r| r.first())
                            .and_then(|r| r["id"].as_str())
                        {
                            break id.to_owned();
                        }
                        assert!(
                            wait.pending(),
                            "retained abandoned questions exhausted live capacity"
                        );
                        thread::sleep(Duration::from_millis(5));
                    };
                    let (status, body) = request(
                        address,
                        "POST",
                        &format!("/api/session/{next}/form/{next_form}/reply"),
                        r#"{"answer":{"color":"Violet","features":["Fast","Small"]}}"#,
                    );
                    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
                    permission_residuals::wait_session_idle(address, &next);
                    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
                    assert_eq!(
                        json(
                            address,
                            "GET",
                            &format!("/api/session/{next}/form/{next_form}/state"),
                            ""
                        )["data"]["status"],
                        "answered"
                    );
                }
                _ => return Err("unknown question case".into()),
            }
        }
        if mode != "restart" {
            permission_residuals::wait_session_idle(address, &session);
            assert_eq!(
                json(address, "GET", &forms, "")["data"],
                serde_json::json!([])
            );
            let bodies = locked(&provider.bodies);
            let continuation: serde_json::Value =
                serde_json::from_str(bodies.get(1).ok_or("no continuation")?)?;
            let output = continuation["input"]
                .as_array()
                .ok_or("continuation input")?
                .iter()
                .find(|item| {
                    item["type"] == "function_call_output" && item["call_id"] == "call_question"
                })
                .and_then(|item| item["output"].as_str())
                .ok_or("question output")?;
            let output: serde_json::Value = serde_json::from_str(output)?;
            assert_eq!(
                output["status"],
                match mode {
                    "answer" => "answered",
                    "cancel" => "declined",
                    _ => "no_user_available",
                },
                "{output}"
            );
            if mode == "answer" {
                assert_eq!(
                    output["answers"]["color"]["answers"],
                    serde_json::json!(["Violet"])
                );
                assert_eq!(
                    output["answers"]["features"]["answers"],
                    serde_json::json!(["Fast", "Small"])
                );
            }
        }
        if let Some(log) = hooks {
            let events = match fs::read_to_string(log) {
                Ok(events) => events,
                Err(error)
                    if mode == "headless" && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    String::new()
                }
                Err(error) => return Err(error.into()),
            };
            assert_eq!(events.contains("PermissionRequest"), mode != "headless");
            assert_eq!(events.contains("WaitingForInput"), mode != "headless");
            if mode != "headless" {
                let messages = json(
                    address,
                    "GET",
                    &format!("/api/session/{session}/message"),
                    "",
                );
                assert!(messages.to_string().contains("\"sessionOnly\":true"));
            }
        }
        drop(attachment);
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
    Ok(())
}

fn retain_abandoned_questions(
    root: &Path,
    session: &str,
    row: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_form::{service::FormPresentation, FormService};
    use grapt_store::{AggregateId, EventStore};
    let mut store = EventStore::open(&root.join("database.sqlite3"), "ask-abandoned-fixture")?;
    let session: grapt_types::SessionId = session.parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    for index in 1..grapt_form::catalog::MAX_FORM_CATALOG_ROWS {
        let id = format!("frm_abandoned_{index}").parse()?;
        let event = format!("evt_abandoned_{index}").parse()?;
        FormService::create_presented_on(
            &mut store,
            &event,
            &id,
            &session,
            FormPresentation {
                title: "Abandoned question",
                fields: &row["fields"],
                metadata: Some(&row["metadata"]),
            },
            0,
        )?;
        if index % 2 == 0 {
            let event = format!("evt_suspend_abandoned_{index}").parse()?;
            FormService::suspend_on(
                &mut store,
                &event,
                &id,
                grapt_types::FormSuspensionReason::ProcessLoss,
                0,
            )?;
        }
    }
    assert_eq!(
        grapt_form::catalog::pending_forms(&store)?.len(),
        grapt_form::catalog::MAX_FORM_CATALOG_ROWS
    );
    store.release_held_writer(&aggregate);
    Ok(())
}

fn question_hooks(root: &Path, mode: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let script = root.join("question-hook.py");
    let log = root.join("question-hooks.log");
    fs::write(
        &script,
        r#"import json, sys
event = json.load(sys.stdin)['hook_event_name']
with open(sys.argv[1], 'a', encoding='utf-8') as out:
    out.write(event + '\n')
if event == 'WaitingForInput':
    sys.exit(7)
if event == 'PermissionRequest':
    print(json.dumps({'hookSpecificOutput': {'hookEventName': event, 'decision': {'behavior': sys.argv[2]}}}))
else:
    print('{}')
"#,
    )?;
    let python = lifecycle_hooks::python()?;
    let invoke = format!(
        "{}'{}' -I '{}' '{}' '{}'",
        if cfg!(windows) { "& " } else { "" },
        python,
        script.display(),
        log.display(),
        if mode == "answer" { "deny" } else { "allow" }
    );
    let profile = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&profile)?)?;
    for event in ["WaitingForInput", "PermissionRequest", "Notification"] {
        config["hooks"][event] = serde_json::json!([{
            "matcher":"request_user_input", "hooks":[{"type":"command", "command":invoke}]
        }]);
    }
    fs::write(profile, serde_json::to_vec(&config)?)?;
    Ok(log)
}

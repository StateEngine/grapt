//! Prompt options cross the real HTTP admission and execution path together.
use super::*;

const TURN_FINISHED: &str = "The requested tool sequence is complete.";

/// Each response opens one real read call, so the next response needs another
/// Step rather than placing all the calls inside one Step.
fn read_steps(count: usize) -> Vec<Script> {
    let mut scripts = (0..count)
        .map(|index| {
            let call = format!("call_turn_{index}");
            let item = format!("item_turn_{index}");
            let events = [
                serde_json::json!({"type":"response.output_item.added", "item":{
                    "type":"function_call", "id":item, "call_id":call, "name":"read"}}),
                serde_json::json!({"type":"response.function_call_arguments.delta",
                    "item_id":item, "delta":r#"{"path":"turn.txt"}"#}),
                serde_json::json!({"type":"response.function_call_arguments.done", "item_id":item}),
                serde_json::json!({"type":"response.completed", "response":{
                    "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
            ];
            Script::Raw(
                events
                    .iter()
                    .map(|event| format!("data: {event}\n\n"))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    scripts.push(Script::SuccessText(TURN_FINISHED));
    scripts
}

struct TurnFacts {
    promoted: i64,
    started: i64,
    steps: i64,
    tools: i64,
    succeeded: i64,
    failed: i64,
    interrupted: i64,
    failure: Option<String>,
    completed_tools: i64,
}

fn turn_facts(root: &Path, session: &str) -> rusqlite::Result<TurnFacts> {
    let database = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    database.query_row(
        "SELECT SUM(type='session.input.promoted.1'),
                SUM(type='session.execution.started.1'),
                SUM(type IN ('session.step.ended.2','session.step.ended.3')),
                SUM(type='session.tool.terminal.1'),
                SUM(type='session.execution.succeeded.1'),
                SUM(type='session.execution.failed.1'),
                SUM(type='session.execution.interrupted.1'),
                MAX(CASE WHEN type='session.execution.failed.1'
                    THEN json_extract(data,'$.error.type') END),
                SUM(type='session.tool.terminal.1'
                    AND json_extract(data,'$.outcome.kind')='completed')
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1",
        [session],
        |row| {
            Ok(TurnFacts {
                promoted: row.get(0)?,
                started: row.get(1)?,
                steps: row.get(2)?,
                tools: row.get(3)?,
                succeeded: row.get(4)?,
                failed: row.get(5)?,
                interrupted: row.get(6)?,
                failure: row.get(7)?,
                completed_tools: row.get(8)?,
            })
        },
    )
}

/// Exercise the shipped CLI's ordinary prompt path; no test mutates execution
/// counters or submits another prompt to replenish a running turn.
fn tool_turn(
    label: &str,
    steps: usize,
    cli_limit: Option<u32>,
    agent_limit: Option<u32>,
) -> Result<(cli_continuity::CliRun, usize, TurnFacts), Box<dyn std::error::Error>> {
    scripted_turn(label, read_steps(steps), cli_limit, agent_limit)
}

fn scripted_turn(
    label: &str,
    scripts: Vec<Script>,
    cli_limit: Option<u32>,
    agent_limit: Option<u32>,
) -> Result<(cli_continuity::CliRun, usize, TurnFacts), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 180_000);
    set_compaction(&root, serde_json::json!({"auto":false}));
    fs::write(root.join("turn.txt"), "bounded read fixture\n")?;
    if let Some(limit) = agent_limit {
        let path = root.join("grapt.jsonc");
        let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        config["agents"] = serde_json::json!({"build":{"steps":limit}});
        fs::write(path, serde_json::to_vec(&config)?)?;
    }
    let provider = ProviderFixture::start(scripts);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let created = json(address, "POST", "/api/session", r#"{"agent":"build"}"#);
    let session = created["data"]["id"].as_str().ok_or("session absent")?;
    let service = root.to_str().ok_or("service path is not UTF-8")?;
    let mut arguments = vec![
        "run",
        "--service",
        service,
        "--session",
        session,
        "--prompt",
        "Complete every requested read and then finish.",
        "--jsonl",
    ];
    let limit = cli_limit.map(|limit| limit.to_string());
    if let Some(limit) = &limit {
        arguments.extend(["--max-steps", limit]);
    }
    let run = cli_continuity::cli(&root, &arguments);
    permission_residuals::wait_session_idle(address, session);
    let facts = turn_facts(&root, session)?;
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    drop(provider);
    process_fixture::cleanup(&root);
    Ok((run, calls, facts))
}

#[test]
fn an_output_truncated_turn_keeps_calling_tools_until_the_model_finishes(
) -> Result<(), Box<dyn std::error::Error>> {
    let partial = [
        serde_json::json!({"type":"response.output_text.delta","delta":"Retained partial answer."}),
        serde_json::json!({"type":"response.output_text.done"}),
        serde_json::json!({"type":"response.incomplete","response":{
            "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
            "incomplete_details":{"reason":"max_output_tokens"}}}),
    ];
    let mut scripts = vec![Script::Raw(
        partial
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect(),
    )];
    scripts.extend(read_steps(3));
    let (run, calls, facts) = scripted_turn("uncapped-output-recovery", scripts, None, None)?;
    assert_eq!(
        calls, 5,
        "output recovery imposed a turn step cap: {:?}; {}",
        facts.failure, run.stderr
    );
    assert_eq!(run.code, Some(0), "{}\n{}", run.stdout, run.stderr);
    assert!(run.stdout.contains(TURN_FINISHED));
    assert_eq!((facts.promoted, facts.started), (1, 1));
    assert_eq!((facts.steps, facts.tools, facts.completed_tools), (5, 3, 3));
    assert_eq!(
        (facts.succeeded, facts.failed, facts.interrupted),
        (1, 0, 0)
    );
    Ok(())
}

#[test]
fn cli_max_steps_stops_after_exactly_the_requested_tool_steps(
) -> Result<(), Box<dyn std::error::Error>> {
    let (run, calls, facts) = tool_turn("explicit-cli-step-cap", 5, Some(3), None)?;
    assert_ne!(run.code, Some(0), "the explicit cap was ignored");
    assert_eq!((calls, facts.steps, facts.tools), (3, 3, 3));
    assert_eq!(facts.completed_tools, 3);
    assert_eq!((facts.promoted, facts.started), (1, 1));
    assert_eq!(
        (facts.succeeded, facts.failed, facts.interrupted),
        (0, 1, 0)
    );
    assert_eq!(facts.failure.as_deref(), Some("StepBudgetExhausted"));
    assert!(!run.stdout.contains(TURN_FINISHED));
    Ok(())
}

#[test]
fn configured_agent_steps_stop_after_exactly_the_requested_tool_steps(
) -> Result<(), Box<dyn std::error::Error>> {
    let (run, calls, facts) = tool_turn("explicit-agent-step-cap", 5, None, Some(2))?;
    assert_ne!(run.code, Some(0), "the agent cap was ignored");
    assert_eq!((calls, facts.steps, facts.tools), (2, 2, 2));
    assert_eq!(facts.completed_tools, 2);
    assert_eq!((facts.promoted, facts.started), (1, 1));
    assert_eq!(
        (facts.succeeded, facts.failed, facts.interrupted),
        (0, 1, 0)
    );
    assert_eq!(facts.failure.as_deref(), Some("StepBudgetExhausted"));
    assert!(!run.stdout.contains(TURN_FINISHED));
    Ok(())
}

#[test]
fn capped_product_prompt_preserves_closed_metadata_and_stops_execution(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("public-step-cap");
    prepare(&root, 150_000);
    set_compaction(&root, serde_json::json!({"auto":false}));
    let partial = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\n",
        serde_json::json!({"type":"response.output_text.delta","delta":"retained capped answer"}),
        serde_json::json!({"type":"response.output_text.done"}),
        serde_json::json!({"type":"response.incomplete","response":{
            "usage":{"input_tokens":31,"output_tokens":7,"total_tokens":38},
            "incomplete_details":{"reason":"max_output_tokens"}}})
    );
    let provider = ProviderFixture::start(vec![
        Script::Raw(partial),
        Script::SuccessText("a second Step must not open"),
    ]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let route = format!("/api/session/{session}/prompt");
    for metadata in [
        serde_json::json!({}),
        serde_json::json!({"maxSteps":1,"bypass":true}),
        serde_json::json!({"maxSteps":1,"origin":"human"}),
        serde_json::json!({"maxSteps":1,"userImages":{}}),
        serde_json::json!({"maxSteps":0}),
    ] {
        let (status, _) = request(
            address,
            "POST",
            &route,
            &serde_json::json!({"text":"refused", "metadata":metadata}).to_string(),
        );
        assert_eq!(status, 400);
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    json(
        address,
        "POST",
        &route,
        &serde_json::json!({
            "id":"msg_public_step_cap", "text":"continue until finished",
            "delivery":"queue", "resume":true,
            "metadata":{"maxSteps":1,"origin":"seat-startup"},
            "files":[{"name":"notes.txt","uri":"file:///work/notes.txt"}]
        })
        .to_string(),
    );
    manual_compaction::assert_carried(&root, "msg_public_step_cap", "session.execution.failed.1");
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let rows = history["data"].as_array().ok_or("history absent")?;
    assert!(rows.iter().any(|row| row["id"] == "msg_public_step_cap"));
    assert!(history.to_string().contains("retained capped answer"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let metadata: String = db.query_row(
        "SELECT metadata_json FROM session_message WHERE id='msg_public_step_cap'",
        [],
        |row| row.get(0),
    )?;
    let metadata: serde_json::Value = serde_json::from_str(&metadata)?;
    assert_eq!(metadata["maxSteps"], 1);
    assert_eq!(metadata["origin"], "seat-startup");
    let failure: String = db.query_row(
        "SELECT json_extract(data, '$.error.type') FROM event
         WHERE aggregate_id=?1 AND type='session.execution.failed.1' ORDER BY seq DESC LIMIT 1",
        [&session],
        |row| row.get(0),
    )?;
    assert_eq!(failure, "StepBudgetExhausted");
    drop(db);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

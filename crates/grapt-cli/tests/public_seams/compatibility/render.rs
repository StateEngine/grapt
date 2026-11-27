//! CLI-COMPATIBILITY-MATRIX-01 witnesses: text-mode rendering and history paging of a run.
//! Split out of `compatibility.rs` by HYGIENE-FOLLOWUPS-01; no test was renamed or dropped.

use super::*;

async fn render_text_run(reasoning: bool) -> TestResult<(String, String)> {
    let earlier = assistant("msg_old", "an earlier answer", "old thought", true);
    let user = r#"{"type":"user","id":"msg_user2","time":{"created":3.0},"text":"go"}"#.to_owned();
    let pending = assistant("msg_new", "", "", false);
    let done = assistant("msg_new", "  the answer  ", "a thought", true);
    let empty_permissions = r#"{"data":[],"hasMore":false}"#;
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
        // The event stream ends at once; its re-list shows the answer in progress.
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &page(&[pending, user.clone(), earlier.clone()]),
        ),
        Script::json(
            "GET",
            "/api/session/ses_a/permission",
            200,
            empty_permissions,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &page(&[done, user, earlier]),
        ),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    let receipt = Runner::new(client(&endpoint)?)
        .with_reasoning(reasoning)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await?;
    assert_eq!(receipt.exit, ExitClass::Success);
    assert_eq!(server.await??.len(), 6);
    let (stdout, stderr, _receipt) = output.into_parts();
    Ok((String::from_utf8(stdout)?, String::from_utf8(stderr)?))
}

/// stdout carries the answer alone, for command substitution; progress and
/// the step label go to stderr; earlier history is never re-rendered.
#[tokio::test]
async fn a_text_run_writes_only_its_own_answer_to_stdout() -> TestResult {
    let (stdout, stderr) = render_text_run(false).await?;
    assert_eq!(stdout, "the answer\n");
    assert_eq!(stderr, "prompt accepted for ses_a\n> build · test-model\n");
    Ok(())
}

#[tokio::test]
async fn thinking_adds_the_reasoning_before_the_answer() -> TestResult {
    let (stdout, _stderr) = render_text_run(true).await?;
    assert_eq!(stdout, "Thinking: a thought\nthe answer\n");
    Ok(())
}

/// A run whose Session records an execution failure is not a success: it
/// exits Remote with the durable failure text, as the TypeScript `run` exited 1.
#[tokio::test]
async fn a_run_whose_execution_failed_exits_remote_with_its_reason() -> TestResult {
    let user = r#"{"type":"user","id":"msg_user2","time":{"created":3.0},"text":"go"}"#;
    let failure = r#"{"type":"system","id":"msg_fail","metadata":{"executionFailure":true},"time":{"created":4.0},"text":"Execution failed: The execution stopped before completing."}"#;
    let failed_page = format!(r#"{{"data":[{failure},{user}],"cursor":{{}}}}"#);
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
        Script::json("GET", "/api/session/ses_a/message", 200, &failed_page),
        Script::json(
            "GET",
            "/api/session/ses_a/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json("GET", "/api/session/ses_a/message", 200, &failed_page),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let result = Runner::new(client(&endpoint)?)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await;
    match result {
        Err(RunError::Execution(message)) => {
            assert_eq!(
                message,
                "Execution failed: The execution stopped before completing."
            );
            assert_eq!(RunError::Execution(message).exit_class(), ExitClass::Remote);
        }
        other => return Err(format!("a failed execution was not refused: {other:?}").into()),
    }
    assert_eq!(server.await??.len(), 6);
    Ok(())
}

/// One `run` whose history holds only `page`: the prompt itself was never
/// promoted, so it is not a history row.
async fn run_against_queued_page(page: &str) -> TestResult<Result<(), RunError>> {
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
        Script::json("GET", "/api/session/ses_a/message", 200, page),
        Script::json(
            "GET",
            "/api/session/ses_a/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json("GET", "/api/session/ses_a/message", 200, page),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let result = Runner::new(client(&endpoint)?)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await
        .map(|_| ());
    server.await??;
    Ok(result)
}

/// A run whose prompt was admitted but not run yet (its turn was blocked
/// and the message stays queued, so history holds the notice and not the
/// prompt) is not a success: it exits Remote with the notice's words, found
/// by its own messageID only.
#[tokio::test]
async fn a_run_whose_prompt_was_not_run_yet_exits_remote_with_the_notice() -> TestResult {
    let text = "Not run yet: catalog configuration is not a regular file: /w/grapt.json. Your message is queued and runs on the next turn once this is fixed.";
    let notice = |queued: &str| {
        format!(
            r#"{{"data":[{{"type":"system","id":"msg_queued","metadata":{{"notRunYet":true,"messageID":"{queued}"}},"time":{{"created":4.0}},"text":"{text}"}}],"cursor":{{}}}}"#
        )
    };
    match run_against_queued_page(&notice("msg_user2")).await? {
        Err(RunError::NotRunYet(message)) => {
            assert_eq!(message, text);
            assert_eq!(RunError::NotRunYet(message).exit_class(), ExitClass::Remote);
        }
        other => return Err(format!("a queued prompt ran as a success: {other:?}").into()),
    }
    // Another prompt's notice is not this run's: the unseen prompt refuses.
    match run_against_queued_page(&notice("msg_other")).await? {
        Err(RunError::AdmissionUnseen { message, .. }) => assert_eq!(message, "msg_user2"),
        other => {
            return Err(format!("another prompt's notice satisfied the run: {other:?}").into())
        }
    }
    Ok(())
}

/// The queued prompt's notice may lie past the newest page: the run walks
/// back to it, as it walks back to a prompt.
#[tokio::test]
async fn a_queued_notice_past_the_newest_page_is_still_found() -> TestResult {
    let text =
        "Not run yet: x. Your message is queued and runs on the next turn once this is fixed.";
    let unrelated: Vec<String> = (1..=PAGE_ROWS)
        .rev()
        .map(|n| assistant(&format!("msg_other{n}"), "another answer", "", true))
        .collect();
    let newest = format!(
        r#"{{"data":[{}],"cursor":{{"next":"older"}}}}"#,
        unrelated.join(",")
    );
    let older = format!(
        r#"{{"data":[{{"type":"system","id":"msg_queued","metadata":{{"notRunYet":true,"messageID":"msg_user2"}},"time":{{"created":4.0}},"text":"{text}"}}],"cursor":{{}}}}"#
    );
    // A machine-mode relist before terminal reads only the newest page; the
    // terminal relist walks back through the cursor to the notice.
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
        Script::json("GET", "/api/session/ses_a/message", 200, &newest),
        Script::json(
            "GET",
            "/api/session/ses_a/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json("GET", "/api/session/ses_a/message", 200, &newest),
        Script::json(
            "GET",
            "/api/session/ses_a/message?cursor=older",
            200,
            &older,
        ),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let result = Runner::new(client(&endpoint)?)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await;
    match result {
        Err(RunError::NotRunYet(message)) => assert_eq!(message, text),
        other => {
            return Err(format!("the queued notice past page one was missed: {other:?}").into())
        }
    }
    assert_eq!(server.await??.len(), 7);
    Ok(())
}

/// The acceptance itself may say the prompt is queued
/// (its notice could not be written); the run shows the reason at once.
#[tokio::test]
async fn a_queued_acceptance_exits_with_its_reason_without_history() -> TestResult {
    let accepted = PROMPT_ACCEPTED.replace(
        r#""delivery":"queue"}}"#,
        r#""delivery":"queue"},"queued":{"reason":"catalog configuration is not a regular file"}}"#,
    );
    assert_ne!(
        accepted, PROMPT_ACCEPTED,
        "the fixture did not gain its queued reason"
    );
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, &accepted),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let result = Runner::new(client(&endpoint)?)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await;
    match result {
        Err(RunError::NotRunYet(message)) => assert_eq!(
            message,
            "Not run yet: catalog configuration is not a regular file. Your message is queued and runs on the next turn once this is fixed."
        ),
        other => return Err(format!("a queued acceptance ran on: {other:?}").into()),
    }
    assert_eq!(server.await??.len(), 2, "a queued acceptance read history");
    Ok(())
}

/// Review P-H1 regression: once the queued prompt ran, its own rows are
/// the run's, not its older notice.
#[tokio::test]
async fn a_prompt_that_ran_after_its_notice_is_a_success() -> TestResult {
    let user = r#"{"type":"user","id":"msg_user2","time":{"created":5.0},"text":"go"}"#;
    let notice = r#"{"type":"system","id":"msg_queued","metadata":{"notRunYet":true,"messageID":"msg_user2"},"time":{"created":4.0},"text":"Not run yet: x."}"#;
    let page = format!(
        r#"{{"data":[{},{user},{notice}],"cursor":{{}}}}"#,
        assistant("msg_answer", "the answer", "", true)
    );
    match run_against_queued_page(&page).await? {
        Ok(()) => Ok(()),
        other => Err(format!("a prompt that ran was not a success: {other:?}").into()),
    }
}

/// Without `--auto`, a run reports a pending permission and never answers
/// it: the decision stays with the Session's preset and its user.
#[tokio::test]
async fn a_text_run_reports_pending_permissions_and_never_answers_them() -> TestResult {
    let user = r#"{"type":"user","id":"msg_user2","time":{"created":3.0},"text":"go"}"#.to_owned();
    let done = assistant("msg_new", "done", "", true);
    let pending = r#"{"data":[{"id":"per_1","sessionID":"ses_a","action":"write","resources":[],"status":"pending"}],"hasMore":false}"#;
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &page(std::slice::from_ref(&user)),
        ),
        Script::json("GET", "/api/session/ses_a/permission", 200, pending),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &page(&[done, user]),
        ),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    Runner::new(client(&endpoint)?)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await?;
    let requests = server.await??;
    assert!(
        !requests.iter().any(|request| request.contains("/reply")),
        "the run answered a permission: {requests:?}"
    );
    let (stdout, stderr, _receipt) = output.into_parts();
    assert_eq!(String::from_utf8(stdout)?, "done\n");
    assert!(
        String::from_utf8(stderr)?.contains("pending permissions for ses_a\n"),
        "the pending permission was not reported"
    );
    Ok(())
}

/// `--jsonl` prints the permission row the server sent, so the tool name the
/// server recorded on the row must reach the machine line verbatim: it is the
/// one place a consumer can tell the tool apart from the row's fs verbs.
#[tokio::test]
async fn a_jsonl_run_carries_the_server_tool_name_on_permission_rows() -> TestResult {
    let user = r#"{"type":"user","id":"msg_user2","time":{"created":3.0},"text":"go"}"#.to_owned();
    let done = assistant("msg_new", "done", "", true);
    let pending = r#"{"data":[
        {"id":"per_1","sessionID":"ses_a","action":"patch","resources":["/r/a"],"status":"pending",
         "metadata":{"tool":"patch","policy":{"ownerFloor":false,"removesFiles":false},
          "consequences":[{"action":"fs.modify","resource":"/r/a"}],"request":null}}
    ],"hasMore":false}"#;
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &page(std::slice::from_ref(&user)),
        ),
        Script::json("GET", "/api/session/ses_a/permission", 200, pending),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &page(&[done, user]),
        ),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    Runner::new(client(&endpoint)?)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await?;
    let _requests = server.await??;
    let (stdout, _stderr, _receipt) = output.into_parts();
    let stdout = String::from_utf8(stdout)?;
    let row = stdout
        .lines()
        .find(|line| line.contains("per_1"))
        .ok_or_else(|| format!("the jsonl run printed no permission row: {stdout:?}"))?;
    let row: serde_json::Value = serde_json::from_str(row)?;
    let row = row
        .get("data")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.iter().find(|row| row["id"] == "per_1"))
        .ok_or_else(|| format!("no per_1 row in the machine line: {row:?}"))?;
    assert_eq!(row["action"], "patch");
    assert_eq!(
        row["metadata"]["tool"], "patch",
        "the jsonl permission row lost the server's tool name"
    );
    assert_eq!(
        row["metadata"]["consequences"][0]["action"], "fs.modify",
        "the row's fs verbs must stay distinct from the tool name"
    );
    Ok(())
}

/// `--auto` answers each pending request once, except one that would remove
/// a file from its path, which it refuses in words; a chosen preset is set on
/// the Session before the prompt.
#[tokio::test]
async fn an_auto_run_approves_once_and_refuses_removal() -> TestResult {
    let user = r#"{"type":"user","id":"msg_user2","time":{"created":3.0},"text":"go"}"#.to_owned();
    let done = assistant("msg_new", "done", "", true);
    let pending = r#"{"data":[
        {"id":"per_1","sessionID":"ses_a","action":"fs.modify","resources":["/r/a"],"status":"pending","metadata":{"policy":{"ownerFloor":false,"removesFiles":false}}},
        {"id":"per_2","sessionID":"ses_a","action":"fs.delete","resources":["/r/b"],"status":"pending","metadata":{"policy":{"ownerFloor":false,"removesFiles":true}}}
    ],"hasMore":false}"#;
    let scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::status("POST", "/api/session/ses_a/preset", 204),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &page(std::slice::from_ref(&user)),
        ),
        Script::json("GET", "/api/session/ses_a/permission", 200, pending),
        Script::status("POST", "/api/session/ses_a/permission/per_1/reply", 204)
            .expect_json(serde_json::json!({"reply":"once"})),
        Script::status("POST", "/api/session/ses_a/permission/per_2/reply", 204)
            .expect_json(serde_json::json!({"reply":"reject","message":"refused fs.delete (/r/b): automatic approval will not remove files or approve an unclassified request. Approve it interactively."})),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &page(&[done, user]),
        ),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    Runner::new(client(&endpoint)?)
        .with_preset(Some("author"))
        .with_auto(grapt_cli::AutoReply::Auto)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await?;
    let requests = server.await??;
    let at = |line: &str| requests.iter().position(|request| request == line);
    assert!(
        at("POST /api/session/ses_a/preset") < at("POST /api/session/ses_a/prompt"),
        "the preset was not set before the prompt: {requests:?}"
    );
    for request in ["per_1", "per_2"] {
        assert!(
            at(&format!(
                "POST /api/session/ses_a/permission/{request}/reply"
            ))
            .is_some(),
            "{request} was not answered: {requests:?}"
        );
    }
    let (_stdout, stderr, _receipt) = output.into_parts();
    assert!(
        String::from_utf8(stderr)?
            .contains("refused fs.delete (/r/b): automatic approval will not remove files"),
        "the refusal was not explained"
    );
    Ok(())
}

/// Steps after the prompt in a turn that outgrows the product's default
/// 50-row history page.
const LONG_STEPS: usize = 55;
const PAGE_ROWS: usize = 50;
const FAILURE: &str = r#"{"type":"system","id":"msg_fail","metadata":{"executionFailure":true},"time":{"created":99.0},"text":"Execution failed: The execution stopped before completing."}"#;

/// A text run whose turn pushed its admitted prompt off the first history
/// page: the newest page carries a cursor to the older one. `anchored` false
/// is a history that never shows the prompt at all.
async fn run_past_first_page(
    failed: bool,
    anchored: bool,
) -> TestResult<(
    Result<grapt_cli::RunReceipt, RunError>,
    String,
    Option<usize>,
)> {
    let mut rows: Vec<String> = Vec::new();
    if failed {
        rows.push(FAILURE.to_owned());
    }
    for step in (1..=LONG_STEPS).rev() {
        rows.push(assistant(
            &format!("msg_step{step}"),
            &format!("step {step}"),
            "",
            true,
        ));
    }
    if anchored {
        rows.push(
            r#"{"type":"user","id":"msg_user2","time":{"created":3.0},"text":"go"}"#.to_owned(),
        );
        rows.push(assistant("msg_old", "an earlier answer", "", true));
    }
    let (newest, older) = rows.split_at(PAGE_ROWS);
    let newest = format!(
        r#"{{"data":[{}],"cursor":{{"next":"older"}}}}"#,
        newest.join(",")
    );
    let older = page(older);
    let mut scripts = vec![
        Script::sse("GET", "/api/event", ""),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
    ];
    // The stream ends at once: a relist, the activity read, and the terminal relist.
    for relist in 0..2 {
        scripts.push(Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            &newest,
        ));
        scripts.push(Script::json(
            "GET",
            "/api/session/ses_a/message?cursor=older",
            200,
            &older,
        ));
        if relist == 0 {
            scripts.push(Script::json(
                "GET",
                "/api/session/ses_a/permission",
                200,
                r#"{"data":[],"hasMore":false}"#,
            ));
            scripts.push(Script::json(
                "GET",
                "/api/session/active",
                200,
                r#"{"data":{}}"#,
            ));
        }
    }
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    let result = Runner::new(client(&endpoint)?)
        .run_selected("ses_a".parse()?, 1, "go".to_owned(), &mut output)
        .await;
    // A run that stops reading early leaves scripts unserved: its server
    // never finishes, and the caller judges the run's result first.
    let served = match tokio::time::timeout(std::time::Duration::from_secs(5), server).await {
        Ok(served) => Some(served??.len()),
        Err(_unserved) => None,
    };
    let (stdout, _stderr, _receipt) = output.into_parts();
    Ok((result, String::from_utf8(stdout)?, served))
}

/// A turn longer than one history page is read back to its own prompt: every
/// step is rendered, and the earlier answer before the prompt is not.
#[tokio::test]
async fn a_text_run_reads_back_past_the_first_page_to_its_own_prompt() -> TestResult {
    let (result, stdout, served) = run_past_first_page(false, true).await?;
    let receipt = result.map_err(|error| format!("a long text run was refused: {error}"))?;
    assert_eq!(receipt.exit, ExitClass::Success);
    assert_eq!(served, Some(8), "both pages were read at each relist");
    let answer: String = (1..=LONG_STEPS)
        .map(|step| format!("step {step}\n"))
        .collect();
    assert_eq!(stdout, answer, "CCM112 steps past the first page were lost");
    Ok(())
}

/// The failure of a turn longer than one history page is still seen.
#[tokio::test]
async fn a_long_run_whose_execution_failed_past_the_first_page_exits_remote() -> TestResult {
    let (result, _stdout, served) = run_past_first_page(true, true).await?;
    match result {
        Err(RunError::Execution(message)) => {
            assert_eq!(
                message,
                "Execution failed: The execution stopped before completing."
            );
        }
        other => return Err(format!("a failed long run was not refused: {other:?}").into()),
    }
    assert_eq!(served, Some(8), "both pages were read at each relist");
    Ok(())
}

/// A run never reports Success without having seen its own admitted prompt.
#[tokio::test]
async fn a_run_whose_history_never_shows_its_prompt_is_refused() -> TestResult {
    let (result, stdout, served) = run_past_first_page(false, false).await?;
    match result {
        Err(error @ RunError::AdmissionUnseen { .. }) => {
            assert_eq!(error.exit_class(), ExitClass::Remote);
            assert!(error.to_string().contains("msg_user2"), "{error}");
        }
        other => {
            return Err(
                format!("CCM113 a run without its prompt was not refused: {other:?}").into(),
            )
        }
    }
    assert_eq!(
        stdout, "",
        "nothing unanchored is rendered as this run's answer"
    );
    assert_eq!(
        served,
        Some(8),
        "the whole history was read before refusing"
    );
    Ok(())
}

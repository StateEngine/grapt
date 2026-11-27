//! One PTY remains owned by the live service across compaction and CLI reopen.
//! Launch and input counters are files written by the actual child, not the model.
use super::*;
#[path = "terminal_compaction/guide.rs"]
mod guide;
#[cfg(windows)]
#[path = "terminal_compaction/powershell_pipe.rs"]
mod powershell_pipe;
#[path = "terminal_compaction/provider_watchdog.rs"]
mod provider_watchdog;

fn run_prompt(root: &Path, address: SocketAddr, session: &str, id: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":id,"delivery":"queue","resume":true}).to_string(),
    );
    manual_compaction::assert_carried(root, id, "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, session);
}

fn tool(
    root: &Path,
    provider: &ProviderFixture,
    address: SocketAddr,
    session: &str,
    call: &'static str,
    name: &'static str,
    arguments: serde_json::Value,
) -> Result<String, Box<dyn std::error::Error>> {
    let before = provider.calls.load(Ordering::SeqCst);
    provider.append(vec![
        Script::ToolInput {
            call,
            tool: name,
            arguments,
        },
        Script::Success,
    ]);
    let id = format!("msg_{call}");
    run_prompt(root, address, session, &id);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        before + 2,
        "unexpected tool replay for {call}"
    );
    let bodies = locked(&provider.bodies);
    let body: serde_json::Value =
        serde_json::from_str(bodies.last().ok_or("provider request absent")?)?;
    let outputs = body["input"]
        .as_array()
        .ok_or("provider input absent")?
        .iter()
        .filter(|item| item["type"] == "function_call_output" && item["call_id"] == call)
        .collect::<Vec<_>>();
    assert_eq!(
        outputs.len(),
        1,
        "tool result delivered more than once for {call}"
    );
    Ok(outputs[0]["output"]
        .as_str()
        .ok_or("tool result text absent")?
        .to_owned())
}

fn stdin(operation: &str, chars: &str, cursor: Option<u64>) -> serde_json::Value {
    let mut value = serde_json::json!({"session_id":operation,"chars":chars,"yield_time_ms":5000,"max_output_tokens":32});
    if let Some(cursor) = cursor {
        value["cursor"] = cursor.into();
    }
    value
}

fn page(
    raw: &str,
    operation: &str,
) -> Result<(serde_json::Value, String, u64), Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    assert_eq!(value["session_id"], operation);
    assert_eq!(value["omitted_bytes"], 0);
    let output = value["output"]
        .as_str()
        .ok_or("terminal output absent")?
        .replace('\r', "");
    assert!(
        output.len() <= 128,
        "bounded output page exceeded its requested byte cap"
    );
    let cursor = value["output_cursor"]
        .as_u64()
        .ok_or("terminal cursor absent")?;
    Ok((value, output, cursor))
}

fn await_file(path: &Path, expected: &str) -> Result<(), Box<dyn std::error::Error>> {
    let wait = PollDeadline::new();
    while wait.pending() {
        match fs::read_to_string(path) {
            Ok(value) if value == expected => return Ok(()),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(format!("child readiness file {} was not observed", path.display()).into())
}

fn exec_arguments() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    #[cfg(unix)]
    let command = "test -t 0 && test -t 1 || exit 17; /bin/stty -echo; printf 'launch\\n' >> launches.txt; trap 'printf interrupted >> stopped.txt; printf \"STOPPED\\n\"; exit 0' INT; printf ready > ready.flag; printf 'PTY-READY\\n'; IFS= read -r word; printf '%s\\n' \"$word\" >> markers.txt; printf 'ACK:%s\\n' \"$word\"; while :; do IFS= read -r extra || :; done";
    #[cfg(windows)]
    let command = "[IO.File]::AppendAllText('launches.txt', \"launch`n\"); [IO.File]::WriteAllText('ready.flag','ready'); [Console]::WriteLine('PTY-READY'); $word=[Console]::ReadLine(); [IO.File]::AppendAllText('markers.txt',$word+\"`n\"); [Console]::WriteLine('ACK:'+$word); while($true){$null=[Console]::ReadLine()}";
    let value = serde_json::json!({"cmd":command,"login":false,"tty":true,"yield_time_ms":250,"timeout_ms":120_000,"max_output_tokens":32});
    #[cfg(windows)]
    let value = {
        let mut value = value;
        let system = std::env::var_os("SystemRoot").ok_or("Windows SystemRoot is absent")?;
        value["shell"] = PathBuf::from(system)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe")
            .to_str()
            .ok_or("PowerShell path is not UTF-8")?
            .into();
        value
    };
    Ok(value)
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn pty_handle_survives_compaction_and_client_reopen_without_relaunch_or_input_replay(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("terminal-compact-reopen");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 240_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    let started = tool(
        &root,
        &provider,
        address,
        &session,
        "call_tc_start",
        "exec_command",
        exec_arguments()?,
    )?;
    let started: serde_json::Value = serde_json::from_str(&started)?;
    let operation = started["session_id"]
        .as_str()
        .ok_or("running process handle absent")?
        .to_owned();
    assert_eq!(started["status"], "running");
    assert_eq!(started["done"], false);
    await_file(&root.join("ready.flag"), "ready")?;
    assert_eq!(fs::read_to_string(root.join("launches.txt"))?, "launch\n");

    provider.append(vec![
        Script::SuccessText(
            "## Objective\n- poll the existing PTY handle, send one marker, then interrupt it",
        ),
        Script::Success,
    ]);
    assert!((200..300).contains(
        &request(
            address,
            "POST",
            &format!("/api/session/{session}/compact"),
            "{}"
        )
        .0
    ));
    run_prompt(&root, address, &session, "msg_tc_compact");
    let history_path = format!("/api/session/{session}/message?order=asc&limit=200");
    let history = json(address, "GET", &history_path, "");
    assert_eq!(
        history["data"]
            .as_array()
            .ok_or("history absent")?
            .iter()
            .filter(|row| row["type"] == "compaction")
            .count(),
        1
    );
    let before_reopen = provider.calls.load(Ordering::SeqCst);
    for _ in 0..2 {
        let attached = cli_continuity::cli(
            &root,
            &[
                "attach",
                "--service",
                root.to_str().ok_or("service path is not UTF-8")?,
                "--resume",
                "--jsonl",
            ],
        );
        assert_eq!(
            attached.code,
            Some(0),
            "client reopen failed: {}",
            attached.stderr
        );
        assert_eq!(attached.receipt()["session"], session);
        assert_eq!(json(address, "GET", &history_path, ""), history);
    }
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        before_reopen,
        "reopen called the provider"
    );
    assert_eq!(
        fs::read_to_string(root.join("launches.txt"))?,
        "launch\n",
        "reopen relaunched the process"
    );

    let foreign = create(address);
    for (owner, call, handle) in [
        (&foreign, "call_tc_foreign", operation.as_str()),
        (&session, "call_tc_invalid", "op_missing_terminal"),
    ] {
        let refused = tool(
            &root,
            &provider,
            address,
            owner,
            call,
            "write_stdin",
            stdin(handle, "DO-NOT-DELIVER\n", None),
        )?;
        assert!(
            refused.contains("absent, expired, or belongs to another Session"),
            "non-actionable refusal: {refused}"
        );
    }
    assert!(
        !root.join("markers.txt").exists(),
        "foreign input reached the child"
    );
    let polled = tool(
        &root,
        &provider,
        address,
        &session,
        "call_tc_poll",
        "write_stdin",
        stdin(&operation, "", Some(0)),
    )?;
    let (_, mut observed, mut cursor) = page(&polled, &operation)?;
    // ConPTY includes terminal setup/title sequences in its raw output. Keep
    // those bytes and their cursors; find the child marker across bounded pages.
    for call in ["call_tc_ready_1", "call_tc_ready_2", "call_tc_ready_3"] {
        if observed.contains("PTY-READY\n") {
            break;
        }
        let raw = tool(
            &root,
            &provider,
            address,
            &session,
            call,
            "write_stdin",
            stdin(&operation, "", Some(cursor)),
        )?;
        let (_, output, next) = page(&raw, &operation)?;
        observed.push_str(&output);
        cursor = next;
    }
    assert!(
        observed.contains("PTY-READY\n"),
        "missing readiness: {observed}"
    );
    let written = tool(
        &root,
        &provider,
        address,
        &session,
        "call_tc_marker",
        "write_stdin",
        stdin(&operation, "MATCHED-MARKER\n", Some(cursor)),
    )?;
    let (_, output, next) = page(&written, &operation)?;
    observed.push_str(&output);
    cursor = next;
    for call in ["call_tc_ack_1", "call_tc_ack_2", "call_tc_ack_3"] {
        if observed.contains("ACK:MATCHED-MARKER") {
            break;
        }
        let raw = tool(
            &root,
            &provider,
            address,
            &session,
            call,
            "write_stdin",
            stdin(&operation, "", Some(cursor)),
        )?;
        let (_, output, next) = page(&raw, &operation)?;
        observed.push_str(&output);
        cursor = next;
    }
    assert_eq!(observed.matches("PTY-READY\n").count(), 1);
    assert_eq!(
        observed.matches("ACK:MATCHED-MARKER").count(),
        1,
        "missing or duplicate output: {observed}"
    );
    assert_eq!(
        fs::read_to_string(root.join("markers.txt"))?,
        "MATCHED-MARKER\n"
    );

    let interrupted = tool(
        &root,
        &provider,
        address,
        &session,
        "call_tc_interrupt",
        "write_stdin",
        stdin(&operation, "\u{3}", Some(cursor)),
    )?;
    let (mut final_page, output, next) = page(&interrupted, &operation)?;
    observed.push_str(&output);
    cursor = next;
    for call in ["call_tc_done_1", "call_tc_done_2", "call_tc_done_3"] {
        if final_page["done"] == true {
            break;
        }
        let raw = tool(
            &root,
            &provider,
            address,
            &session,
            call,
            "write_stdin",
            stdin(&operation, "", Some(cursor)),
        )?;
        let (value, output, next) = page(&raw, &operation)?;
        final_page = value;
        observed.push_str(&output);
        cursor = next;
    }
    assert_eq!(
        final_page["done"], true,
        "terminal cleanup did not settle: {final_page}"
    );
    assert_eq!(final_page["cleanup"], "complete");
    assert_eq!(final_page["pipe"], "drained");
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(root.join("stopped.txt"))?, "interrupted");
    assert_eq!(fs::read_to_string(root.join("launches.txt"))?, "launch\n");
    assert_eq!(
        fs::read_to_string(root.join("markers.txt"))?,
        "MATCHED-MARKER\n"
    );
    assert_eq!(observed.matches("ACK:MATCHED-MARKER").count(), 1);
    service.stop_after_witness(&root);
    drop(service);
    process_fixture::cleanup(&root);
    Ok(())
}

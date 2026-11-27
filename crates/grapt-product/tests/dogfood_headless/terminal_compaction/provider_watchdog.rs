//! A real quiet terminal poll outlives the production provider idle window.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn rel14_long_write_stdin_poll_completes_without_killing_the_execution(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("rel14-terminal-provider-watchdog");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 180_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    // No output or exit can wake the empty poll early. Its own yield must end
    // the wait; a later input proves the same live process survived it.
    #[cfg(unix)]
    let command = "IFS= read -r word; printf 'grapt-rel14:%s' \"$word\"";
    #[cfg(windows)]
    let command = "$word=[Console]::ReadLine(); [Console]::Write('grapt-rel14:'+$word)";
    let args = serde_json::json!({
        "cmd":command,"yield_time_ms":250,"timeout_ms":120_000,"max_output_tokens":256
    });
    #[cfg(windows)]
    let args = {
        let mut args = args;
        let system = std::env::var_os("SystemRoot").ok_or("Windows SystemRoot absent")?;
        args["shell"] = PathBuf::from(system)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe")
            .to_str()
            .ok_or("PowerShell path is not UTF-8")?
            .into();
        args
    };
    let started: serde_json::Value = serde_json::from_str(&tool(
        &root,
        &provider,
        address,
        &session,
        "call_rel14_start",
        "exec_command",
        args,
    )?)?;
    assert_eq!(started["done"], false);
    let operation = started["session_id"]
        .as_str()
        .ok_or("terminal handle absent")?;
    let window = grapt_runtime::StepPolicy::ruled_default()?.no_progress_timeout();
    let yield_ms = u64::try_from(window.as_millis())?
        .checked_add(5_000)
        .ok_or("poll duration overflow")?;
    assert!(yield_ms <= 300_000, "fixture must use a legal long poll");
    let before = provider.calls.load(Ordering::SeqCst);
    provider.append(vec![
        Script::ToolInput {
            call: "call_rel14_poll", tool: "write_stdin",
            arguments: serde_json::json!({"session_id":operation,"chars":"","yield_time_ms":yield_ms}),
        },
        Script::Success,
    ]);
    let input = "msg_rel14_long_poll";
    let began = client_allowance::now();
    json(address, "POST", &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":input,"text":"poll the existing terminal","delivery":"queue","resume":true}).to_string());
    let ended = permission_residuals::wait_until(
        window + Duration::from_secs(30),
        Duration::from_millis(50),
        || {
            // An idle gap before admission is not completion of this input.
            if provider.calls.load(Ordering::SeqCst) <= before {
                return false;
            }
            let active = json(address, "GET", "/api/session/active", "");
            !active["data"]
                .as_object()
                .unwrap_or_else(|| panic!("active execution response is not an object"))
                .contains_key(&session)
        },
    );
    assert!(
        ended.is_some(),
        "long poll did not settle within its own bound"
    );
    manual_compaction::assert_carried(&root, input, "session.execution.succeeded.1");
    assert!(
        began.elapsed() >= window,
        "the fixture did not cross the provider watchdog window"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        before + 2,
        "poll lost its continuation or replayed"
    );
    let body: serde_json::Value = serde_json::from_str(
        locked(&provider.bodies)
            .last()
            .ok_or("continuation request absent")?,
    )?;
    let output = body["input"]
        .as_array()
        .ok_or("provider input absent")?
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "call_rel14_poll")
        .and_then(|item| item["output"].as_str())
        .ok_or("poll output absent")?;
    let output: serde_json::Value = serde_json::from_str(output)?;
    assert_eq!(output["session_id"], operation);
    assert_eq!(output["done"], false);
    assert_eq!(output["status"], "running");
    assert_eq!(output["output"], "");
    let finished: serde_json::Value = serde_json::from_str(&tool(
        &root,
        &provider,
        address,
        &session,
        "call_rel14_finish",
        "write_stdin",
        serde_json::json!({"session_id":operation,"chars":"finish\n","yield_time_ms":5000}),
    )?)?;
    assert_eq!(finished["exit_code"], 0);
    assert_eq!(finished["done"], true);
    assert_eq!(finished["cleanup"], "complete");
    assert_eq!(finished["output"], "grapt-rel14:finish");
    service.stop_after_witness(&root);
    drop(service);
    process_fixture::cleanup(&root);
    Ok(())
}

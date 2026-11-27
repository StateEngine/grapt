//! Real argv subprocess witnesses; no process-wide environment mutation.
use super::*;
use crate::dogfood::catalog_config::CatalogDocument;
use grapt_types::IdAuthority;
use serde_json::json;
use std::path::PathBuf;

// The test harness may run tests concurrently, but this slice admits only one
// hook process tree at a time (including its cleanup assertion).
static CHILD_SLOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// The v1 checks assert annotations; v2 decisions are exercised at the tool seam.
async fn run(
    command: &HookCommand,
    environment: &Environment,
    input: &[u8],
) -> Result<Vec<String>, String> {
    Ok(super::run(command, environment, input).await?.notes)
}
fn notes(stdout: &str) -> Result<Vec<String>, String> {
    Ok(super::super::decision::parse(stdout, false)?.notes)
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, String> {
        let ids = grapt_store::OsRandomIds::seed().map_err(|_| "fixture random seed failed")?;
        let root = std::env::temp_dir().join(format!("grapt-hook-command-{}", ids.mint().as_str()));
        std::fs::create_dir(&root).map_err(|error| error.to_string())?;
        Ok(Self { root })
    }

    fn environment(&self) -> Result<Environment, String> {
        Environment::read(&self.root, &self.root, &[])
    }

    /// Resolve exactly the documented public JSONC declaration shape instead of
    /// constructing HookCommand internals (including its owning hook ID).
    fn script(&self, unix: &str, windows: &str, timeout_ms: u64) -> Result<HookCommand, String> {
        #[cfg(windows)]
        let argv = {
            let system = std::env::var_os("SystemRoot").ok_or("SystemRoot is missing")?;
            let program =
                PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let _ = unix;
            vec![
                program.to_string_lossy().into_owned(),
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                windows.to_owned(),
            ]
        };
        #[cfg(not(windows))]
        let argv = {
            let _ = windows;
            vec!["/bin/sh".to_owned(), "-c".to_owned(), unix.to_owned()]
        };
        let value = json!({
            "hook_commands": {"fixture": {"command": argv, "timeout_ms": timeout_ms}},
            "hooks": {"PreToolUse": ["fixture"]}
        });
        let value = grapt_config::decode(
            &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
            crate::dogfood::catalog_config::bounds()?,
        )
        .map_err(|error| error.to_string())?
        .into_value();
        let document = CatalogDocument {
            source: self.root.join("grapt.jsonc").to_string_lossy().into_owned(),
            directory: self.root.clone(),
            origin: grapt_plugin::PluginOrigin::UserGlobal,
            value,
        };
        let settings = super::super::config::HookSettings::from_sources(&[document])?;
        settings
            .commands(super::super::config::Event::PreToolUse)
            .first()
            .cloned()
            .ok_or_else(|| "fixture hook was not configured".into())
    }

    fn emitting(&self, out: &[u8], err: &[u8], exit: i32) -> Result<HookCommand, String> {
        fn shell_bytes(bytes: &[u8]) -> String {
            bytes.iter().map(|byte| format!("\\{byte:03o}")).collect()
        }
        fn ps_bytes(bytes: &[u8], stream: &str) -> String {
            if bytes.is_empty() {
                return String::new();
            }
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            format!(
                "$b=[Convert]::FromBase64String('{encoded}');\
                 [Console]::OpenStandard{stream}().Write($b,0,$b.Length);"
            )
        }
        self.script(
            &format!(
                "printf '{}' ; printf '{}' >&2; exit {exit}",
                shell_bytes(out),
                shell_bytes(err)
            ),
            &format!(
                "{}{}exit {exit}",
                ps_bytes(out, "Output"),
                ps_bytes(err, "Error")
            ),
            10_000,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn configured_argv_roundtrips_stdin_and_all_supported_outputs() -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let environment = fixture.environment()?;
    for (stdout, expected) in [
        ("", Vec::new()),
        ("  plain annotation\n", vec!["plain annotation"]),
        (
            r#"{"hookSpecificOutput":{"additionalContext":"context"}}"#,
            vec!["context"],
        ),
        (
            r#"{"decision":"approve","hookSpecificOutput":{"permissionDecision":"allow","additionalContext":"still just context","updatedInput":{"danger":true}}}"#,
            vec!["still just context"],
        ),
        (r#"{"decision":"allow"}"#, Vec::new()),
        (
            r#"{"systemMessage":7,"suppressOutput":"ignored","hookSpecificOutput":{"hookEventName":7,"updatedInput":"ignored","additionalContext":"legacy context"}}"#,
            vec!["legacy context"],
        ),
    ] {
        let command = fixture.emitting(stdout.as_bytes(), b"ignored successful stderr", 0)?;
        assert_eq!(run(&command, &environment, b"{}").await?, expected);
    }
    let command = fixture.script(
        "/bin/cat",
        "[Console]::Out.Write([Console]::In.ReadToEnd())",
        10_000,
    )?;
    assert_eq!(
        run(
            &command,
            &environment,
            br#"{"hookSpecificOutput":{"additionalContext":"stdin reached EOF"}}"#,
        )
        .await?,
        ["stdin reached EOF"]
    );
    Ok(())
}

#[tokio::test]
async fn child_blocks_crashes_invalid_utf8_and_malformed_json_fail_closed() -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let environment = fixture.environment()?;
    for (stdout, stderr, exit, expected) in [
        (
            br#"{"decision":"block","reason":"legacy refusal"}"#.as_slice(),
            b"".as_slice(),
            0,
            "legacy refusal",
        ),
        (
            br#"{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"policy refusal"}}"#.as_slice(),
            b"".as_slice(),
            0,
            "policy refusal",
        ),
        (
            br#"{"continue":false,"stopReason":"stop refusal"}"#.as_slice(),
            b"".as_slice(),
            0,
            "stop refusal",
        ),
        (b"allow", b"stderr refusal", 2, "stderr refusal"),
        (b"allow", b"", 2, "no reason supplied"),
        (b"allow", b"not exposed", 7, "exit code 7"),
        (b"{truncated", b"", 0, "invalid structured output"),
        (b"[null,null,null,null,null]", b"", 0, "invalid structured output"),
        (b"{\"continue\":\"false\"}", b"", 0, "invalid structured output"),
        (b"\xff", b"", 0, "stdout is not UTF-8"),
        (b"", b"\xff", 0, "stderr is not UTF-8"),
        (b"", b"\xff", 2, "stderr is not UTF-8"),
    ] {
        let command = fixture.emitting(stdout, stderr, exit)?;
        let reason = run(&command, &environment, b"{}").await.err().ok_or("hook unexpectedly allowed")?;
        assert!(reason.contains(expected), "unexpected reason: {reason}");
        if exit == 7 {
            assert!(!reason.contains("not exposed"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn bounds_cover_both_output_streams_and_stalled_stdin() -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let environment = fixture.environment()?;
    for (unix, windows, label) in [
        (
            "i=0; while [ $i -lt 16385 ]; do printf x; i=$((i+1)); done",
            "[Console]::Out.Write(('x' * 16385))",
            "stdout",
        ),
        (
            "i=0; while [ $i -lt 16385 ]; do printf x >&2; i=$((i+1)); done",
            "[Console]::Error.Write(('x' * 16385))",
            "stderr",
        ),
    ] {
        let command = fixture.script(unix, windows, 10_000)?;
        let reason = run(&command, &environment, b"{}")
            .await
            .err()
            .ok_or("overflow allowed")?;
        assert!(
            reason.contains(&format!("{label} exceeds 16384 bytes")),
            "{reason}"
        );
    }
    let command = fixture.script("/bin/sleep 30", "Start-Sleep -Seconds 30", 100)?;
    let reason = run(&command, &environment, &vec![b'x'; MAX_STREAM_BYTES])
        .await
        .err()
        .ok_or("stalled command allowed")?;
    assert!(reason.contains("timed out"), "{reason}");
    for input in [&vec![b'x'; MAX_STREAM_BYTES + 1][..], &[255][..]] {
        let reason = run(&command, &environment, input)
            .await
            .err()
            .ok_or("invalid stdin allowed")?;
        assert!(reason.contains("stdin"));
    }
    // Exact cap is legal, not truncated; producing then reading concurrently
    // avoids deadlock when a child fills stdout before it starts reading stdin.
    let command = fixture.script(
        "i=0; while [ $i -lt 16384 ]; do printf x; i=$((i+1)); done; /bin/cat >/dev/null",
        "[Console]::Out.Write(('x' * 16384)); $null=[Console]::In.ReadToEnd()",
        10_000,
    )?;
    let notes = run(&command, &environment, &vec![b'x'; MAX_STREAM_BYTES]).await?;
    assert_eq!(notes, ["x".repeat(MAX_STREAM_BYTES)]);
    let command = fixture.script(
        "i=0; while [ $i -lt 16384 ]; do printf x >&2; i=$((i+1)); done",
        "[Console]::Error.Write(('x' * 16384))",
        10_000,
    )?;
    assert!(run(&command, &environment, b"{}").await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn completion_kills_pipe_holding_descendants() -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let environment = fixture.environment()?;
    let command = fixture.script(
        "(printf ready > descendant-ready; /bin/sleep 1; printf leaked > leaked) & \
         while [ ! -f descendant-ready ]; do /bin/sleep 0.01; done; exit 0",
        "$s='[IO.File]::WriteAllText(\"descendant-ready\",\"ready\"); \
         Start-Sleep -Seconds 1; [IO.File]::WriteAllText(\"leaked\",\"leaked\")'; \
         $e=[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($s)); \
         $p=(Get-Process -Id $PID).Path; \
         $null=Start-Process -FilePath $p -NoNewWindow -PassThru \
         -ArgumentList \"-NoLogo -NoProfile -NonInteractive -EncodedCommand $e\"; \
         while (!(Test-Path 'descendant-ready')) { Start-Sleep -Milliseconds 10 }; exit 0",
        10_000,
    )?;
    assert!(run(&command, &environment, b"{}").await?.is_empty());
    assert!(fixture.root.join("descendant-ready").exists());
    tokio::time::sleep(Duration::from_millis(1800)).await;
    assert!(
        !fixture.root.join("leaked").exists(),
        "completed hook leaked a descendant"
    );
    Ok(())
}

#[tokio::test]
async fn dropping_run_future_kills_a_live_child() -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let environment = fixture.environment()?;
    let command = fixture.script(
        "printf ready > ready; /bin/sleep 1; printf leaked > leaked",
        "[IO.File]::WriteAllText('ready','ready'); Start-Sleep -Seconds 1; \
         [IO.File]::WriteAllText('leaked','leaked')",
        10_000,
    )?;
    let mut running = Box::pin(run(&command, &environment, b"{}"));
    let ready = async {
        for _ in 0..500 {
            if fixture.root.join("ready").exists() {
                return Ok::<_, String>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Err("fixture child never became ready".to_owned())
    };
    tokio::select! {
        result = &mut running => return Err(format!("hook ended before cancellation: {result:?}")),
        result = ready => result?,
    }
    drop(running);
    tokio::time::sleep(Duration::from_millis(1800)).await;
    assert!(
        !fixture.root.join("leaked").exists(),
        "cancelled hook kept running"
    );
    Ok(())
}

#[tokio::test]
async fn spawn_errors_never_echo_argv_or_paths() -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let environment = fixture.environment()?;
    let mut command = fixture.emitting(b"", b"", 0)?;
    command.command = vec![fixture
        .root
        .join("secret-do-not-echo")
        .to_string_lossy()
        .into_owned()];
    let reason = run(&command, &environment, b"{}")
        .await
        .err()
        .ok_or("absent executable ran")?;
    assert!(reason.contains("spawn failed"), "{reason}");
    assert!(!reason.contains("secret-do-not-echo"));
    assert!(!reason.contains(&*fixture.root.to_string_lossy()));
    Ok(())
}

#[tokio::test]
async fn hook_child_cannot_receive_explicit_launcher_identity_overrides() -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let environment = fixture.environment()?;
    let hook = fixture.script(
        "/usr/bin/env",
        "Get-ChildItem Env: | ForEach-Object { [Console]::WriteLine($_.Name+'='+$_.Value) }",
        10_000,
    )?;
    let (program, arguments) = hook.command.split_first().ok_or("hook argv is empty")?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .current_dir(&hook.directory)
        .env_clear()
        .envs(environment.iter())
        .kill_on_drop(true);
    for name in [
        "STATEENGINE_AGENT_ID",
        "stateengine_future_identity",
        "SECODE_STATEENGINE_PROJECT_ID",
        "secode_stateengine_future_identity",
        "SECODE_SESSION_ID",
        "secode_session_id",
    ] {
        command.env(name, "launcher-identity-must-not-reach-hook");
    }
    command.env("NORMAL_SETTING", "ordinary-hook-setting");
    grapt_service::launcher_environment::scrub(command.as_std_mut());
    let output = tokio::time::timeout(Duration::from_millis(hook.timeout_ms), command.output())
        .await
        .map_err(|_| "hook environment probe timed out")?
        .map_err(|error| error.to_string())?;
    assert!(output.status.success(), "hook environment probe failed");
    let output = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    assert!(output
        .lines()
        .any(|line| line == "NORMAL_SETTING=ordinary-hook-setting"));
    for line in output.lines() {
        let name = line.split_once('=').map_or(line, |(name, _)| name);
        let upper = name.to_ascii_uppercase();
        assert!(
            !upper.starts_with("STATEENGINE_")
                && !upper.starts_with("SECODE_STATEENGINE_")
                && upper != "SECODE_SESSION_ID",
            "hook inherited launcher identity: {name}"
        );
    }
    assert!(!output.contains("launcher-identity-must-not-reach-hook"));
    Ok(())
}

#[test]
fn structured_denials_dominate_allow_and_duplicate_decisions_fail_closed() {
    for output in [
        r#"{"decision":"allow","hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"deny wins"}}"#,
        r#"{"decision":"block","reason":"deny wins","hookSpecificOutput":{"permissionDecision":"allow"}}"#,
        r#"{"continue":false,"stopReason":"deny wins","decision":"allow"}"#,
        r#"{"decision":"allow","decision":"block","reason":"deny wins"}"#,
        r#"{"hookSpecificOutput":{"permissionDecision":123}}"#,
    ] {
        assert!(
            notes(output).is_err(),
            "structured refusal allowed: {output}"
        );
    }
}

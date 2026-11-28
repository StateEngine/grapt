//! The shell owns and stops its whole process tree.
use super::*;
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
#[test]
fn shell_status_reservation_preserves_exact_near_cap_stream_markers() -> Result<(), String> {
    use std::os::unix::process::ExitStatusExt;
    // The edge introduces stdout and stderr markers on different passes in
    // the old renderer. The exact-fit neighbour needs neither marker.
    for (name, out_len, err_len, out_seen, err_seen, out_kept, err_kept) in [
        ("edge", 524_288, 524_260, 524_288, 524_260, 524_225, 524_198),
        (
            "exact fit",
            524_288,
            524_259,
            524_288,
            524_259,
            524_288,
            524_259,
        ),
        (
            "edge plus one",
            524_288,
            524_261,
            524_288,
            524_261,
            524_225,
            524_198,
        ),
        ("one complete", 524_288, 0, 524_288, 0, 524_288, 0),
        (
            "two complete",
            524_288,
            524_288,
            524_288,
            524_288,
            524_211,
            524_212,
        ),
        ("one truncated", 524_288, 0, 614_400, 0, 524_288, 0),
        (
            "two truncated",
            524_288,
            524_260,
            614_400,
            614_400,
            524_225,
            524_198,
        ),
    ] {
        let output = conclude_shell(
            ShellRun {
                stderr_tail: None,
                exit: Some(Ok(std::process::ExitStatus::from_raw(0))),
                cleanup: Ok(()),
                drained: Some(Ok(())),
                out: BoundedStream {
                    bytes: vec![b'O'; out_len],
                    observed: out_seen,
                },
                err: BoundedStream {
                    bytes: vec![b'E'; err_len],
                    observed: err_seen,
                },
            },
            Vec::new(),
            5_000,
        )?
        .model_output()?;
        let mut expected = Vec::new();
        for (stream, byte, kept, observed) in [
            ("stdout", b'O', out_kept, out_seen),
            ("stderr", b'E', err_kept, err_seen),
        ] {
            expected.extend(std::iter::repeat_n(byte, kept));
            if observed > kept as u64 {
                expected.extend_from_slice(
                    format!(
                        "\n[{stream} output bounded: kept {kept} of {observed} observed bytes]"
                    )
                    .as_bytes(),
                );
            }
        }
        expected.extend_from_slice(b"\n\nCommand exited with code 0.");
        assert!(
            output.len() as u64 <= grapt_runtime::TOOL_ARTIFACT_MAX_BYTES,
            "{name}"
        );
        assert_eq!(
            output, expected,
            "{name}: stream counts or final status changed"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn shell_observed_exit_preserves_status_and_both_streams_at_the_artifact_cap() -> Result<(), String>
{
    for code in [0, 1, 7] {
        let status = Command::new("/bin/sh")
            .args(["-c", &format!("exit {code}")])
            .status()
            .map_err(|error| error.to_string())?;
        let output = conclude_shell(
            ShellRun {
                stderr_tail: None,
                exit: Some(Ok(status)),
                cleanup: Ok(()),
                drained: Some(Ok(())),
                out: BoundedStream {
                    bytes: vec![b'O'; SHELL_STREAM_MAX_BYTES],
                    observed: SHELL_STREAM_MAX_BYTES as u64,
                },
                err: BoundedStream {
                    bytes: vec![b'E'; SHELL_STREAM_MAX_BYTES],
                    observed: SHELL_STREAM_MAX_BYTES as u64,
                },
            },
            Vec::new(),
            5_000,
        )?
        .model_output()?;
        assert_eq!(output.len(), SHELL_OUTPUT_MAX_BYTES);
        let text = String::from_utf8(output).map_err(|error| error.to_string())?;
        assert!(text.ends_with(&format!("\n\nCommand exited with code {code}.")));
        assert!(text.contains("OOO") && text.contains("EEE"));
        assert!(text.contains("[stdout output bounded: kept "));
        assert!(text.contains("[stderr output bounded: kept "));
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn shell_signal_is_an_observed_result_and_wait_failure_stays_a_failure() -> Result<(), String> {
    let status = Command::new("/bin/sh")
        .args(["-c", "kill -TERM $$"])
        .status()
        .map_err(|error| error.to_string())?;
    let run = |exit| ShellRun {
        stderr_tail: None,
        exit: Some(exit),
        cleanup: Ok(()),
        drained: Some(Ok(())),
        out: BoundedStream::default(),
        err: BoundedStream::default(),
    };
    let output = conclude_shell(run(Ok(status)), Vec::new(), 5_000)?.model_output()?;
    assert!(String::from_utf8_lossy(&output).contains("\n\nCommand terminated by signal ("));
    let failure = conclude_shell(
        run(Err(std::io::Error::other("SHELL_WAIT_WITNESS"))),
        Vec::new(),
        5_000,
    )
    .err()
    .ok_or("failed shell wait was reported as completed")?;
    assert!(failure.contains("shell wait failed: SHELL_WAIT_WITNESS"));
    Ok(())
}

/// A plain `sh -c` configured as run_shell configures it.
#[cfg(unix)]
fn sh(root: &Path, script: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args(["-c", script])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Dead or a zombie awaiting its reaper; either way it no longer runs.
#[cfg(target_os = "linux")]
async fn stopped(pid: &str) -> bool {
    let running = || {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .is_some_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| !rest.starts_with('Z'))
            })
    };
    // Up to five seconds for the kill to land.
    for _ in 0..250 {
        if !running() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    !running()
}

#[cfg(unix)]
fn background_pid(scratch: &Scratch) -> Result<String, String> {
    std::fs::read_to_string(scratch.0.join("bg.pid"))
        .map(|text| text.trim().to_owned())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shell_kills_its_whole_tree_and_keeps_output_on_timeout() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let shell = |text: &str, timeout_ms: u64| {
        let root = scratch.0.clone();
        let text = text.to_owned();
        async move {
            let shell_input =
                input(&serde_json::json!({"command":text,"timeout_ms":timeout_ms}).to_string())?;
            let prepared = prepare_effect(&root, SHELL_TOOL, &shell_input, &no_routes())?;
            let EffectTarget::Shell {
                command,
                program,
                mut environment,
            } = prepared.target
            else {
                return Err("shell preparation changed effect class".to_owned());
            };
            environment.without_sandbox();
            Ok::<_, String>(
                run_shell(
                    &root,
                    &command,
                    &program,
                    &environment,
                    &shell_input,
                    &[],
                    None,
                )
                .await,
            )
        }
    };

    // A background job holding stdout no longer stalls the call until timeout.
    let started = tokio::time::Instant::now();
    let output = shell("sleep 60 & echo $! > bg.pid; echo started", 20_000)
        .await??
        .model_output()?;
    assert!(String::from_utf8_lossy(&output).contains("started"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(
        stopped(&background_pid(&scratch)?).await,
        "background job outlived the call"
    );

    // On timeout the grandchild dies too, and output so far is returned. The
    // timeout is the subject: the command waits on a 60 s sleep, far beyond
    // it. It still leaves a loaded box time to spawn the shell and print
    // "partial" first, which 500 ms did not.
    let failure = shell("sleep 60 & echo $! > bg.pid; echo partial; wait", 5_000)
        .await?
        .err()
        .ok_or("hung command completed")?;
    assert!(failure.contains("timed out after 5000 ms"), "{failure}");
    assert!(failure.contains("partial"), "{failure}");
    assert!(
        stopped(&background_pid(&scratch)?).await,
        "grandchild outlived the timeout"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn shell_without_containment_stops_draining_a_held_pipe_and_says_so() -> Result<(), String> {
    let scratch = Scratch::new()?;
    // Nothing kills the survivor, which holds stdout past the grace period.
    let started = tokio::time::Instant::now();
    let output = supervise_shell(
        sh(&scratch.0, "sleep 6 & echo $! > bg.pid; echo started"),
        20_000,
        Err("disabled in this test".to_owned()),
        &[],
    )
    .await?
    .model_output()?;
    // So the test stops it: it inherits this process's descriptors, among
    // them the build's target lease, which it held for the rest of its sleep.
    let survivor = background_pid(&scratch)?;
    let killed = tokio::process::Command::new("kill")
        .arg(&survivor)
        .status()
        .await
        .map_err(|error| error.to_string())?;
    assert!(killed.success(), "the survivor could not be stopped");
    #[cfg(target_os = "linux")]
    assert!(stopped(&survivor).await, "the survivor outlived the test");
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("started"), "{text}");
    assert!(
        text.contains("process containment is unavailable (disabled in this test)"),
        "{text}"
    );
    assert!(text.contains("output collection stopped"), "{text}");
    assert!(
        started.elapsed() < SHELL_DRAIN_GRACE + Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn shell_reports_a_failed_kill_and_keeps_its_output() -> Result<(), String> {
    use std::os::unix::process::ExitStatusExt;
    let partial = || BoundedStream {
        bytes: b"partial\n".to_vec(),
        observed: 8,
    };
    let failure = conclude_shell(
        ShellRun {
            stderr_tail: None,
            exit: None,
            cleanup: Err("permission denied".to_owned()),
            drained: Some(Ok(())),
            out: partial(),
            err: BoundedStream::default(),
        },
        Vec::new(),
        500,
    )
    .err()
    .ok_or("a timed-out run succeeded")?;
    assert!(failure.contains("timed out after 500 ms"), "{failure}");
    assert!(failure.contains("partial"), "{failure}");
    assert!(
        failure.contains("killing the command's processes failed (permission denied)"),
        "{failure}"
    );
    // A clean exit whose leftover jobs could not be killed still says so.
    let output = conclude_shell(
        ShellRun {
            stderr_tail: None,
            exit: Some(Ok(std::process::ExitStatus::from_raw(0))),
            cleanup: Err("permission denied".to_owned()),
            drained: Some(Ok(())),
            out: partial(),
            err: BoundedStream::default(),
        },
        Vec::new(),
        500,
    )?
    .model_output()?;
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.starts_with("partial\n\n\n[shell: killing")
            && text.ends_with("\n\nCommand exited with code 0."),
        "{text}"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shell_setsid_child_is_killed_or_reported() -> Result<(), String> {
    // The POSIX process-group fallback cannot reach a new session; it says so.
    assert!(containment_notice(processkit::Mechanism::ProcessGroup)
        .is_some_and(|notice| notice.contains("setsid")));
    let scratch = Scratch::new()?;
    let group = processkit::ProcessGroup::new().map_err(|error| error.to_string())?;
    let notice = containment_notice(group.mechanism());
    let output = supervise_shell(
        sh(
            &scratch.0,
            "setsid sleep 60 >/dev/null 2>&1 & echo $! > bg.pid; echo started",
        ),
        20_000,
        Ok(group),
        &[],
    )
    .await?
    .model_output()?;
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("started"), "{text}");
    match notice {
        // Under the fallback the escaped child is disclosed, not silently missed.
        Some(notice) => assert!(text.contains(notice), "{text}"),
        // A cgroup or reaper holds the new session too.
        None => assert!(
            stopped(&background_pid(&scratch)?).await,
            "setsid child escaped containment unreported: {text}"
        ),
    }
    Ok(())
}

/// The Job Object path, exercised through the windows-native door.
#[cfg(windows)]
#[tokio::test]
async fn shell_job_object_owns_the_tree_on_windows() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let command = |program: &str, args: &[&str]| {
        let mut command = windows_spawn::Command::new(program);
        command
            .args(args)
            .current_dir(&scratch.0)
            .stdin(windows_spawn::Stdio::null())
            .stdout(windows_spawn::Stdio::piped())
            .stderr(windows_spawn::Stdio::piped());
        command
    };
    // An unreadable process table is an error, never "not running".
    let running = |pid: u32| {
        let listed = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map_err(|error| error.to_string())?;
        if !listed.status.success() {
            return Err(format!(
                "tasklist failed ({}): {}",
                listed.status,
                String::from_utf8_lossy(&listed.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&listed.stdout)
            .to_ascii_uppercase()
            .contains("PING.EXE"))
    };

    // A plain command returns its output.
    let output = windows_shell::supervise(
        command("cmd.exe", &["/D", "/S", "/C", "echo ok"]),
        30_000,
        &[],
        None,
    )
    .await?
    .model_output()?;
    assert!(String::from_utf8_lossy(&output).contains("ok"));

    // A background child the command starts does not outlive the call.
    let output = windows_shell::supervise(
        command(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$ErrorActionPreference = 'Stop'; $p = Start-Process ping.exe -ArgumentList '-n','30','127.0.0.1' -PassThru -WindowStyle Hidden; if (-not $p -or $p.Id -le 0) { throw 'no child started' }; Set-Content bg.pid $p.Id; Write-Output started",
            ],
        ),
        30_000,
        &[],
        None,
    )
    .await?
    .model_output()?;
    assert!(String::from_utf8_lossy(&output).contains("started"));
    let recorded =
        std::fs::read_to_string(scratch.0.join("bg.pid")).map_err(|error| error.to_string())?;
    let pid = recorded
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| format!("bg.pid holds no child pid: {recorded:?}"))?;
    let mut alive = running(pid)?;
    for _ in 0..50 {
        if !alive {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        alive = running(pid)?;
    }
    assert!(!alive, "background ping {pid} outlived the call");

    // A hung command times out and returns its partial output.
    let failure = windows_shell::supervise(
        command(
            "cmd.exe",
            &[
                "/D",
                "/S",
                "/C",
                "echo partial& %SystemRoot%\\System32\\PING.EXE -n 30 127.0.0.1",
            ],
        ),
        3_000,
        &[],
        None,
    )
    .await
    .err()
    .ok_or("hung command completed")?;
    assert!(failure.contains("timed out after 3000 ms"), "{failure}");
    assert!(failure.contains("partial"), "{failure}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn shell_machine_output_is_byte_exact_and_nonzero_or_signal_stays_refused(
) -> Result<(), String> {
    let scratch = Scratch::new()?;
    let group = || processkit::ProcessGroup::new().map_err(|error| error.to_string());
    let output = supervise_shell(
        sh(&scratch.0, "printf 'hello\nCommand exited with code 0.'"),
        5_000,
        group(),
        &[],
    )
    .await?
    .command_text()?;
    assert_eq!(output, b"hello\nCommand exited with code 0.");
    for command in [
        "printf 'Command exited with code 0.'; exit 1",
        "printf partial; kill -TERM $$",
    ] {
        let refusal = supervise_shell(sh(&scratch.0, command), 5_000, group(), &[])
            .await?
            .command_text()
            .err()
            .ok_or("machine consumer accepted failed shell")?;
        assert!(refusal.contains("shell exited with") && refusal.contains("captured output:"));
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn shell_unavailable_containment_refuses_success_without_losing_command_output(
) -> Result<(), String> {
    let scratch = Scratch::new()?;
    let outcome = supervise_shell(
        sh(&scratch.0, "printf hello"),
        5_000,
        Err("disabled in this test".to_owned()),
        &[],
    )
    .await?;
    let refusal = outcome
        .command_text()
        .err()
        .ok_or("unconfirmed shell cleanup was accepted")?;
    assert!(refusal.contains("cleanup unconfirmed") && refusal.contains("captured output: hello"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn shell_supervision_notices_are_separate_and_never_enter_machine_bytes() -> Result<(), String> {
    use std::os::unix::process::ExitStatusExt;
    let raw = b"hello\nCommand exited with code 0.";
    for mechanism in [
        processkit::Mechanism::CgroupV2,
        processkit::Mechanism::ProcessGroup,
    ] {
        let notices: Vec<String> = containment_notice(mechanism)
            .map(str::to_owned)
            .into_iter()
            .collect();
        let outcome = |code| {
            conclude_shell(
                ShellRun {
                    stderr_tail: None,
                    exit: Some(Ok(std::process::ExitStatus::from_raw(code << 8))),
                    cleanup: Ok(()),
                    drained: Some(Ok(())),
                    out: BoundedStream {
                        bytes: raw.to_vec(),
                        observed: raw.len() as u64,
                    },
                    err: BoundedStream {
                        bytes: b" stderr".to_vec(),
                        observed: 7,
                    },
                },
                notices.clone(),
                5_000,
            )
        };
        let mut command = raw.to_vec();
        command.extend_from_slice(b" stderr");
        assert_eq!(outcome(0)?.command_text()?, command);
        for code in [0, 1, 7] {
            let mut expected =
                String::from_utf8(command.clone()).map_err(|error| error.to_string())?;
            for notice in &notices {
                expected.push_str(&format!("\n\n[shell: {notice}]"));
            }
            expected.push_str(&format!("\n\nCommand exited with code {code}."));
            assert_eq!(outcome(code)?.model_output()?, expected.as_bytes());
            if code != 0 {
                let failure = outcome(code)?
                    .command_text()
                    .err()
                    .ok_or("machine nonzero was accepted")?;
                assert!(!failure.contains("[shell:") && failure.ends_with(" stderr"));
            }
        }
        let capped = conclude_shell(
            ShellRun {
                stderr_tail: None,
                exit: Some(Ok(std::process::ExitStatus::from_raw(0))),
                cleanup: Ok(()),
                drained: Some(Ok(())),
                out: BoundedStream {
                    bytes: vec![b'O'; SHELL_STREAM_MAX_BYTES],
                    observed: SHELL_STREAM_MAX_BYTES as u64,
                },
                err: BoundedStream {
                    bytes: vec![b'E'; SHELL_STREAM_MAX_BYTES],
                    observed: SHELL_STREAM_MAX_BYTES as u64,
                },
            },
            notices.clone(),
            5_000,
        )?
        .model_output()?;
        assert!(capped.len() as u64 <= grapt_runtime::TOOL_ARTIFACT_MAX_BYTES);
        let capped = String::from_utf8(capped).map_err(|error| error.to_string())?;
        assert!(
            capped.contains("[stdout output bounded: kept ")
                && capped.contains("[stderr output bounded: kept ")
        );
        assert!(capped.ends_with("\n\nCommand exited with code 0."));
        for notice in &notices {
            assert!(capped.contains(&format!("\n\n[shell: {notice}]\n\nCommand exited")));
        }
    }
    Ok(())
}

use super::*;

mod interrupt;
mod login;
#[cfg(target_os = "macos")]
mod macos;
mod observed;
#[cfg(unix)]
mod recovery;
#[cfg(windows)]
mod windows_spawn;

fn session(value: &str) -> Result<SessionId, String> {
    value
        .parse()
        .map_err(|error| format!("test Session identity: {error}"))
}
fn operation(value: &str) -> Result<OperationId, String> {
    OperationId::new(value).ok_or_else(|| "test operation identity".into())
}
fn spec(root: &Path, command: &str, tty: bool) -> Result<ExecSpec, String> {
    let environment = shell_environment::Environment::parent()?;
    let (program, arguments, cmd_shell) =
        super::command::resolve(None, command, false, &environment)?;
    Ok(ExecSpec {
        origin: None,
        grant: None,
        command: command.into(),
        cwd: root.to_owned(),
        program,
        arguments,
        environment,
        cmd_shell,
        login: false,
        tty,
        wait_ms: 250,
        timeout_ms: 10_000,
        page_bytes: 40_000,
    })
}
#[cfg(unix)]
fn json(bytes: &[u8]) -> Result<serde_json::Value, String> {
    serde_json::from_slice(bytes).map_err(|error| error.to_string())
}
#[cfg(unix)]
fn failure(error: mcp_call::EffectFailure) -> String {
    error.detail
}

#[cfg(unix)]
async fn await_output(entry: &Entry, expected: &str) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let changed = entry.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let data = lock(&entry.observation);
                let page = data.output.page(0, 40_000, data.pipe.is_settled())?;
                if String::from_utf8_lossy(&page.bytes).contains(expected) {
                    return Ok(());
                }
                if data.exit.is_some() && data.pipe.is_settled() {
                    return Err(format!("fixture ended without {expected:?}"));
                }
            }
            changed.await;
        }
    })
    .await
    .map_err(|_| format!("fixture output {expected:?} was not observed within five seconds"))?
}

#[cfg(unix)]
async fn settled_report(entry: &Entry) -> Result<serde_json::Value, String> {
    match tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let changed = entry.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (bytes, _) = observe(entry, 0, 40_000, Some(0)).await?;
            let report = json(&bytes)?;
            if report["done"] == true {
                return Ok(report);
            }
            changed.await;
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => {
            entry.cancel();
            Err("shell fixture did not settle within thirty seconds; cancelled owned entry".into())
        }
    }
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn shell_fixture_completion_deadline_cancels_its_owned_entry() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let entry = registry.reserve(
        session("ses_shell_deadline")?,
        operation("op_shell_deadline")?,
        spec(&scratch.0, "exit 31", false)?,
    )?;
    let error = settled_report(&entry)
        .await
        .err()
        .ok_or("an unsettled shell fixture passed its completion deadline")?;
    assert!(error.contains("did not settle within thirty seconds"));
    assert!(
        supervise::start(Arc::clone(&entry), entry.spec.clone()).is_err(),
        "an expired shell fixture could still start its child"
    );
    assert!(lock(&entry.control).is_none());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn named_bash_and_login_false_reach_the_actual_child() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_shell_choice")?;
    let script = "test -n \"$BASH_VERSION\" || exit 23; if shopt -q login_shell; then printf grapt-fixture-login=yes; else printf grapt-fixture-login=no; fi";
    for (ordinal, (login, expected)) in [
        (None, "grapt-fixture-login=no"),
        (Some(false), "grapt-fixture-login=no"),
        (Some(true), "grapt-fixture-login=yes"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut args = serde_json::json!({
            "cmd":script, "shell":"bash", "yield_time_ms":5000,
        });
        if let Some(login) = login {
            args["login"] = login.into();
        }
        let input =
            BoundedToolInput::new(serde_json::to_vec(&args).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let mut request = arguments::decode_exec(
            &scratch.0,
            &input,
            shell_environment::Environment::parent()?,
        )?;
        // A poll may report a running child. Shell selection does not promise
        // completion within that poll, so force an immediate observation.
        request.wait_ms = 0;
        let id = operation(&format!("op_shell_choice_{ordinal}"))?;
        let (_, delivery) = registry
            .exec(&owner, id.clone(), request)
            .await
            .map_err(failure)?;
        delivery.captured();
        let entry = registry.lookup(&owner, &id)?;
        let output = settled_report(&entry).await?;
        assert_eq!(output["exit_code"], 0, "{output}");
        let text = output["output"].as_str().ok_or("shell output absent")?;
        if login == Some(true) {
            assert!(text.contains(expected), "{output}");
        } else {
            assert_eq!(text, expected);
        }
        assert_eq!(output["done"], true);
    }
    Ok(())
}

#[test]
fn reservations_and_foreign_handles_obey_session_and_global_bounds() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let owner = session("ses_terminal_owner")?;
    let foreign = session("ses_terminal_foreign")?;
    let registry = TerminalRegistry::default();
    let mut entries = Vec::new();
    for index in 0..SESSION_RESOURCES {
        let id = operation(&format!("op_terminal_{index}"))?;
        entries.push(registry.reserve(owner.clone(), id, spec(&scratch.0, "printf ok", false)?)?);
    }
    assert!(registry
        .reserve(
            owner.clone(),
            operation("op_overfull")?,
            spec(&scratch.0, "printf bad", false)?
        )
        .is_err());
    assert!(registry.lookup(&foreign, &entries[0].operation).is_err());
    assert!(registry.lookup(&owner, &entries[0].operation).is_ok());
    let _interaction = entries[0].acquire()?;
    assert!(
        entries[0].acquire().is_err(),
        "parallel writes were admitted"
    );
    for index in 1..=7 {
        let owner = session(&format!("ses_other_{index}"))?;
        for ordinal in 0..SESSION_RESOURCES {
            registry.reserve(
                owner.clone(),
                operation(&format!("op_other_{index}_{ordinal}"))?,
                spec(&scratch.0, "printf ok", false)?,
            )?;
        }
    }
    assert!(registry
        .reserve(
            foreign,
            operation("op_global_overfull")?,
            spec(&scratch.0, "printf bad", false)?
        )
        .is_err());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn real_pty_accepts_input_and_keeps_retryable_output_after_exit() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_interactive")?;
    let id = operation("op_interactive")?;
    let command = "test -t 0 && test -t 1 || exit 17; printf 'ready> '; IFS= read -r word; printf '\\nreply=%s\\n' \"$word\"";
    let (first, delivery) = registry
        .exec(&owner, id.clone(), spec(&scratch.0, command, true)?)
        .await
        .map_err(failure)?;
    let first = json(&first)?;
    assert_eq!(first["status"], "running");
    let entry = registry.lookup(&owner, &id)?;
    await_output(&entry, "ready>").await?;
    assert_eq!(first["done"], false);
    delivery.captured();
    let request = StdinSpec {
        operation: id.clone(),
        chars: "hello\n".into(),
        wait_ms: 5_000,
        page_bytes: 40_000,
        cursor: None,
        line: None,
    };
    let (last, delivery) = registry.stdin(&owner, &request).await.map_err(failure)?;
    let last = json(&last)?;
    assert_eq!(last["exit_code"], 0);
    assert_eq!(last["done"], true);
    assert!(last["output"]
        .as_str()
        .is_some_and(|text| text.contains("reply=hello")));
    delivery.captured();
    let retry = StdinSpec {
        chars: String::new(),
        cursor: Some(0),
        ..request
    };
    let (retried, delivery) = registry.stdin(&owner, &retry).await.map_err(failure)?;
    assert!(json(&retried)?["output"]
        .as_str()
        .is_some_and(|text| text.contains("ready>") && text.contains("reply=hello")));
    delivery.captured();
    assert!(registry.lookup(&session("ses_foreign")?, &id).is_err());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn background_timeout_retains_output_and_does_not_confuse_exit_with_drain(
) -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_background")?;
    let id = operation("op_background")?;
    let request = spec(&scratch.0, "printf before; /bin/sleep 30", false)?;
    let (first, delivery) = registry
        .exec(&owner, id.clone(), request)
        .await
        .map_err(failure)?;
    assert_eq!(json(&first)?["status"], "running");
    await_output(registry.lookup(&owner, &id)?.as_ref(), "before").await?;
    delivery.captured();
    let poll = StdinSpec {
        operation: id,
        chars: String::new(),
        wait_ms: 15_000,
        page_bytes: 40_000,
        cursor: Some(0),
        line: None,
    };
    let (last, delivery) = registry.stdin(&owner, &poll).await.map_err(failure)?;
    let last = json(&last)?;
    assert_eq!(last["status"], "timed_out");
    assert_eq!(last["done"], true);
    assert_eq!(last["pipe"], "drained");
    assert_eq!(last["cleanup"], "complete");
    assert_eq!(last["output"], "before");
    delivery.captured();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn discarded_spawn_result_stops_the_process_and_does_not_advance_output_cursor(
) -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_discarded")?;
    let id = operation("op_discarded")?;
    let (_first, delivery) = registry
        .exec(
            &owner,
            id.clone(),
            spec(&scratch.0, "printf saved; /bin/sleep 10", false)?,
        )
        .await
        .map_err(failure)?;
    await_output(registry.lookup(&owner, &id)?.as_ref(), "saved").await?;
    drop(delivery);
    let poll = StdinSpec {
        operation: id,
        chars: String::new(),
        wait_ms: 5_000,
        page_bytes: 40_000,
        cursor: None,
        line: None,
    };
    let (last, delivery) = registry.stdin(&owner, &poll).await.map_err(failure)?;
    let last = json(&last)?;
    assert_eq!(last["status"], "cancelled");
    assert_eq!(last["done"], true);
    assert_eq!(last["output"], "saved");
    delivery.captured();
    Ok(())
}

#[test]
fn only_empty_owned_polls_use_the_read_only_permission_resource() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_poll_permissions")?;
    let id = operation("op_poll_permissions")?;
    registry.reserve(
        owner.clone(),
        id.clone(),
        spec(&scratch.0, "approved command", false)?,
    )?;
    let make_input = |chars: &str| {
        BoundedToolInput::new(
            serde_json::to_vec(&serde_json::json!({"session_id":id.as_str(),"chars":chars}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    };
    let poll = registry.prepare_stdin(&scratch.0, &owner, &make_input("")?)?;
    assert_eq!(poll.action, "shell.poll");
    assert_eq!(poll.resource, "session terminal output");
    let write = registry.prepare_stdin(&scratch.0, &owner, &make_input("rm file\n")?)?;
    assert_eq!(write.action, "shell");
    assert!(write.resource.contains("approved command") && write.resource.contains("rm file"));
    assert_ne!(write.capability, poll.capability);
    assert!(registry
        .prepare_stdin(
            &scratch.0,
            &session("ses_foreign_permissions")?,
            &make_input("")?
        )
        .is_err());
    Ok(())
}

/// Review HOLD: a destructive command typed in pieces is
/// decided whole, and input too long or edited to read asks.
#[test]
fn split_terminal_input_is_decided_as_the_line_it_completes() -> Result<(), String> {
    use grapt_permission::preset::{decide, Effect, Layer, Preset};
    let scratch = super::super::tests::Scratch::new()?;
    let root = path_text(&scratch.0);
    let registry = TerminalRegistry::default();
    let owner = session("ses_split_input")?;
    let id = operation("op_split_input")?;
    let entry = registry.reserve(owner.clone(), id.clone(), spec(&scratch.0, "bash", false)?)?;
    let layers = [
        Layer::owner(),
        Layer::preset_in(Preset::Maintainer, Some(&root)),
        Layer {
            name: grapt_permission::preset::LayerName::Config,
            rules: vec![
                grapt_permission::preset::Rule::new("shell", "*", Effect::Allow),
                grapt_permission::preset::Rule::destructive("shell", Effect::Ask, Some(&root)),
            ],
            floor: false,
            compose: grapt_permission::preset::Compose::Override,
        },
    ];
    // Each allowed piece is delivered; an asked one is refused.
    let send = |chars: &str| -> Result<Effect, String> {
        let input = BoundedToolInput::new(
            serde_json::to_vec(&serde_json::json!({"session_id":id.as_str(),"chars":chars}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let write = registry.prepare_stdin(&scratch.0, &owner, &input)?;
        let effect = decide(&write.action, &[&write.resource], &layers).effect;
        let EffectTarget::TerminalStdin(spec) = write.target else {
            return Err("stdin target".into());
        };
        if effect == Effect::Allow {
            entry.record_input(spec.line.as_ref(), true);
        }
        Ok(effect)
    };
    assert_eq!(send("rm -")?, Effect::Allow);
    assert_eq!(send("rf build\n")?, Effect::Ask);
    // The refused piece was never delivered: the line is still `rm -`.
    assert_eq!(send("r build\n")?, Effect::Ask);
    assert_eq!(send("x\n")?, Effect::Allow);
    assert_eq!(send("ls\n")?, Effect::Allow);
    // An escaped Enter continues the line,
    assert_eq!(send("r\\\n")?, Effect::Allow);
    assert_eq!(send("m -rf build\n")?, Effect::Ask);
    assert_eq!(send("\n")?, Effect::Allow);
    // and so does a quote left open.
    assert_eq!(send("echo '")?, Effect::Allow);
    // Read alone, a piece cannot be hidden by the quote left open before it,
    assert_eq!(send("rm -rf build\n")?, Effect::Ask);
    // and joined to that quote it reads as the shell would run it.
    assert_eq!(send("'\nrm -rf build\n")?, Effect::Ask);
    assert_eq!(send("'\n")?, Effect::Allow);
    assert_eq!(send("git reset \u{3}ls\n")?, Effect::Allow);
    // An editor can ignore Ctrl-D, joining what surrounds it.
    assert_eq!(send("rm -\u{4}rf build\n")?, Effect::Ask);
    assert_eq!(send("rmx\u{7f} -rf build\n")?, Effect::Ask);
    // An unfinished line too long to keep asks from then on.
    assert_eq!(send(&"a".repeat(40 * 1024))?, Effect::Allow);
    assert_eq!(send(&"b".repeat(40 * 1024))?, Effect::Allow);
    assert_eq!(send("c")?, Effect::Ask);
    assert_eq!(send("\nls\n")?, Effect::Ask);
    // Input delivered uncertainly, or never decided, leaves the line unknown.
    let unread = format!("\n{}", grapt_permission::command_safety::UNREAD_INPUT);
    let other = registry.reserve(
        owner.clone(),
        operation("op_uncertain_input")?,
        spec(&scratch.0, "bash", false)?,
    )?;
    let (section, line) = other.input_line("ls\n");
    assert_eq!(section, "");
    other.record_input(Some(&line), false);
    assert_eq!(other.input_line("ls\n").0, unread);
    let another = registry.reserve(
        owner.clone(),
        operation("op_undecided_input")?,
        spec(&scratch.0, "bash", false)?,
    )?;
    another.record_input(None, true);
    assert_eq!(another.input_line("ls\n").0, unread);
    Ok(())
}

/// Re-review probes against a real shell: neither an escaped Enter,
/// a refused piece nor a write decided before another was delivered can
/// complete a delete.
#[cfg(unix)]
#[tokio::test]
async fn a_real_shell_never_runs_a_delete_typed_around_its_decision() -> Result<(), String> {
    use grapt_permission::preset::{decide, Effect, Layer, Preset};
    let scratch = super::super::tests::Scratch::new()?;
    let victim = scratch.0.join("victim");
    std::fs::create_dir(&victim).map_err(|error| error.to_string())?;
    std::fs::write(victim.join("kept"), "kept").map_err(|error| error.to_string())?;
    let root = path_text(&scratch.0);
    let registry = TerminalRegistry::default();
    let owner = session("ses_split_shell")?;
    let id = operation("op_split_shell")?;
    let (_, delivery) = registry
        .exec(&owner, id.clone(), spec(&scratch.0, "bash", false)?)
        .await
        .map_err(failure)?;
    delivery.captured();
    let layers = [
        Layer::owner(),
        Layer::preset_in(Preset::Maintainer, Some(&root)),
        Layer {
            name: grapt_permission::preset::LayerName::Config,
            rules: vec![
                grapt_permission::preset::Rule::new("shell", "*", Effect::Allow),
                grapt_permission::preset::Rule::destructive("shell", Effect::Ask, Some(&root)),
            ],
            floor: false,
            compose: grapt_permission::preset::Compose::Override,
        },
    ];
    let prepare = |chars: &str| -> Result<(Effect, StdinSpec), String> {
        let input = BoundedToolInput::new(
            serde_json::to_vec(&serde_json::json!({"session_id":id.as_str(),"chars":chars}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let write = registry.prepare_stdin(&scratch.0, &owner, &input)?;
        let effect = decide(&write.action, &[&write.resource], &layers).effect;
        let EffectTarget::TerminalStdin(spec) = write.target else {
            return Err("stdin target".into());
        };
        Ok((effect, spec))
    };
    // Prepare, decide, and deliver only what is allowed.
    let mut typed = Vec::new();
    for chars in [
        "r\\\n",
        "m -rf victim\n",
        "\n",
        "rm -",
        "rf victim\n",
        "r victim\n",
        "x\n",
    ] {
        let (effect, spec) = prepare(chars)?;
        typed.push((chars, effect));
        if effect == Effect::Allow {
            let (_, delivery) = registry.stdin(&owner, &spec).await.map_err(failure)?;
            delivery.captured();
        }
    }
    assert_eq!(
        typed,
        [
            ("r\\\n", Effect::Allow),
            ("m -rf victim\n", Effect::Ask),
            ("\n", Effect::Allow),
            ("rm -", Effect::Allow),
            ("rf victim\n", Effect::Ask),
            ("r victim\n", Effect::Ask),
            ("x\n", Effect::Allow),
        ]
    );
    // Both decided against an empty line; the second is stale once the first
    // is delivered.
    let (first, piece) = prepare("rm -")?;
    let (second, rest) = prepare("rf victim\n")?;
    assert_eq!((first, second), (Effect::Allow, Effect::Allow));
    let (_, delivery) = registry.stdin(&owner, &piece).await.map_err(failure)?;
    delivery.captured();
    let stale = registry.stdin(&owner, &rest).await.err().map(failure);
    assert!(
        stale
            .as_deref()
            .is_some_and(|detail| detail.contains("changed")),
        "{stale:?}"
    );
    let (effect, spec) = prepare("x\ntest -f victim/kept && echo victim-alive\n")?;
    assert_eq!(effect, Effect::Allow);
    let (_, delivery) = registry.stdin(&owner, &spec).await.map_err(failure)?;
    delivery.captured();
    let entry = registry.lookup(&owner, &id)?;
    await_output(&entry, "victim-alive").await?;
    assert!(victim.join("kept").exists());
    let (_, spec) = prepare("exit\n")?;
    let (_, delivery) = registry.stdin(&owner, &spec).await.map_err(failure)?;
    delivery.captured();
    Ok(())
}
#[test]
fn closing_a_session_cannot_return_capacity_while_cleanup_is_unconfirmed() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_quarantine_owner")?;
    let id = operation("op_quarantine")?;
    let entry = registry.reserve(
        owner.clone(),
        id.clone(),
        spec(&scratch.0, "printf ok", false)?,
    )?;
    {
        // The root exited and its pipes drained, but a descendant's exit was
        // never confirmed: the settled cleanup failure.
        let mut data = lock(&entry.observation);
        data.exit = Some(ShellTerminal::Exited(0));
        data.pipe = PipeState::Drained;
        data.cleanup_error = Some("terminal job still had 1 active process(es)".into());
    }
    drop(entry);
    registry.close_session(&owner);
    assert!(
        registry.lookup(&owner, &id).is_err(),
        "the closed handle stayed reachable"
    );
    assert_eq!(registry.capacity.available_permits(), GLOBAL_RESOURCES - 1);
    // Every other Session together still cannot reclaim the held capacity.
    for index in 0..GLOBAL_RESOURCES - 1 {
        let other = session(&format!("ses_quarantine_{}", index / SESSION_RESOURCES))?;
        registry.reserve(
            other,
            operation(&format!("op_quarantine_{index}"))?,
            spec(&scratch.0, "printf ok", false)?,
        )?;
    }
    let refused = registry.reserve(
        session("ses_quarantine_last")?,
        operation("op_quarantine_last")?,
        spec(&scratch.0, "printf bad", false)?,
    );
    assert!(refused.is_err(), "unconfirmed cleanup returned capacity");
    assert_eq!(lock(&registry.entries).quarantine.len(), 1);
    // Once cleanup completes, the next reservation prunes it and capacity returns.
    {
        let entries = lock(&registry.entries);
        let mut data = lock(&entries.quarantine[0].observation);
        data.cleanup_error = None;
        data.cleanup = CleanupState::Complete;
        data.startup_settled = true;
    }
    registry.reserve(
        session("ses_quarantine_last")?,
        operation("op_quarantine_after")?,
        spec(&scratch.0, "printf ok", false)?,
    )?;
    assert!(lock(&registry.entries).quarantine.is_empty());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn closing_a_session_stops_its_background_process_and_forgets_the_handle(
) -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_close_terminal")?;
    let id = operation("op_close_terminal")?;
    let (_, delivery) = registry
        .exec(
            &owner,
            id.clone(),
            spec(&scratch.0, "/bin/sleep 1; printf late > late.txt", false)?,
        )
        .await
        .map_err(failure)?;
    delivery.captured();
    let entry = registry.lookup(&owner, &id)?;
    registry.close_session(&owner);
    assert!(registry.lookup(&owner, &id).is_err());
    let (output, _) = observe(&entry, 5_000, 40_000, Some(0)).await?;
    let output = json(&output)?;
    assert_eq!(output["status"], "cancelled");
    assert_eq!(output["done"], true);
    assert!(!scratch.0.join("late.txt").exists());
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn child_exit_with_a_held_stdout_reports_incomplete_cleanup_until_io_closes(
) -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_held_pipe")?;
    let id = operation("op_held_pipe")?;
    let (_, delivery) = registry
        .exec(
            &owner,
            id.clone(),
            spec(
                &scratch.0,
                "printf ready; IFS= read -r word; printf done",
                false,
            )?,
        )
        .await
        .map_err(failure)?;
    delivery.captured();
    let entry = registry.lookup(&owner, &id)?;
    let control = lock(&entry.control)
        .clone()
        .ok_or("process control absent")?;
    let held = lock(&control).hold_stdout_for_test()?;
    let request = StdinSpec {
        operation: id.clone(),
        chars: "finish\n".into(),
        wait_ms: 5_000,
        page_bytes: 40_000,
        cursor: Some(0),
        line: None,
    };
    let (output, delivery) = registry.stdin(&owner, &request).await.map_err(failure)?;
    let output = json(&output)?;
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["pipe"], "timed-out");
    assert_eq!(output["cleanup"], "incomplete");
    assert!(output["cleanup_error"]
        .as_str()
        .is_some_and(|error| error.contains("I/O cleanup is incomplete")));
    assert_eq!(output["done"], true);
    delivery.captured();
    drop(held);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let changed = entry.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let drained = {
                let data = lock(&entry.observation);
                data.readers == 0 && !data.writer_open
            };
            if drained {
                break;
            }
            changed.await;
        }
    })
    .await
    .map_err(|_| "fixture I/O did not close")?;
    let (output, delivery) = registry
        .stdin(
            &owner,
            &StdinSpec {
                chars: String::new(),
                ..request
            },
        )
        .await
        .map_err(failure)?;
    let output = json(&output)?;
    assert_eq!(output["cleanup"], "complete");
    assert_eq!(
        output["pipe"], "timed-out",
        "a missed drain deadline must remain visible"
    );
    assert!(output.get("cleanup_error").is_none());
    assert_eq!(output["output"], "readydone");
    delivery.captured();
    Ok(())
}

#[tokio::test]
async fn cancelled_reservation_cannot_start_a_later_process() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_cancelled_start")?;
    let entry = registry.reserve(
        owner.clone(),
        operation("op_cancelled_start")?,
        spec(&scratch.0, "exit 31", false)?,
    )?;
    registry.close_session(&owner);
    assert!(supervise::start(Arc::clone(&entry), entry.spec.clone()).is_err());
    assert!(
        lock(&entry.control).is_none(),
        "a cancelled reservation started a process"
    );
    assert!(lock(&entry.input).is_none());
    Ok(())
}

#[tokio::test]
async fn an_opaque_post_creation_failure_cannot_claim_no_effect_or_complete_cleanup(
) -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    for (ordinal, uncertain) in [false, true].into_iter().enumerate() {
        let entry = registry.reserve(
            session("ses_spawn_failure")?,
            operation(&format!("op_spawn_failure_{ordinal}"))?,
            spec(&scratch.0, "exit 0", false)?,
        )?;
        let failure = if uncertain {
            process::SpawnFailure::uncertain("post-creation watcher failed".into())
        } else {
            process::SpawnFailure::from("shell preflight failed")
        };
        let result = settle_spawn_failure(&entry, failure);
        let receipt =
            sandbox::receipt(&result.detail).ok_or("spawn failure lost its execution receipt")?;
        assert_eq!(
            receipt["ran"],
            if uncertain {
                serde_json::Value::Null
            } else {
                serde_json::json!(false)
            }
        );
        assert_eq!(
            matches!(result.reason, ToolFailureReason::EffectUnknown),
            uncertain
        );
        assert_eq!(entry.evictable(), !uncertain);
        let observation = lock(&entry.observation);
        assert_eq!(
            observation.cleanup(),
            if uncertain {
                CleanupState::Pending
            } else {
                CleanupState::Complete
            }
        );
        if uncertain {
            assert!(result.detail.contains("do not replay"));
            assert!(observation.cleanup_error.is_some());
        }
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn an_immediate_timeout_can_precede_the_first_output() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let mut request = spec(&scratch.0, "/bin/sleep 10; printf too-late", false)?;
    request.timeout_ms = 1;
    request.wait_ms = 5_000;
    let (bytes, delivery) = registry
        .exec(
            &session("ses_early_timeout")?,
            operation("op_early_timeout")?,
            request,
        )
        .await
        .map_err(failure)?;
    let result = json(&bytes)?;
    assert_eq!(result["status"], "timed_out");
    assert_eq!(result["done"], true);
    assert_eq!(result["output"], "");
    delivery.captured();
    Ok(())
}

#[test]
fn nul_command_refusal_keeps_literal_shell_escape_text_valid() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let input = |cmd: &str| {
        BoundedToolInput::new(
            serde_json::to_vec(&serde_json::json!({"cmd":cmd}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    };
    let invalid = arguments::decode_exec(
        &scratch.0,
        &input("echo before\0echo after")?,
        shell_environment::Environment::parent()?,
    );
    assert!(invalid.err().is_some_and(|error| error.contains("NUL")));
    let literal = r"printf '\0'";
    let prepared = arguments::decode_exec(
        &scratch.0,
        &input(literal)?,
        shell_environment::Environment::parent()?,
    )?;
    assert_eq!(prepared.command, literal);
    Ok(())
}

#[test]
fn cancelling_a_queued_spawn_releases_its_session_reservation() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let (release, held) = std::sync::mpsc::sync_channel::<()>(1);
        let ready = Arc::new(Notify::new());
        let worker_ready = Arc::clone(&ready);
        let blocker = tokio::task::spawn_blocking(move || {
            worker_ready.notify_one();
            held.recv_timeout(Duration::from_secs(30))
        });
        tokio::time::timeout(Duration::from_secs(5), ready.notified())
            .await
            .map_err(|_| "fixture blocking worker did not become ready")?;
        let scratch = super::super::tests::Scratch::new()?;
        let registry = Arc::new(TerminalRegistry::default());
        let owner = session("ses_queued_cancel")?;
        let id = operation("op_queued_cancel")?;
        let queued_registry = Arc::clone(&registry);
        let queued_owner = owner.clone();
        let queued_id = id.clone();
        let request = spec(&scratch.0, "exit 31", false)?;
        let queued = tokio::spawn(async move {
            queued_registry
                .exec(&queued_owner, queued_id, request)
                .await
        });
        let entry = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(entry) = registry.lookup(&owner, &id) {
                    break entry;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "fixture exec did not reserve its queued resource")?;
        queued.abort();
        if !queued.await.is_err_and(|error| error.is_cancelled()) {
            return Err("fixture exec completed before its queued startup was cancelled".into());
        }
        // Dropping the only sender also releases this worker on any error path.
        drop(release);
        let _released = blocker.await.map_err(|error| error.to_string())?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !entry.evictable() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "cancelled queued startup kept a session resource reserved")?;
        assert!(
            lock(&entry.control).is_none(),
            "cancelled startup created a process"
        );
        drop(entry);
        for index in 0..SESSION_RESOURCES {
            registry.reserve(
                owner.clone(),
                operation(&format!("op_after_cancel_{index}"))?,
                spec(&scratch.0, "exit 0", false)?,
            )?;
        }
        assert!(registry.lookup(&owner, &id).is_err());
        Ok(())
    })
}

#[cfg(unix)]
#[tokio::test]
async fn a_retry_cursor_past_the_observed_output_names_the_latest_returned_cursor(
) -> Result<(), String> {
    // The fixture emits exactly OUTPUT and nothing else, so waiting for the
    // whole string pins the observed total before any cursor is judged.
    const OUTPUT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let scratch = super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_cursor_reject")?;
    let id = operation("op_cursor_reject")?;
    let (_first, delivery) = registry
        .exec(
            &owner,
            id.clone(),
            spec(
                &scratch.0,
                &format!("printf '{OUTPUT}'; /bin/sleep 30"),
                false,
            )?,
        )
        .await
        .map_err(failure)?;
    await_output(registry.lookup(&owner, &id)?.as_ref(), OUTPUT).await?;
    delivery.captured();
    // One successful poll makes the latest returned cursor deterministic:
    // it is the full fixture total whether the first response captured the
    // output or not.
    let (polled, second) = registry
        .stdin(
            &owner,
            &StdinSpec {
                operation: id.clone(),
                chars: String::new(),
                wait_ms: 100,
                page_bytes: 40_000,
                cursor: None,
                line: None,
            },
        )
        .await
        .map_err(failure)?;
    let returned = json(&polled)?["output_cursor"]
        .as_u64()
        .ok_or("poll response has no output_cursor")?;
    assert_eq!(
        returned,
        OUTPUT.len() as u64,
        "fixture output must be fully captured"
    );
    second.captured();
    let rejected = returned + 4;
    let error = registry
        .stdin(
            &owner,
            &StdinSpec {
                operation: id,
                chars: String::new(),
                wait_ms: 5_000,
                page_bytes: 40_000,
                cursor: Some(rejected),
                line: None,
            },
        )
        .await
        .err()
        .ok_or("a past-the-end cursor was accepted")?
        .detail;
    assert_eq!(
        error,
        format!(
            "cursor {rejected} is past the observed output; pass the latest returned output_cursor ({returned}) unchanged."
        )
    );
    Ok(())
}

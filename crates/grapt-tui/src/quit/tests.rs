use super::*;
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The UI, its backend handle, the command intake, and the loop's wake: a
/// live worker holds both receivers.
fn fixture() -> (Ui, Backend, crate::Commands, mpsc::Receiver<()>) {
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, ready) = mpsc::sync_channel(1);
    (
        Ui::new(false),
        Backend {
            commands,
            updates,
            wake,
        },
        received,
        ready,
    )
}

fn ctrl_c() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
}

#[test]
fn first_control_c_interrupts_running_work_even_behind_a_picker() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    ui.session_update(None, true);
    ui.picker = Some(crate::sessions::Picker::new(&[]));
    ui.composer.insert_str("keep this draft");
    assert_eq!(
        key(ctrl_c(), &mut ui, &backend, Duration::ZERO),
        Some(false)
    );
    assert!(matches!(received.try_recv()?, Command::Interrupt(_)));
    assert_eq!(keys::draft(&ui), "keep this draft");
    assert_eq!(ui.quit.hint(), Some(HINT));
    assert!(!ui.quit.waiting());
    Ok(())
}

#[test]
fn pending_submission_is_interrupted_before_running_status_arrives() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    ui.pending = Some("submitted before status".into());
    assert_eq!(
        key(ctrl_c(), &mut ui, &backend, Duration::ZERO),
        Some(false)
    );
    assert!(matches!(received.try_recv()?, Command::Interrupt(_)));
    assert_eq!(ui.pending.as_deref(), Some("submitted before status"));
    Ok(())
}

#[test]
fn idle_control_c_clears_multiline_text_without_submitting_and_keeps_undo() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    ui.composer.insert_str("keep 中文\nsecond line");
    assert_eq!(
        key(ctrl_c(), &mut ui, &backend, Duration::ZERO),
        Some(false)
    );
    assert!(keys::draft(&ui).is_empty());
    assert!(received.try_recv().is_err());
    assert!(ui.composer.undo());
    assert_eq!(keys::draft(&ui), "keep 中文\nsecond line");
    Ok(())
}

#[test]
fn idle_empty_hint_expires_and_only_a_fresh_second_press_confirms() {
    let (mut ui, backend, _, _ready) = fixture();
    assert_eq!(
        key(ctrl_c(), &mut ui, &backend, Duration::ZERO),
        Some(false)
    );
    assert_eq!(ui.quit.hint(), Some(HINT));
    assert!(!ui.quit.tick(Duration::from_millis(1499)));
    assert!(ui.quit.tick(CONFIRM));
    assert_eq!(ui.quit.hint(), None);
    assert_eq!(key(ctrl_c(), &mut ui, &backend, CONFIRM), Some(false));
    assert_eq!(
        key(
            ctrl_c(),
            &mut ui,
            &backend,
            CONFIRM + Duration::from_millis(1499)
        ),
        Some(true)
    );
}

#[test]
fn key_repeats_never_quit_and_other_keys_disarm_confirmation() {
    let (mut ui, backend, _, _ready) = fixture();
    key(ctrl_c(), &mut ui, &backend, Duration::ZERO);
    let mut repeated = ctrl_c();
    repeated.kind = KeyEventKind::Repeat;
    assert_eq!(
        key(repeated, &mut ui, &backend, Duration::from_millis(10)),
        Some(false)
    );
    assert_eq!(
        key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut ui,
            &backend,
            Duration::from_millis(20)
        ),
        None
    );
    assert_eq!(ui.quit.hint(), None);
    assert_eq!(
        key(ctrl_c(), &mut ui, &backend, Duration::from_millis(30)),
        Some(false)
    );
}

#[test]
fn quit_waits_for_the_interrupt_acknowledgement_and_blocks_new_input() -> TestResult {
    let (mut ui, backend, _received, _ready) = fixture();
    ui.quit.request(&backend, Duration::ZERO)?;
    let Some(reply) = backend.updates.take_quits().pop() else {
        return Err("missing acknowledged quit".into());
    };
    assert_eq!(ui.quit.poll(&backend, Duration::from_millis(1)), None);
    assert_eq!(
        key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
            Duration::from_millis(2)
        ),
        Some(false)
    );
    assert!(ui.quit.waiting());
    reply.try_send(Ok(()))?;
    assert_eq!(
        ui.quit.poll(&backend, Duration::from_millis(3)),
        Some(Ok(()))
    );
    assert!(!ui.quit.waiting());
    Ok(())
}

#[test]
fn failed_lost_and_timed_out_stop_acknowledgements_keep_the_terminal_open() -> TestResult {
    let (mut ui, backend, _received, _ready) = fixture();
    ui.quit.request(&backend, Duration::ZERO)?;
    let Some(reply) = backend.updates.take_quits().pop() else {
        return Err("missing quit".into());
    };
    reply.try_send(Err("service unavailable".into()))?;
    assert_eq!(
        ui.quit.poll(&backend, Duration::ZERO),
        Some(Err("service unavailable".into()))
    );
    ui.quit.request(&backend, Duration::ZERO)?;
    let Some(reply) = backend.updates.take_quits().pop() else {
        return Err("missing quit".into());
    };
    drop(reply);
    assert!(matches!(
        ui.quit.poll(&backend, Duration::ZERO),
        Some(Err(_))
    ));
    ui.quit.request(&backend, Duration::ZERO)?;
    let Some(_reply) = backend.updates.take_quits().pop() else {
        return Err("missing quit".into());
    };
    assert!(matches!(ui.quit.poll(&backend, STOP_TIMEOUT), Some(Err(_))));
    assert!(!ui.quit.waiting());
    Ok(())
}

#[test]
fn other_control_chords_do_not_exit_or_detach() -> TestResult {
    for character in ['b', 'd', 'q', 'z', '\\'] {
        let (mut ui, backend, _received, _ready) = fixture();
        let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL);
        assert_eq!(key(event, &mut ui, &backend, Duration::ZERO), None);
        assert!(!keys::handle_key(event, &mut ui, &backend)?);
    }
    Ok(())
}

#[test]
fn quit_behind_a_command_burst_is_accepted_once_and_arms_its_wait() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    crate::burst(&backend)?;
    ui.quit.request(&backend, Duration::ZERO)?;
    assert!(ui.quit.waiting());
    assert!(crate::after_burst(&received)?.is_empty());
    assert_eq!(backend.updates.take_quits().len(), 1);
    Ok(())
}

#[test]
fn repeated_console_breaks_never_confirm_a_quit() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    ui.session_update(None, true);
    key(ctrl_c(), &mut ui, &backend, Duration::ZERO);
    for milliseconds in [10, 20] {
        console_interrupt(&mut ui, &backend, Duration::from_millis(milliseconds));
        assert!(!ui.quit.waiting());
        assert!(!ui.quit.closing());
    }
    for _ in 0..3 {
        assert!(matches!(received.try_recv()?, Command::Interrupt(_)));
    }
    assert!(received.try_recv().is_err());
    Ok(())
}

#[test]
fn closing_console_uses_normal_interrupt_ack_and_blocks_new_work() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    ui.quit.close_console(&backend, Duration::ZERO);
    assert!(ui.quit.waiting());
    assert!(ui.quit.closing());
    // The request precedes any further console input or drawing.
    let Some(reply) = backend.updates.take_quits().pop() else {
        return Err("closing console did not request normal interrupt".into());
    };
    assert_eq!(ui.quit.poll(&backend, Duration::ZERO), None);
    assert_eq!(
        key(ctrl_c(), &mut ui, &backend, Duration::ZERO),
        Some(false)
    );
    assert!(received.try_recv().is_err());
    reply.try_send(Ok(()))?;
    assert_eq!(
        ui.quit.poll(&backend, Duration::from_millis(1)),
        Some(Ok(()))
    );
    Ok(())
}

#[test]
fn console_close_deadline_is_not_extended_by_repeated_signals() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    ui.quit.close_console(&backend, Duration::ZERO);
    assert_eq!(ui.quit.poll(&backend, Duration::ZERO), None);
    let Some(_reply) = backend.updates.take_quits().pop() else {
        return Err("missing console close interrupt".into());
    };
    ui.quit
        .close_console(&backend, CONSOLE_GRACE - Duration::from_millis(1));
    assert_eq!(
        ui.quit
            .poll(&backend, CONSOLE_GRACE - Duration::from_millis(1)),
        None
    );
    assert!(matches!(
        ui.quit.poll(&backend, CONSOLE_GRACE),
        Some(Err(_))
    ));
    assert!(ui.quit.closing());
    assert!(received.try_recv().is_err());
    Ok(())
}

#[test]
fn console_close_exits_with_an_error_on_rejected_or_disconnected_ack() -> TestResult {
    for disconnect in [false, true] {
        let (mut ui, backend, _received, _ready) = fixture();
        ui.quit.close_console(&backend, Duration::ZERO);
        assert_eq!(ui.quit.poll(&backend, Duration::ZERO), None);
        let Some(reply) = backend.updates.take_quits().pop() else {
            return Err("missing console close interrupt".into());
        };
        if !disconnect {
            reply.try_send(Err("service unavailable".into()))?;
        }
        drop(reply);
        assert!(matches!(
            ui.quit.poll(&backend, Duration::ZERO),
            Some(Err(_))
        ));
        assert!(ui.quit.closing());
    }
    Ok(())
}

#[test]
fn console_close_retries_queue_pressure_without_resending_an_accepted_request() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    for _ in 0..16 {
        crate::send(&backend, Command::Models)?;
    }
    ui.quit.close_console(&backend, Duration::ZERO);
    assert_eq!(ui.quit.poll(&backend, Duration::ZERO), None);
    assert!(ui.quit.waiting());
    for _ in 0..16 {
        assert!(matches!(received.try_recv()?, Command::Models));
    }
    assert_eq!(ui.quit.poll(&backend, Duration::from_millis(1)), None);
    let Some(reply) = backend.updates.take_quits().pop() else {
        return Err("queue relief did not admit console interrupt".into());
    };
    assert_eq!(ui.quit.poll(&backend, Duration::from_millis(2)), None);
    assert!(received.try_recv().is_err());
    reply.try_send(Ok(()))?;
    assert_eq!(
        ui.quit.poll(&backend, Duration::from_millis(3)),
        Some(Ok(()))
    );
    Ok(())
}

#[test]
fn permanently_full_console_queue_has_a_bounded_exit() -> TestResult {
    let (mut ui, backend, _received, _ready) = fixture();
    for _ in 0..16 {
        crate::send(&backend, Command::Models)?;
    }
    ui.quit.close_console(&backend, Duration::ZERO);
    assert_eq!(ui.quit.poll(&backend, Duration::ZERO), None);
    assert!(matches!(
        ui.quit.poll(&backend, CONSOLE_GRACE),
        Some(Err(_))
    ));
    Ok(())
}

#[test]
fn console_close_bounds_an_already_pending_manual_quit_without_resending() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    ui.quit.request(&backend, Duration::ZERO)?;
    let Some(_reply) = backend.updates.take_quits().pop() else {
        return Err("missing manual quit".into());
    };
    ui.quit.close_console(&backend, Duration::from_secs(1));
    assert_eq!(ui.quit.poll(&backend, Duration::from_secs(1)), None);
    assert!(received.try_recv().is_err());
    assert!(matches!(
        ui.quit
            .poll(&backend, Duration::from_secs(1) + CONSOLE_GRACE),
        Some(Err(_))
    ));
    Ok(())
}

#[test]
fn disconnected_worker_allows_local_exit_only_after_unattached_startup_failure() -> TestResult {
    let (mut ui, backend, received, ready) = fixture();
    drop(received);
    drop(ready);
    let error = send(&backend, Command::Models)
        .err()
        .ok_or("dead worker accepted command")?;
    assert!(error.contains("stopped"));
    assert!(!error.contains("busy"));
    // A dead worker alone cannot establish that an active Session was stopped.
    ui.quit.backend_failed(false);
    assert!(ui.quit.request(&backend, Duration::ZERO).is_err());
    assert!(!ui.quit.waiting());
    ui.quit.backend_failed(true);
    assert_eq!(
        key(ctrl_c(), &mut ui, &backend, Duration::ZERO),
        Some(false)
    );
    assert_eq!(
        key(ctrl_c(), &mut ui, &backend, Duration::from_millis(1)),
        Some(true)
    );
    ui.quit.request(&backend, Duration::from_millis(1))?;
    assert_eq!(
        ui.quit.poll(&backend, Duration::from_millis(1)),
        Some(Ok(()))
    );
    // The command path uses the same local request without a double-press.
    ui.quit.request(&backend, Duration::from_millis(2))?;
    assert_eq!(
        ui.quit.poll(&backend, Duration::from_millis(2)),
        Some(Ok(()))
    );
    Ok(())
}

#[test]
fn a_queued_draft_waits_while_quit_does_and_resumes_when_it_is_refused() -> TestResult {
    let (mut ui, backend, received, _ready) = fixture();
    ui.pending = Some("in flight".into());
    crate::sending::enter(&mut ui, &backend, "queued behind it".into());
    ui.quit.request(&backend, Duration::ZERO)?;
    let Some(reply) = backend.updates.take_quits().pop() else {
        return Err("missing quit".into());
    };
    ui.pending = None;
    assert!(!crate::sending::advance(&mut ui, &backend));
    assert!(
        received.try_recv().is_err(),
        "a queued draft was sent while quitting"
    );
    reply.try_send(Err("stop refused".into()))?;
    assert!(matches!(
        ui.quit.poll(&backend, Duration::from_millis(1)),
        Some(Err(_))
    ));
    assert!(crate::sending::advance(&mut ui, &backend));
    assert!(matches!(received.try_recv()?, Command::Submit(text) if text == "queued behind it"));
    Ok(())
}

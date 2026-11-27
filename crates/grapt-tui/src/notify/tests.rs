use super::*;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn notifier() -> Notifier {
    let mut notifier = Notifier::new(true);
    notifier.method = Method::Bel;
    notifier.tmux = false;
    notifier
}

fn question(id: &str, owner: FormOwner) -> Result<FormRow> {
    Ok(FormRow {
        id: grapt_client::wire::form_id(id)?,
        owner,
        title: "Question".into(),
        fields: Vec::new(),
        metadata: Default::default(),
    })
}

fn approval(id: &str, session: &SessionWireId) -> Result<PermissionAsk> {
    Ok(PermissionAsk {
        session_id: session.clone(),
        request_id: grapt_client::wire::permission_id(id)?,
        title: "Approval".into(),
        subtitle: "tool".into(),
        live: true,
        allow_always: true,
        call: None,
        preview: None,
    })
}

#[test]
fn unknown_activity_preserves_the_same_turn_but_never_crosses_sessions() -> Result {
    let first = grapt_client::wire::session_id("ses_notify_one")?;
    let second = grapt_client::wire::session_id("ses_notify_two")?;
    let mut edge = TurnEdge::default();
    assert!(!edge.observe(Some(first.clone()), Some(true)));
    assert!(!edge.observe(Some(first.clone()), None));
    assert!(edge.observe(Some(first.clone()), Some(false)));
    assert!(!edge.observe(Some(first), Some(true)));
    assert!(!edge.observe(Some(second.clone()), None));
    assert!(!edge.observe(Some(second), Some(false)));
    Ok(())
}

#[test]
fn completion_uses_focus_and_input_idle_at_the_edge_without_a_later_alert() -> Result {
    let selected = grapt_client::wire::session_id("ses_notify_one")?;
    let mut notifier = notifier();
    let observe = |notifier: &mut Notifier, running, seconds| {
        notifier.lines(
            Some(&selected),
            Some(running),
            None,
            &[],
            Duration::from_secs(seconds),
        )
    };
    assert_eq!(observe(&mut notifier, true, 0), [None; 3]);
    assert_eq!(observe(&mut notifier, false, 1), [None; 3]);
    assert_eq!(
        observe(&mut notifier, false, 10),
        [None; 3],
        "becoming idle later must not replay a completed turn"
    );
    notifier.input(&crate::record_input::focus(false), Duration::from_secs(10));
    assert_eq!(observe(&mut notifier, true, 10), [None; 3]);
    assert_eq!(
        observe(&mut notifier, false, 11),
        [Some(TURN_COMPLETE), None, None]
    );
    notifier.input(&crate::record_input::focus(true), Duration::from_secs(12));
    observe(&mut notifier, true, 12);
    notifier.input(&Event::Paste("typing".into()), Duration::from_secs(13));
    assert_eq!(observe(&mut notifier, false, 17), [None; 3]);
    observe(&mut notifier, true, 18);
    assert_eq!(
        observe(&mut notifier, false, 19),
        [Some(TURN_COMPLETE), None, None]
    );
    Ok(())
}

#[test]
fn waiting_requests_alert_while_active_and_repeat_only_for_new_keys() -> Result {
    let selected = grapt_client::wire::session_id("ses_notify_one")?;
    let mut notifier = notifier();
    let mut ask = approval("per_notify_one", &selected)?;
    let first = question("frm_notify_one", FormOwner::Session(selected.clone()))?;
    let second = question("frm_notify_two", FormOwner::Global)?;
    let now = Duration::ZERO;
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            Some(&ask),
            std::slice::from_ref(&first),
            now
        ),
        [None, Some(APPROVAL_WAITING), Some(QUESTION_WAITING)],
    );
    ask.title = "Updated preview".into();
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            Some(&ask),
            std::slice::from_ref(&first),
            now
        ),
        [None; 3],
    );
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            Some(&ask),
            &[first, second],
            now
        ),
        [None, None, Some(QUESTION_WAITING)],
    );
    ask.request_id = grapt_client::wire::permission_id("per_notify_two")?;
    assert_eq!(
        notifier.lines(Some(&selected), Some(false), Some(&ask), &[], now),
        [None, Some(APPROVAL_WAITING), None],
    );
    Ok(())
}

#[test]
fn visible_foreign_approval_alerts_once_even_across_transcript_switches() -> Result {
    let selected = grapt_client::wire::session_id("ses_notify_one")?;
    let other = grapt_client::wire::session_id("ses_notify_two")?;
    let ask = approval("per_notify_one", &other)?;
    let mut notifier = notifier();
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            Some(&ask),
            &[],
            Duration::ZERO
        ),
        [None, Some(APPROVAL_WAITING), None],
    );
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            Some(&ask),
            &[],
            Duration::ZERO
        ),
        [None; 3],
    );
    assert_eq!(
        notifier.lines(Some(&other), Some(false), Some(&ask), &[], Duration::ZERO),
        [None; 3],
    );
    Ok(())
}

#[test]
fn foreign_questions_and_settled_approvals_do_not_alert_and_keys_are_bounded() -> Result {
    let selected = grapt_client::wire::session_id("ses_notify_one")?;
    let other = grapt_client::wire::session_id("ses_notify_two")?;
    let mut notifier = notifier();
    let mut ask = approval("per_notify_one", &other)?;
    ask.live = false;
    let foreign = question("frm_foreign", FormOwner::Session(other))?;
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            Some(&ask),
            &[foreign],
            Duration::ZERO
        ),
        [None; 3],
    );
    ask.session_id = selected.clone();
    ask.live = false;
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            Some(&ask),
            &[],
            Duration::ZERO
        ),
        [None; 3]
    );
    let questions: Vec<_> = (0..32)
        .map(|index| question(&format!("frm_notify_{index}"), FormOwner::Global))
        .collect::<Result<_>>()?;
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            None,
            &questions,
            Duration::ZERO
        ),
        [None, None, Some(QUESTION_WAITING)],
    );
    assert_eq!(notifier.questions.len(), grapt_tui_forms::MAX_FORMS);
    assert_eq!(
        notifier.lines(
            Some(&selected),
            Some(false),
            None,
            &questions,
            Duration::ZERO
        ),
        [None; 3]
    );
    Ok(())
}

#[test]
fn actual_writer_honors_disable_transport_and_failure_shutdown() -> Result {
    let mut notifier = notifier();
    let mut output = Vec::new();
    notifier.emit_to(APPROVAL_WAITING, &mut output)?;
    assert_eq!(output, b"\x07");
    notifier.method = Method::Osc9;
    notifier.tmux = true;
    output.clear();
    notifier.emit_to(QUESTION_WAITING, &mut output)?;
    assert_eq!(
        output,
        b"\x1bPtmux;\x1b\x1b]9;Grapt: question waiting\x07\x1b\\"
    );
    notifier.enabled = false;
    output.clear();
    notifier.emit_to(TURN_COMPLETE, &mut output)?;
    assert!(output.is_empty());

    struct Failed(usize);
    impl Write for Failed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.0 += 1;
            Err(std::io::Error::other("terminal unavailable"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    notifier.enabled = true;
    let mut failed = Failed(0);
    assert!(notifier.emit_to(TURN_COMPLETE, &mut failed).is_err());
    notifier.emit_to(TURN_COMPLETE, &mut failed)?;
    assert_eq!(failed.0, 1, "a failed terminal is not retried");
    Ok(())
}

#[test]
fn osc9_carries_the_line_and_bel_carries_nothing() {
    assert_eq!(bytes(Method::Osc9, "hello"), b"\x1b]9;hello\x07");
    assert_eq!(bytes(Method::Bel, "hello"), b"\x07");
}

#[test]
fn tmux_envelope_wraps_osc9_and_doubles_escapes() {
    let payload = bytes(Method::Osc9, "done");
    assert_eq!(
        tmux_passthrough(&payload),
        b"\x1bPtmux;\x1b\x1b]9;done\x07\x1b\\"
    );
    let hostile = b"\x1b]9;danger\x1b[31m\x07";
    assert_eq!(
        tmux_passthrough(hostile),
        b"\x1bPtmux;\x1b\x1b]9;danger\x1b\x1b[31m\x07\x1b\\"
    );
}

#[test]
fn a_notification_is_due_only_when_the_user_is_not_looking() {
    assert!(due(false, false));
    assert!(due(true, true));
    assert!(due(false, true));
    assert!(!due(true, false));
}

#[test]
fn osc9_is_trusted_only_for_the_named_terminals() {
    for program in ["Ghostty", "iTerm.app", "WezTerm", "WarpTerminal", "kitty"] {
        assert!(
            osc9_for(program, false, false, false, false),
            "{program} should trust OSC 9"
        );
    }
    assert!(osc9_for("tmux", true, false, false, false));
    assert!(osc9_for("tmux", false, true, false, false));
    assert!(osc9_for("tmux", false, false, true, false));
    assert!(osc9_for("tmux", false, false, false, true));
    for program in [
        "vscode",
        "Apple_Terminal",
        "VTE_6800",
        "WezTerm-Mux",
        "xterm",
        "",
    ] {
        assert!(
            !osc9_for(program, false, false, false, false),
            "{program} should fall back to the bell"
        );
    }
    assert!(!osc9_for("tmux", false, false, false, false));
}

fn session(raw: &str) -> SessionWireId {
    SessionWireId::parse(raw.to_owned())
        .unwrap_or_else(|| panic!("witness session id `{raw}` must parse"))
}

#[test]
fn only_the_same_session_running_to_idle_is_a_completion() {
    let first = session("ses_notify_one");
    let second = session("ses_notify_two");
    let mut edge = TurnEdge::default();
    assert!(
        !edge.observe(None, Some(false)),
        "a cold start is not a completion"
    );
    assert!(
        !edge.observe(Some(first.clone()), Some(true)),
        "starting is not a completion"
    );
    assert!(
        edge.observe(Some(first.clone()), Some(false)),
        "same session ran to idle"
    );
    assert!(
        !edge.observe(Some(first.clone()), Some(false)),
        "a steady idle repeats nothing"
    );
    assert!(
        !edge.observe(Some(second.clone()), Some(false)),
        "switching to an idle session is not a completion"
    );
    assert!(
        !edge.observe(Some(first.clone()), Some(true)),
        "starting again is not a completion"
    );
    assert!(
        edge.observe(Some(first.clone()), Some(false)),
        "the second turn completed"
    );
}
#[test]
fn closing_console_never_touches_notification_restore_or_error_sinks() {
    use grapt_console::{Access, Restore};
    use std::{cell::Cell, io};

    struct Sink<'a>(&'a Cell<usize>);
    impl io::Write for Sink<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.set(self.0.get() + 1);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
    }

    let access = Access::default();
    let touches = Cell::new(0);
    let opens = Cell::new(0);
    let restore = Restore::new(&access, Sink(&touches));
    let mut notifier = super::Notifier::new(true);
    access.close();
    access.with_output(|| {
        let _ = notifier.emit_to(super::TURN_COMPLETE, &mut Sink(&touches));
    });
    drop(restore);
    access.report(
        || {
            opens.set(opens.get() + 1);
            Sink(&touches)
        },
        "session stop acknowledgement timed out",
    );
    assert_eq!(opens.get(), 0, "closing must not even open the error sink");
    assert_eq!(touches.get(), 0, "no console write or flush after close");
}

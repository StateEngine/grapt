use super::*;
use std::sync::mpsc;
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn setup() -> (Ui, Backend, crate::Commands) {
    let mut ui = Ui::new(false);
    ui.prompt_ready = true;
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    (
        ui,
        Backend {
            commands,
            updates,
            wake,
        },
        received,
    )
}

fn observe(
    ui: &mut Ui,
    session: &str,
    message: Option<&str>,
    outcome: Outcome,
    running: bool,
) -> TestResult {
    let session = grapt_client::wire::session_id(session)?;
    let completion = message
        .map(|id| {
            MessageWireId::parse(id.to_owned())
                .ok_or("message")
                .map(|id| (id, outcome))
        })
        .transpose()?;
    ui.session_update(Some(session.clone()), running);
    ui.followups
        .observe(Some(session), completion, true, Some(running));
    Ok(())
}

fn key(ui: &mut Ui, backend: &Backend, code: KeyCode, modifiers: KeyModifiers) -> TestResult {
    crate::keys::handle_key(KeyEvent::new(code, modifiers), ui, backend)?;
    Ok(())
}

#[test]
fn pending_count_tracks_selected_unsent_followups_through_custody() -> TestResult {
    let (mut ui, backend, received) = setup();
    observe(&mut ui, "ses_a", None, Outcome::Succeeded, true)?;
    ui.composer.insert_str("follow up");
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    assert_eq!(ui.followups.pending_count(), 1);
    observe(&mut ui, "ses_b", None, Outcome::Succeeded, false)?;
    assert_eq!(ui.followups.pending_count(), 0);
    observe(&mut ui, "ses_a", None, Outcome::Succeeded, true)?;
    assert_eq!(ui.followups.pending_count(), 1);
    // Restore and requeue the paused draft through the ordinary editing keys.
    key(&mut ui, &backend, KeyCode::Up, KeyModifiers::ALT)?;
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    observe(
        &mut ui,
        "ses_a",
        Some("msg_done"),
        Outcome::Succeeded,
        false,
    )?;
    assert!(advance(&mut ui, &backend));
    let (sent, Command::Followup(_)) = received.try_envelope()? else {
        return Err("not a follow-up".into());
    };
    assert_eq!(ui.followups.pending_count(), 0);
    assert!(ui.followups.acknowledge(sent, Custody::Unconfirmed));
    assert_eq!(ui.followups.pending_count(), 0);
    assert!(ui.followups.acknowledge(sent, Custody::Refused));
    assert_eq!(ui.followups.pending_count(), 1);
    assert!(ui.followups.acknowledge(sent, Custody::Accepted));
    assert_eq!(ui.followups.pending_count(), 0);
    Ok(())
}

#[test]
fn keys_keep_followups_editable_and_release_one_after_each_completion() -> TestResult {
    let (mut ui, backend, received) = setup();
    observe(&mut ui, "ses_a", None, Outcome::Succeeded, true)?;
    ui.composer.insert_str("first");
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    assert!(keys::draft(&ui).is_empty());
    assert!(
        received.try_recv().is_err(),
        "queueing must neither submit nor interrupt"
    );
    ui.composer.insert_str("discard");
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    key(&mut ui, &backend, KeyCode::Delete, KeyModifiers::ALT)?;
    key(&mut ui, &backend, KeyCode::Up, KeyModifiers::ALT)?;
    assert_eq!(keys::draft(&ui), "first");
    ui.composer.insert_str(" edited");
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    ui.composer.insert_str("later");
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    ui.composer.insert_str("new human draft");
    observe(&mut ui, "ses_a", Some("msg_old"), Outcome::Failed, true)?;
    assert!(!advance(&mut ui, &backend));
    observe(
        &mut ui,
        "ses_a",
        Some("msg_first"),
        Outcome::Succeeded,
        false,
    )?;
    assert!(advance(&mut ui, &backend));
    let (sent, Command::Followup(request)) = received.try_envelope()? else {
        return Err("not a follow-up".into());
    };
    assert_eq!(request.text, "first edited");
    assert_eq!(request.session, grapt_client::wire::session_id("ses_a")?);
    assert!(ui.followups.acknowledge(sent, Custody::Unconfirmed));
    assert!(
        !advance(&mut ui, &backend),
        "uncertain custody must not replay automatically"
    );
    assert!(ui.followups.acknowledge(sent, Custody::Accepted));
    observe(
        &mut ui,
        "ses_a",
        Some("msg_first"),
        Outcome::Succeeded,
        false,
    )?;
    assert!(
        !advance(&mut ui, &backend),
        "duplicate idle must not drain the next prompt"
    );
    assert_eq!(keys::draft(&ui), "new human draft");
    // The next execution can finish before the UI sees its running phase.
    observe(
        &mut ui,
        "ses_a",
        Some("msg_next"),
        Outcome::Succeeded,
        false,
    )?;
    assert!(advance(&mut ui, &backend));
    let (sent, Command::Followup(request)) = received.try_envelope()? else {
        return Err("not a follow-up".into());
    };
    assert_eq!(request.text, "later");
    ui.followups.acknowledge(sent, Custody::Accepted);
    key(&mut ui, &backend, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(matches!(received.try_recv()?, Command::Submit(text) if text == "new human draft"));
    Ok(())
}

#[test]
fn stopped_or_backgrounded_followups_restore_images_without_overwriting_edits() -> TestResult {
    let (mut ui, backend, received) = setup();
    observe(&mut ui, "ses_a", None, Outcome::Succeeded, true)?;
    let file: PromptInputFileAttachmentDto = serde_json::from_value(serde_json::json!({
        "uri":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/6X8AAAAASUVORK5CYII=", "name":"pixel.png"
    }))?;
    ui.images.restore(vec![file.clone()])?;
    ui.composer.insert_str("image task");
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    assert_eq!(ui.images.len(), 0);
    ui.composer.insert_str("keep this");
    key(&mut ui, &backend, KeyCode::Up, KeyModifiers::ALT)?;
    assert_eq!(keys::draft(&ui), "keep this");
    observe(
        &mut ui,
        "ses_b",
        Some("msg_other"),
        Outcome::Succeeded,
        false,
    )?;
    assert!(!advance(&mut ui, &backend));
    observe(
        &mut ui,
        "ses_a",
        Some("msg_first"),
        Outcome::Succeeded,
        false,
    )?;
    assert!(
        !advance(&mut ui, &backend),
        "leaving a Session pauses its local queue"
    );
    ui.recall
        .replace_if_unchanged("keep this", "", &mut ui.composer);
    key(&mut ui, &backend, KeyCode::Up, KeyModifiers::ALT)?;
    assert_eq!(keys::draft(&ui), "image task");
    let restored = ui.images.submission(keys::draft(&ui))?;
    assert_eq!(restored.files, vec![file]);
    observe(
        &mut ui,
        "ses_a",
        Some("msg_first"),
        Outcome::Succeeded,
        true,
    )?;
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    key(&mut ui, &backend, KeyCode::Esc, KeyModifiers::NONE)?;
    assert!(matches!(
        received.try_recv()?,
        Command::InterruptAndResume(_)
    ));
    observe(
        &mut ui,
        "ses_a",
        Some("msg_stopped"),
        Outcome::Interrupted,
        false,
    )?;
    assert!(!advance(&mut ui, &backend));
    key(&mut ui, &backend, KeyCode::Up, KeyModifiers::ALT)?;
    assert_eq!(keys::draft(&ui), "image task");
    assert_eq!(ui.images.len(), 1);
    assert!(received.try_recv().is_err());
    Ok(())
}

#[test]
fn review_ids_unsent_edits_and_removals_never_send_or_accept_a_result() -> TestResult {
    for operation in [KeyCode::Up, KeyCode::Delete] {
        let (mut ui, backend, received) = setup();
        observe(&mut ui, "ses_ids", None, Outcome::Succeeded, true)?;
        ui.composer.insert_str("unsent draft");
        key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
        assert!(!ui
            .followups
            .acknowledge(crate::backend::CommandId::TEST, Custody::Accepted));
        key(&mut ui, &backend, operation, KeyModifiers::ALT)?;
        assert_eq!(ui.followups.pending_count(), 0);
        if operation == KeyCode::Up {
            assert_eq!(keys::draft(&ui), "unsent draft");
        }
        observe(
            &mut ui,
            "ses_ids",
            Some("msg_ids_done"),
            Outcome::Succeeded,
            false,
        )?;
        assert!(!advance(&mut ui, &backend));
        assert!(received.try_envelope().is_err());
        assert!(backend.updates.take().is_none());
    }
    Ok(())
}

#[test]
fn review_ids_followup_custody_only_matches_its_send_identity() -> TestResult {
    let (mut ui, backend, received) = setup();
    observe(&mut ui, "ses_ids", None, Outcome::Succeeded, true)?;
    ui.composer.insert_str("owned draft");
    key(&mut ui, &backend, KeyCode::Tab, KeyModifiers::NONE)?;
    observe(
        &mut ui,
        "ses_ids",
        Some("msg_ids_done"),
        Outcome::Succeeded,
        false,
    )?;
    assert!(advance(&mut ui, &backend));
    let (owner, Command::Followup(_)) = received.try_envelope()? else {
        return Err("expected follow-up".into());
    };
    let other = crate::send_tracked(&backend, Command::Submit("/status".into()))?;
    assert_ne!(owner, other);
    let revision = ui.followups.revision();
    assert!(!ui.followups.acknowledge(other, Custody::Accepted));
    assert_eq!(ui.followups.revision(), revision);
    assert!(ui.followups.acknowledge(owner, Custody::Unconfirmed));
    assert!(!ui.followups.acknowledge(other, Custody::Accepted));
    assert!(ui.followups.acknowledge(owner, Custody::Accepted));
    assert!(ui.followups.lines(100).is_empty());
    assert!(!ui.followups.acknowledge(owner, Custody::Accepted));
    Ok(())
}

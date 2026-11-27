//! Promotion is the authority boundary, including stops/rebinds after admission.
use super::*;
use grapt_inbox::{AdmitRequest, PromoteScope};
use grapt_types::EventId;

fn promote(
    inbox: &mut Inbox,
    session: &SessionId,
    name: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let event: EventId = format!("evt_promote_{name}").parse()?;
    Ok(inbox
        .promote_next(session, PromoteScope::Steer, &mut || event.clone(), 5)?
        .map(|row| row.input.to_string()))
}

fn human(inbox: &mut Inbox, session: &SessionId, name: &str) -> TestResult {
    inbox.admit(AdmitRequest {
        event_id: &format!("evt_{name}").parse()?,
        session,
        input: &format!("msg_{name}").parse()?,
        text: "continue human work",
        delivery: DeliveryMode::Steer,
        resume: true,
        created_ms: 4,
    })?;
    Ok(())
}

#[test]
fn native_steer_is_consumed_once_at_the_next_step() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "next_step", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit_mode(&mut inbox, &frozen, "step_notice", DeliveryMode::Steer)?;
    assert_eq!(
        promote(&mut inbox, &session, "step_notice")?.as_deref(),
        Some("msg_step_notice")
    );
    assert!(promote(&mut inbox, &session, "no_replay")?.is_none());
    assert!(inbox.pending_inputs(&session)?.is_empty());
    assert!(
        claim(&inbox, &session).is_err(),
        "source mail was consumed twice"
    );
    Ok(())
}

#[test]
fn stop_after_admission_keeps_notice_until_a_genuine_human_promotes() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "before_stop", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit_mode(&mut inbox, &frozen, "stopped_notice", DeliveryMode::Steer)?;
    inbox.hold_native_mail_wakes(&session, 3)?;
    assert!(promote(&mut inbox, &session, "stopped")?.is_none());
    human(&mut inbox, &session, "human_after_stop")?;
    assert_eq!(
        promote(&mut inbox, &session, "human_after_stop")?.as_deref(),
        Some("msg_human_after_stop")
    );
    assert!(inbox.native_mail_wakes_available(&session)?);
    assert_eq!(
        promote(&mut inbox, &session, "explicit_resume")?.as_deref(),
        Some("msg_stopped_notice")
    );
    assert!(promote(&mut inbox, &session, "no_replay")?.is_none());
    mail(&mut inbox, "after_resume", MailAttention::Wake, true)?;
    let fresh = claim(&inbox, &session)?;
    admit_mode(&mut inbox, &fresh, "fresh_notice", DeliveryMode::Steer)?;
    assert_eq!(
        promote(&mut inbox, &session, "fresh")?.as_deref(),
        Some("msg_fresh_notice")
    );
    assert!(inbox.pending_inputs(&session)?.is_empty());
    Ok(())
}

#[test]
fn rebind_after_admission_cannot_promote_old_notice_ahead_of_human_work() -> TestResult {
    let (mut inbox, session) = fixture()?;
    let fingerprint = MailBindingExpectation::new(
        InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound".into(),
    )?
    .fingerprint();
    inbox.select_native_mail_target(
        &session,
        &[],
        &fingerprint,
        &"evt_native_generation_one".parse()?,
        1,
    )?;
    mail(&mut inbox, "generation_one", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit_mode(&mut inbox, &frozen, "old_generation", DeliveryMode::Steer)?;
    inbox.select_native_mail_target(
        &session,
        &[],
        &fingerprint,
        &"evt_native_generation_two".parse()?,
        2,
    )?;
    human(&mut inbox, &session, "human_after_rebind")?;
    assert_eq!(
        promote(&mut inbox, &session, "human_after_rebind")?.as_deref(),
        Some("msg_human_after_rebind")
    );
    assert!(promote(&mut inbox, &session, "old_generation")?.is_none());
    assert_eq!(inbox.pending_inputs(&session)?.len(), 1);
    Ok(())
}

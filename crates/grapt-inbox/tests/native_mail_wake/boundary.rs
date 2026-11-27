//! Queue recovery must recheck authority at actual promotion.
//! The paired next-boundary tests cover the existing Queue mode.
use super::*;
use grapt_inbox::{AdmitRequest, PromoteScope};
use grapt_types::EventId;

fn promote(
    inbox: &mut Inbox,
    session: &SessionId,
    name: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    promote_at(inbox, session, name, PromoteScope::InputAtIdle)
}

fn promote_at(
    inbox: &mut Inbox,
    session: &SessionId,
    name: &str,
    scope: PromoteScope,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let event: EventId = format!("evt_promote_{name}").parse()?;
    Ok(inbox
        .promote_next(session, scope, &mut || event.clone(), 5)?
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
fn stop_after_admission_keeps_notice_until_a_genuine_human_promotes() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "before_stop", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "stopped_notice")?;
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
    admit(&mut inbox, &fresh, "fresh_notice")?;
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
    admit(&mut inbox, &frozen, "old_generation")?;
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

#[test]
fn human_steer_resumes_held_native_queue_before_the_next_step_only() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "before_stop", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "stopped_notice")?;
    inbox.admit(AdmitRequest {
        event_id: &"evt_ordinary_queued".parse()?,
        session: &session,
        input: &"msg_ordinary_queued".parse()?,
        text: "ordinary queued user work",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 2,
    })?;
    inbox.hold_native_mail_wakes(&session, 3)?;
    assert!(promote_at(&mut inbox, &session, "still_stopped", PromoteScope::Steer)?.is_none());
    human(&mut inbox, &session, "human_resumes_steer")?;
    assert_eq!(
        promote_at(&mut inbox, &session, "human", PromoteScope::Steer)?.as_deref(),
        Some("msg_human_resumes_steer")
    );
    assert_eq!(
        promote_at(&mut inbox, &session, "held_native", PromoteScope::Steer)?.as_deref(),
        Some("msg_stopped_notice")
    );
    assert!(promote_at(
        &mut inbox,
        &session,
        "ordinary_stays_queued",
        PromoteScope::Steer
    )?
    .is_none());
    assert_eq!(
        inbox
            .pending_inputs(&session)?
            .iter()
            .map(|row| row.input.as_str())
            .collect::<Vec<_>>(),
        ["msg_ordinary_queued"]
    );
    // New native Queue admissions retain their original idle-only mode.
    mail(&mut inbox, "fresh_queue", MailAttention::Wake, true)?;
    let fresh = claim(&inbox, &session)?;
    admit(&mut inbox, &fresh, "fresh_queue")?;
    assert!(promote_at(
        &mut inbox,
        &session,
        "fresh_stays_queued",
        PromoteScope::Steer
    )?
    .is_none());
    assert_eq!(
        promote(&mut inbox, &session, "ordinary_at_idle")?.as_deref(),
        Some("msg_ordinary_queued")
    );
    assert_eq!(
        promote(&mut inbox, &session, "fresh_at_idle")?.as_deref(),
        Some("msg_fresh_queue")
    );
    Ok(())
}

#[path = "boundary/timing.rs"]
mod timing;

//! Stop/admission races and target generations use the same durable input owner.
use super::*;
use grapt_inbox::{AdmitRequest, PendingInput, UserInputOrigin};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}
fn scratch() -> Result<Scratch, Box<dyn std::error::Error>> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!(
        "grapt-native-state-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&path)?;
    Ok(Scratch(path))
}

fn fingerprint() -> Result<String, Box<dyn std::error::Error>> {
    Ok(MailBindingExpectation::new(
        InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound".into(),
    )?
    .fingerprint())
}
fn select(inbox: &mut Inbox, session: &SessionId, old: &[SessionId], name: &str) -> TestResult {
    inbox.select_native_mail_target(session, old, &fingerprint()?, &name.parse()?, 2)?;
    Ok(())
}
fn human(
    inbox: &mut Inbox,
    session: &SessionId,
    name: &str,
    origin: UserInputOrigin,
) -> Result<PendingInput, Box<dyn std::error::Error>> {
    let input = format!("msg_{name}").parse()?;
    inbox.admit_with_origin(
        AdmitRequest {
            event_id: &format!("evt_{name}").parse()?,
            session,
            input: &input,
            text: "continue",
            delivery: DeliveryMode::Steer,
            resume: true,
            created_ms: 3,
        },
        &[],
        origin,
    )?;
    inbox
        .pending_inputs(session)?
        .into_iter()
        .find(|row| row.input == input)
        .ok_or_else(|| "human input missing".into())
}
fn resume(inbox: &mut Inbox, session: &SessionId, input: &PendingInput) -> TestResult {
    let held = inbox.store().held_writers();
    let tx = inbox.store_mut().governed_transaction()?;
    Inbox::resume_native_mail_after_human_within(&tx, &held, session, input, 4)?;
    tx.commit()?.discard();
    Ok(())
}
fn eligible(
    inbox: &Inbox,
    session: &SessionId,
    input: &PendingInput,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(Inbox::native_mail_input_eligible_within(
        inbox.store().connection(),
        session,
        input,
    )?)
}

#[test]
fn stop_before_any_mail_refuses_a_future_claim_without_consuming_it() -> TestResult {
    let (mut inbox, session) = fixture()?;
    assert_eq!(inbox.hold_native_mail_wakes(&session, 1)?, 0);
    assert_eq!(inbox.hold_native_mail_wakes(&session, 2)?, 0);
    mail(&mut inbox, "late", MailAttention::Wake, true)?;
    let late = claim(&inbox, &session)?;
    assert!(admit(&mut inbox, &late, "late_after_stop").is_err());
    assert!(inbox.pending_inputs(&session)?.is_empty());
    assert!(claim(&inbox, &session)?.source_messages().next().is_some());
    assert!(!inbox.native_mail_wakes_available(&session)?);
    Ok(())
}

#[test]
fn prepared_claim_loses_to_interrupt_before_admission() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "race", MailAttention::Wake, true)?;
    let prepared = claim(&inbox, &session)?;
    inbox.hold_native_mail_wakes(&session, 2)?;
    assert!(admit(&mut inbox, &prepared, "race_loser").is_err());
    assert!(inbox.pending_inputs(&session)?.is_empty());
    assert!(claim(&inbox, &session)?.source_messages().next().is_some());
    Ok(())
}

#[test]
fn interrupt_after_admission_fences_promotion_until_genuine_human_resume() -> TestResult {
    let (mut inbox, session) = fixture()?;
    select(&mut inbox, &session, &[], "evt_target_before_stop")?;
    mail(&mut inbox, "before", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "before_stop")?;
    let old = inbox.pending_inputs(&session)?.remove(0);
    assert!(eligible(&inbox, &session, &old)?);
    inbox.hold_native_mail_wakes(&session, 3)?;
    assert!(!eligible(&inbox, &session, &old)?);
    let user = human(&mut inbox, &session, "real_resume", UserInputOrigin::Human)?;
    assert!(eligible(&inbox, &session, &user)?);
    resume(&mut inbox, &session, &user)?;
    assert!(inbox.native_mail_wakes_available(&session)?);
    assert!(eligible(&inbox, &session, &old)?);
    assert!(
        inbox.pending_native_mail_wake(&session)?.is_none(),
        "per-input hold reauthorized automatic recovery"
    );
    mail(&mut inbox, "after", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "after_resume")?;
    let fresh = inbox
        .pending_inputs(&session)?
        .into_iter()
        .find(|row| row.input.as_str() == "msg_after_resume")
        .ok_or("fresh input")?;
    assert!(eligible(&inbox, &session, &fresh)?);
    Ok(())
}

#[test]
fn startup_and_synthetic_inputs_cannot_clear_an_explicit_stop() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "old", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "synthetic_resume")?;
    let synthetic = inbox.pending_inputs(&session)?.remove(0);
    inbox.hold_native_mail_wakes(&session, 2)?;
    resume(&mut inbox, &session, &synthetic)?;
    let startup = human(
        &mut inbox,
        &session,
        "startup",
        UserInputOrigin::SeatStartup,
    )?;
    resume(&mut inbox, &session, &startup)?;
    assert!(!inbox.native_mail_wakes_available(&session)?);
    Ok(())
}

#[test]
fn takeover_revokes_already_admitted_work_and_human_cannot_reclaim_old_target() -> TestResult {
    let (mut inbox, old) = fixture()?;
    select(&mut inbox, &old, &[], "evt_target_old")?;
    mail(&mut inbox, "takeover", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &old)?;
    admit(&mut inbox, &frozen, "old_target_notice")?;
    let notice = inbox.pending_inputs(&old)?.remove(0);
    let replacement: SessionId = "ses_replacement_target".parse()?;
    inbox
        .store_mut()
        .elect_writer(&AggregateId::Session(replacement.clone()))?;
    select(
        &mut inbox,
        &replacement,
        std::slice::from_ref(&old),
        "evt_target_replacement",
    )?;
    assert!(!eligible(&inbox, &old, &notice)?);
    let user = human(&mut inbox, &old, "old_human", UserInputOrigin::Human)?;
    resume(&mut inbox, &old, &user)?;
    assert!(
        !inbox.native_mail_wakes_available(&old)?,
        "human reclaimed superseded target"
    );
    assert!(!eligible(&inbox, &old, &notice)?);
    assert!(inbox.native_mail_wakes_available(&replacement)?);
    Ok(())
}

#[test]
fn explicit_rebind_invalidates_old_stamp_even_with_identical_binding_bytes() -> TestResult {
    let (mut inbox, session) = fixture()?;
    select(&mut inbox, &session, &[], "evt_target_generation_one")?;
    mail(&mut inbox, "generation", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "generation_one")?;
    let notice = inbox.pending_inputs(&session)?.remove(0);
    select(&mut inbox, &session, &[], "evt_target_generation_two")?;
    assert!(!eligible(&inbox, &session, &notice)?);
    Ok(())
}

#[test]
fn revocation_before_capability_change_is_fail_closed() -> TestResult {
    let (mut inbox, session) = fixture()?;
    select(&mut inbox, &session, &[], "evt_target_generation_live")?;
    mail(&mut inbox, "revocation", MailAttention::Wake, true)?;
    let prepared = claim(&inbox, &session)?;
    inbox.revoke_native_mail_target(&session, &"evt_before_capability".parse()?, 3)?;
    assert!(admit(&mut inbox, &prepared, "revoked_claim").is_err());
    assert!(!inbox.native_mail_wakes_available(&session)?);
    Ok(())
}

#[test]
fn held_session_state_survives_restart_and_strict_replay() -> TestResult {
    let directory = scratch()?;
    let path = directory.0.join("inbox.sqlite");
    let (mut inbox, session) = initialize(EventStore::open(&path, "native-state-create")?)?;
    inbox.hold_native_mail_wakes(&session, 1)?;
    drop(inbox);
    let inbox = Inbox::new(EventStore::open(&path, "native-state-recover")?)?;
    assert!(!inbox.native_mail_wakes_available(&session)?);
    let data: String = inbox.store().connection().query_row(
        "SELECT data FROM event WHERE type='session.native-mail-state.1'",
        [],
        |r| r.get(0),
    )?;
    let value = serde_json::from_str(&data)?;
    assert_eq!(
        grapt_inbox::InboxEvent::decode("session.native-mail-state", 1, &value)?,
        None
    );
    assert!(grapt_inbox::InboxEvent::decode("session.native-mail-state", 2, &value).is_err());
    assert!(grapt_inbox::InboxEvent::decode(
        "session.native-mail-state",
        1,
        &serde_json::json!({"sessionID":session.as_str(),"held":false,"humanResume":false,"target":null,"binding":"forged"})
    )
    .is_err());
    Ok(())
}

#[test]
fn corrupt_durable_state_refuses_instead_of_recovering_as_unheld() -> TestResult {
    let directory = scratch()?;
    let path = directory.0.join("inbox.sqlite");
    let (mut inbox, session) = initialize(EventStore::open(&path, "native-corrupt-create")?)?;
    let held = inbox.store().held_writers();
    let aggregate = AggregateId::Session(session.clone());
    let data = serde_json::json!({"sessionID":session.as_str(),"held":"false","humanResume":false,"target":null,"binding":null});
    inbox
        .store_mut()
        .append(
            grapt_store::AppendInput {
                id: &"evt_corrupt_native_state".parse()?,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.native-mail-state",
                version: grapt_types::EventVersion::new(1).ok_or("version")?,
                created_ms: 1,
                data: &data,
            },
            &mut [],
        )?
        .1
        .discard();
    assert!(inbox.native_mail_wakes_available(&session).is_err());
    drop(inbox);
    let recovered = Inbox::new(EventStore::open(&path, "native-corrupt-recover")?)?;
    assert!(recovered.native_mail_wakes_available(&session).is_err());
    Ok(())
}

#[test]
fn ordinary_human_promotions_do_not_append_native_control_facts() -> TestResult {
    for bound_target in [false, true] {
        let (mut inbox, session) = fixture()?;
        if bound_target {
            select(&mut inbox, &session, &[], "evt_unheld_target")?;
        }
        let user = human(&mut inbox, &session, "ordinary", UserInputOrigin::Human)?;
        let event: grapt_types::EventId = "evt_ordinary_promoted".parse()?;
        let promoted = inbox
            .promote_next(
                &session,
                grapt_inbox::PromoteScope::Steer,
                &mut || event.clone(),
                4,
            )?
            .ok_or("ordinary human promotion")?;
        assert_eq!(promoted.seq, user.seq + 1);
        let native_states: i64 = inbox.store().connection().query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND type='session.native-mail-state.1'",
            [session.as_str()],
            |row| row.get(0),
        )?;
        assert_eq!(native_states, i64::from(bound_target));
    }
    Ok(())
}

#[test]
fn a_legacy_per_input_hold_still_resumes_after_genuine_human_promotion() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "legacy_mail", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "legacy_notice")?;
    let aggregate = AggregateId::Session(session.clone());
    let held = inbox.store().held_writers();
    let data = serde_json::json!({"sessionID":session.as_str(),"inputID":"msg_legacy_notice"});
    inbox
        .store_mut()
        .append(
            grapt_store::AppendInput {
                id: &"evt_native_wake_hold_msg_legacy_notice".parse()?,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.input.native-wake-held",
                version: grapt_types::EventVersion::new(1).ok_or("version")?,
                created_ms: 2,
                data: &data,
            },
            &mut [&mut grapt_inbox::InboxProjector],
        )?
        .1
        .discard();
    assert!(inbox.pending_native_mail_wake(&session)?.is_none());
    let user = human(
        &mut inbox,
        &session,
        "resume_legacy",
        UserInputOrigin::Human,
    )?;
    let event: grapt_types::EventId = "evt_legacy_human_promoted".parse()?;
    let promoted = inbox
        .promote_next(
            &session,
            grapt_inbox::PromoteScope::Steer,
            &mut || event.clone(),
            4,
        )?
        .ok_or("human resume promotion")?;
    assert_eq!(promoted.input, user.input);
    let event: grapt_types::EventId = "evt_legacy_notice_promoted".parse()?;
    let resumed = inbox
        .promote_next(
            &session,
            grapt_inbox::PromoteScope::Steer,
            &mut || event.clone(),
            5,
        )?
        .ok_or("legacy native promotion")?;
    assert_eq!(resumed.input.as_str(), "msg_legacy_notice");
    assert!(inbox.pending_inputs(&session)?.is_empty());
    Ok(())
}

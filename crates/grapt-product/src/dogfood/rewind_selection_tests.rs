use super::*;
use grapt_session::rewind::{self, RewindError};

fn fixture() -> Result<(Inbox, [SessionId; 2]), Box<dyn std::error::Error>> {
    let clock = FixedClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let inbox = Arc::new(Mutex::new(Inbox::new(EventStore::open_in_memory()?)?));
    let ports = SessionPorts::new(Arc::clone(&inbox), &authorities);
    let seed = SessionSeed::new(
        ProjectId::new("prj_rewind_selection").ok_or("project identity")?,
        if cfg!(windows) {
            "E:/rewind-selection"
        } else {
            "/rewind-selection"
        },
        "rewind selection",
    )?;
    let sessions = [ports.create(&seed)?.session, ports.create(&seed)?.session];
    drop(ports);
    let inbox = Arc::try_unwrap(inbox)
        .map_err(|_| "rewind fixture retained an inbox owner")?
        .into_inner()
        .map_err(|_| "rewind fixture inbox was poisoned")?;
    let mut inbox = inbox;
    grapt_instruction::InstructionService::ensure_tables(inbox.store_mut())?;
    Ok((inbox, sessions))
}

fn promote(
    inbox: &mut Inbox,
    session: &SessionId,
    suffix: &str,
) -> Result<MessageId, Box<dyn std::error::Error>> {
    promote_with_delivery(inbox, session, suffix, DeliveryMode::Queue)
}

fn promote_with_delivery(
    inbox: &mut Inbox,
    session: &SessionId,
    suffix: &str,
    delivery: DeliveryMode,
) -> Result<MessageId, Box<dyn std::error::Error>> {
    let input = message(&format!("msg_rewind_{suffix}"));
    let admitted = event(&format!("evt_rewind_{suffix}_admitted"));
    inbox.admit(grapt_inbox::AdmitRequest {
        event_id: &admitted,
        session,
        input: &input,
        text: suffix,
        delivery,
        resume: false,
        created_ms: FixedClock.now().as_millis(),
    })?;
    inbox
        .promote_next(
            session,
            if delivery == DeliveryMode::Steer {
                grapt_inbox::PromoteScope::Steer
            } else {
                grapt_inbox::PromoteScope::InputAtIdle
            },
            &mut || event(&format!("evt_rewind_{suffix}_promoted")),
            FixedClock.now().as_millis(),
        )?
        .ok_or("fixture input was not promoted")?;
    Ok(input)
}

#[test]
fn rw_cut_preserves_prior_and_new_work_without_erasing_the_ledger(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut inbox, [session, _]) = fixture()?;
    let first = promote(&mut inbox, &session, "kept")?;
    let removed = promote(&mut inbox, &session, "discarded")?;
    let selected = rewind::select(inbox.store(), &session, &removed)?;
    let clock = FixedClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let seq = rewind::commit(
        inbox.store_mut(),
        &authorities,
        &selected,
        &mut [&mut grapt_instruction::InstructionProjector],
    )?;
    assert_eq!(seq, selected.tip().value() + 1);
    let rows = inbox.promoted_messages(&session, 10)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input, first);
    assert!(matches!(
        rewind::select(inbox.store(), &session, &removed),
        Err(RewindError::NotUserPrompt)
    ));
    let count: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = 'session.input.promoted.1'",
        [session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(count, 2, "rewind erased canonical promotions");
    let next = promote(&mut inbox, &session, "new")?;
    let retry = rewind::for_commit(inbox.store(), &session, &removed, selected.tip())?;
    assert_eq!(
        rewind::commit(inbox.store_mut(), &authorities, &retry, &mut [])?,
        seq
    );
    assert_eq!(
        grapt_store::history_visibility::HistoryVisibility::read(
            inbox.store().connection(),
            &session
        )?
        .count(),
        1
    );
    let rows = inbox.promoted_messages(&session, 10)?;
    assert_eq!(
        rows.iter().map(|row| &row.input).collect::<Vec<_>>(),
        vec![&first, &next]
    );
    Ok(())
}

#[test]
fn rw_changed_tip_and_failed_projection_leave_no_cut() -> Result<(), Box<dyn std::error::Error>> {
    struct Refuse;
    impl grapt_store::Projector for Refuse {
        fn projection(&self) -> grapt_store::ProjectionId {
            grapt_store::ProjectionId::new("rewind_failure")
        }
        fn project(
            &mut self,
            _: &rusqlite::Transaction<'_>,
            _: &grapt_store::DecodedEvent,
        ) -> Result<(), String> {
            Err("injected rewind projection failure".into())
        }
    }
    let (mut inbox, [session, _]) = fixture()?;
    let first = promote(&mut inbox, &session, "first")?;
    let stale = rewind::select(inbox.store(), &session, &first)?;
    promote(&mut inbox, &session, "second")?;
    let clock = FixedClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    assert!(matches!(
        rewind::commit(inbox.store_mut(), &authorities, &stale, &mut []),
        Err(RewindError::Stale)
    ));
    let selected = rewind::select(inbox.store(), &session, &first)?;
    assert!(rewind::commit(
        inbox.store_mut(),
        &authorities,
        &selected,
        &mut [&mut Refuse]
    )
    .is_err());
    assert_eq!(rewind::select(inbox.store(), &session, &first)?, selected);
    assert_eq!(inbox.promoted_messages(&session, 10)?.len(), 2);
    assert_eq!(
        grapt_store::history_visibility::HistoryVisibility::read(
            inbox.store().connection(),
            &session
        )?
        .count(),
        0
    );
    Ok(())
}

#[test]
fn rw_pending_input_and_unknown_projection_refuse_without_a_cut(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut inbox, [session, _]) = fixture()?;
    let first = promote(&mut inbox, &session, "before_pending")?;
    inbox.admit(grapt_inbox::AdmitRequest {
        event_id: &event("evt_rewind_pending"),
        session: &session,
        input: &message("msg_rewind_pending"),
        text: "pending work",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: FixedClock.now().as_millis(),
    })?;
    let selected = rewind::select(inbox.store(), &session, &first)?;
    let clock = FixedClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    assert!(matches!(
        rewind::commit(inbox.store_mut(), &authorities, &selected, &mut []),
        Err(RewindError::Pending)
    ));
    inbox.store().connection().execute(
        "UPDATE session_message SET type = 'future-user' WHERE id = ?1",
        [first.as_str()],
    )?;
    assert!(matches!(
        rewind::previous(inbox.store(), &session, 1),
        Err(RewindError::Corrupt(_))
    ));
    assert_eq!(
        grapt_store::history_visibility::HistoryVisibility::read(
            inbox.store().connection(),
            &session
        )?
        .count(),
        0
    );
    Ok(())
}

#[test]
fn rw_multiple_cuts_preserve_only_survivors_and_new_work() -> Result<(), Box<dyn std::error::Error>>
{
    let (mut inbox, [session, _]) = fixture()?;
    let first = promote(&mut inbox, &session, "cut_one")?;
    let second = promote(&mut inbox, &session, "cut_two")?;
    let clock = FixedClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let selected = rewind::select(inbox.store(), &session, &second)?;
    rewind::commit(inbox.store_mut(), &authorities, &selected, &mut [])?;
    let third = promote(&mut inbox, &session, "cut_three")?;
    let selected = rewind::previous(inbox.store(), &session, 2)?;
    assert_eq!(selected.before(), &first);
    rewind::commit(inbox.store_mut(), &authorities, &selected, &mut [])?;
    assert!(inbox.promoted_messages(&session, 10)?.is_empty());
    assert!(matches!(
        rewind::select(inbox.store(), &session, &third),
        Err(RewindError::NotUserPrompt)
    ));
    let last = promote(&mut inbox, &session, "cut_survivor")?;
    assert_eq!(inbox.promoted_messages(&session, 10)?[0].input, last);
    assert_eq!(
        grapt_store::history_visibility::HistoryVisibility::read(
            inbox.store().connection(),
            &session
        )?
        .count(),
        2
    );
    Ok(())
}

#[test]
fn a_rewind_selection_is_bound_to_its_visible_prompt_and_session(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut inbox, [session, foreign]) = fixture()?;
    let input = message("msg_rewind_selected");
    let admitted = event("evt_rewind_admitted");
    let text = "Explain the change to λ without editing files.";
    let now = FixedClock.now().as_millis();
    inbox.admit(grapt_inbox::AdmitRequest {
        event_id: &admitted,
        session: &session,
        input: &input,
        text,
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: now,
    })?;
    assert!(matches!(
        rewind::select(inbox.store(), &session, &input),
        Err(RewindError::NotUserPrompt)
    ));
    let promoted = inbox
        .promote_next(
            &session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut || event("evt_rewind_promoted"),
            now + 1,
        )?
        .ok_or("selected prompt was not promoted")?;
    let selected = rewind::select(inbox.store(), &session, &input)?;
    assert_eq!(selected.session(), &session);
    assert_eq!(selected.before(), &input);
    assert_eq!(selected.boundary().value(), promoted.seq);
    assert_eq!(selected.tip(), selected.boundary());
    assert_eq!(selected.prompt(), text);
    assert!(matches!(
        rewind::select(inbox.store(), &foreign, &input),
        Err(RewindError::NotUserPrompt)
    ));
    assert!(matches!(
        rewind::select(inbox.store(), &session, &message("msg_rewind_absent")),
        Err(RewindError::NotUserPrompt)
    ));
    assert!(matches!(
        rewind::select(inbox.store(), &super::session("ses_rewind_unknown"), &input),
        Err(RewindError::Session(_))
    ));
    let oversized: MessageId = format!("msg_{}", "a".repeat(512)).parse()?;
    assert!(matches!(
        rewind::select(inbox.store(), &session, &oversized),
        Err(RewindError::AddressTooLong)
    ));
    Ok(())
}

#[test]
fn rw_steers_are_not_boundaries_but_a_new_execution_after_process_loss_is(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut inbox, [session, _]) = fixture()?;
    let initial = promote(&mut inbox, &session, "initial_execution")?;
    let clock = FixedClock;
    let ids = CountingIds(AtomicU64::new(1000));
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let execution = grapt_types::ExecutionId::new("execution-before-process-loss")
        .ok_or("execution identity")?;
    grapt_session::execution::begin(inbox.store_mut(), &authorities, &session, &execution)?;
    let steer = promote_with_delivery(
        &mut inbox,
        &session,
        "during_execution",
        DeliveryMode::Steer,
    )?;
    assert!(matches!(
        rewind::select(inbox.store(), &session, &steer),
        Err(RewindError::MidExecution)
    ));
    assert!(rewind::select(inbox.store(), &session, &initial).is_ok());
    // Execution observations survive their process. The next coordinator may
    // admit an initial input after recovering its attempts and effects.
    let next = promote(&mut inbox, &session, "new_execution")?;
    let execution = grapt_types::ExecutionId::new("execution-after-process-loss")
        .ok_or("execution identity")?;
    grapt_session::execution::begin(inbox.store_mut(), &authorities, &session, &execution)?;
    assert_eq!(
        rewind::select(inbox.store(), &session, &next)?.before(),
        &next
    );
    assert!(matches!(
        rewind::select(inbox.store(), &session, &steer),
        Err(RewindError::MidExecution)
    ));
    Ok(())
}

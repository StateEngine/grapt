//! SC-COMPACT-01 fix 2 through the ordinary product plan: a structural
//! refusal plans no request of its own, so the compactor is handed the
//! request the plan last sent. On row A it had none, and every compaction fell
//! back from the provider's server-side compaction and the cache-warm summary
//! to a cold summary that read nothing from cache.

use super::sustained_defects::product_plan;
use super::*;
use grapt_runtime::AttemptPlan;
use grapt_types::CompactionId;

fn plan_at(
    plan: &mut crate::dogfood::production::ProductPlan,
    step: u32,
) -> Result<Result<grapt_runtime::PlannedAttempt, grapt_runtime::PlanRefusal>, String> {
    Ok(plan.plan(
        StepNumber::new(step).ok_or("step")?,
        1,
        grapt_runtime::HistoryGeneration::new(1),
    ))
}

#[test]
fn a_structural_refusal_hands_the_compactor_the_request_last_sent() -> Result<(), String> {
    let fixture = Fixture::new("structural-request")?;
    fixture.record_exchange()?;
    let slot = admission::DiagnosisSlot::default();
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?
        .with_admission_diagnosis(slot.clone());
    let sent = plan_at(&mut plan, 1)?
        .map_err(|refusal| refusal.detail)?
        .request;
    plan.admitted(&grapt_types::PhysicalAttemptId::new("att_structural_sent").ok_or("attempt")?);

    // Promoted inputs one past the page bound: the next plan refuses before
    // it can plan a request.
    for index in 0..=MAX_CONTINUATION_ROWS {
        let input = format!("msg_structural_{index:04}")
            .parse::<MessageId>()
            .map_err(|e| e.to_string())?;
        promote_test_input(fixture.inbox()?, &fixture.session, &input, "later")?;
    }
    let refusal = plan_at(&mut plan, 2)?
        .err()
        .ok_or("the plan ignored the structural bound")?;
    assert_eq!(
        refusal.compaction,
        Some(grapt_types::CompactionTrigger::StructuralBound)
    );
    assert_eq!(
        slot.coding_request(),
        Some(sent),
        "the compactor was not handed the request last sent"
    );

    // A plan that refuses again, with nothing sent since, has none to stand on.
    plan_at(&mut plan, 3)?
        .err()
        .ok_or("the plan ignored the structural bound")?;
    assert!(slot.coding_request().is_none(), "a reused request was kept");
    Ok(())
}

/// A settled response of one text block, `text`, at `step`.
fn answer(fixture: &Fixture, step: u32, text: &str) -> Result<(), String> {
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(u64::from(step) * 100_000))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(step).ok_or("step")?,
        None,
    )
    .map_err(|e| e.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("answer-{step}")).ok_or("attempt")?,
        })
        .map_err(|e| e.to_string())?;
    let block = grapt_provider::BlockOrdinal::new(0);
    for fragment in [
        AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block,
            text: text.to_owned(),
            state: AdapterState::none(),
        },
        AttemptFragment::TextClosed {
            block,
            state: AdapterState::none(),
        },
    ] {
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(fragment),
            })
            .map_err(|e| e.to_string())?;
    }
    for fact in [
        DurableFact::AttemptEnded {
            finish: FinishReason::Stop,
        },
        DurableFact::StepEnded {
            finish: FinishReason::Stop,
        },
    ] {
        owner.commit(fact).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Plans and sends a request on `fixture`, sharing `slot` with the plan.
fn send_one(
    fixture: &Fixture,
    slot: &admission::DiagnosisSlot,
) -> Result<crate::dogfood::production::ProductPlan, String> {
    let mut plan = product_plan(fixture, fixture.input.clone(), "use the tool")?
        .with_admission_diagnosis(slot.clone());
    plan_at(&mut plan, 1)?.map_err(|refusal| refusal.detail)?;
    plan.admitted(&grapt_types::PhysicalAttemptId::new("att_coverage_sent").ok_or("attempt")?);
    Ok(plan)
}

const ANSWER_MARKER: &str = "ASSISTANT_ONLY_MARKER the request never carried";

/// The fix 2 review hold, the instruction-epoch path: that refusal comes with
/// no structural boundary, so on fully settled history a provider's item cuts
/// at its head (SE's own summary keeps an exact tail there instead). The reused
/// request was sent before the newest response existed; a cut past that
/// response's opening would replace text no compaction input held. The
/// refusal site's own sequence (a new plan, then the reuse) runs on the shared
/// snapshot, then the cut is committed and the request reopened.
#[test]
fn a_reused_request_never_covers_a_response_it_did_not_carry() -> Result<(), String> {
    let fixture = Fixture::new("coverage-response")?;
    fixture.record_exchange()?;
    let slot = admission::DiagnosisSlot::default();
    let _plan = send_one(&fixture, &slot)?;
    answer(&fixture, 2, ANSWER_MARKER)?;
    slot.clear();
    slot.reuse_previous_request();
    assert!(slot.coding_request().is_some());

    let continuation = fixture
        .continuation(fixture.input.clone())?
        .with_coverage(slot.clone());
    let id = CompactionId::new("coverage-response").ok_or("compaction")?;
    let selected = continuation.prepare_provider_summary("use the tool", &id, 1_000_000)?;
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    let rebuilt = format!(
        "{:?}",
        fixture
            .continuation(fixture.input.clone())?
            .request_input("use the tool", false)?
    );
    assert!(
        rebuilt.contains(ANSWER_MARKER),
        "a response the reused request never carried was compacted away"
    );
    Ok(())
}

const INPUT_MARKER: &str = "UNSEEN_PROMOTED_MARKER arrived after the request";

/// The promoted-input path through the real plan: inputs that arrived after
/// the reused request stay exact after the cut, though their own boundary is
/// a legal cut for a summary of current history.
#[test]
fn a_reused_request_never_covers_inputs_that_arrived_after_it() -> Result<(), String> {
    for provider in [true, false] {
        let label = if provider {
            "coverage-promoted-provider"
        } else {
            "coverage-promoted-text"
        };
        let fixture = Fixture::new(label)?;
        fixture.record_exchange()?;
        let slot = admission::DiagnosisSlot::default();
        let mut plan = send_one(&fixture, &slot)?;
        // The response to that request, which it never carried: the bounded
        // cut falls at its opening and replaces the exchange before it.
        answer(&fixture, 2, ANSWER_MARKER)?;
        let mut first = None;
        for index in 0..=MAX_CONTINUATION_ROWS {
            let input = format!("msg_coverage_{index:04}")
                .parse::<MessageId>()
                .map_err(|e| e.to_string())?;
            let text = if index == 0 {
                INPUT_MARKER.to_owned()
            } else {
                format!("later {index}")
            };
            promote_test_input(fixture.inbox()?, &fixture.session, &input, &text)?;
            first.get_or_insert(input);
        }
        plan_at(&mut plan, 2)?
            .err()
            .ok_or("the plan ignored the structural bound")?;
        assert!(slot.coding_request().is_some());

        let continuation = fixture
            .continuation(fixture.input.clone())?
            .with_coverage(slot.clone());
        let id = CompactionId::new(label).ok_or("compaction")?;
        let selected = if provider {
            continuation.prepare_provider_summary("use the tool", &id, 1_000_000)?
        } else {
            continuation.prepare_summary("use the tool", &id, 1_000_000)?
        };
        let first_seq = lock(fixture.inbox()?)
            .promoted_input(&fixture.session, &first.ok_or("no input")?)
            .map_err(|e| e.to_string())?
            .seq;
        // Later inputs are no rows of this input's request (its context stops
        // at the next input), so the cut alone decides whether they stay exact.
        assert!(
            selected.selection.from <= first_seq,
            "provider={provider}: the cut {} passed an input the reused request never carried ({first_seq})",
            selected.selection.from
        );
        super::summary_tests::commit_selection(&fixture, &selected, id)?;
        let durable = lock(fixture.inbox()?)
            .store()
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM session_message WHERE session_id = ?1 AND text = ?2",
                (fixture.session.as_str(), INPUT_MARKER),
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| e.to_string())?;
        assert_eq!(durable, 1, "the unseen input is no longer durable");
    }
    Ok(())
}

const EXCLUDED_MARKER: &str = "EXCLUDED_INPUT_MARKER past the facts budget";

/// The second fix 2 review hold: a request stops at the next promoted input,
/// which may be durable before its plan read the Session head. That input
/// is no row of the request, so a summary standing for the request must not
/// cut past it; its text (here past the bounded retained-facts block) stays
/// exact in the request of the input after it.
#[test]
fn a_reused_request_never_covers_an_input_its_context_stopped_before() -> Result<(), String> {
    let fixture = Fixture::new("coverage-ceiling")?;
    // An earlier turn, then the current input and the one after it, both
    // promoted before the plan reads the Session head.
    fixture.record_exchange()?;
    let id = |text: &str| text.parse::<MessageId>().map_err(|e| e.to_string());
    let (current, excluded, next) = (
        id("msg_coverage_current")?,
        id("msg_coverage_excluded")?,
        id("msg_coverage_next")?,
    );
    let excluded_text = format!("{}{EXCLUDED_MARKER}", "x".repeat(20_000));
    promote_test_input(fixture.inbox()?, &fixture.session, &current, "read on")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &excluded,
        &excluded_text,
    )?;

    let slot = admission::DiagnosisSlot::default();
    let mut plan =
        product_plan(&fixture, current.clone(), "read on")?.with_admission_diagnosis(slot.clone());
    let sent = plan_at(&mut plan, 1)?
        .map_err(|refusal| refusal.detail)?
        .request;
    assert!(!format!("{sent:?}").contains(EXCLUDED_MARKER));
    plan.admitted(&grapt_types::PhysicalAttemptId::new("att_coverage_ceiling").ok_or("attempt")?);
    answer(&fixture, 2, "the current input's answer")?;
    slot.clear();
    slot.reuse_previous_request();

    let compaction = CompactionId::new("coverage-ceiling").ok_or("compaction")?;
    let selected = fixture
        .continuation(current)?
        .with_coverage(slot.clone())
        .prepare_provider_summary("read on", &compaction, 1_000_000)?;
    super::summary_tests::commit_selection(&fixture, &selected, compaction)?;

    promote_test_input(fixture.inbox()?, &fixture.session, &next, "and then")?;
    let rebuilt = format!(
        "{:?}",
        fixture
            .continuation(next)?
            .request_input("and then", false)?
    );
    assert!(
        rebuilt.contains(EXCLUDED_MARKER),
        "an input the reused request's context stopped before was compacted away"
    );
    Ok(())
}

/// The third fix 2 review hold: when a prior descriptor leaves no cut inside
/// the reused request's coverage, SE's own summary falls back to durable
/// history, but its quoted history still stops at the next input's ceiling.
/// A cut past a long input that already waits there would replace text the
/// summary never quoted, so the fallback refuses or keeps the input exact.
#[test]
fn a_text_fallback_never_cuts_past_an_input_its_payload_stopped_before() -> Result<(), String> {
    let fixture = Fixture::new("coverage-fallback")?;
    fixture.record_exchange()?;
    answer(&fixture, 2, "an older answer")?;
    // A prior compaction retains from the head, so no older cut can advance.
    super::sustained_defects::stage_cut(
        &fixture,
        "coverage-fallback-prior",
        super::sustained_defects::head(&fixture)?,
    )?;
    let id = |text: &str| text.parse::<MessageId>().map_err(|e| e.to_string());
    let (current, excluded, next) = (
        id("msg_fallback_current")?,
        id("msg_fallback_excluded")?,
        id("msg_fallback_next")?,
    );
    let excluded_text = format!("{}{EXCLUDED_MARKER}", "x".repeat(20_000));
    promote_test_input(fixture.inbox()?, &fixture.session, &current, "read on")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &excluded,
        &excluded_text,
    )?;

    let slot = admission::DiagnosisSlot::default();
    let mut plan =
        product_plan(&fixture, current.clone(), "read on")?.with_admission_diagnosis(slot.clone());
    plan_at(&mut plan, 3)?.map_err(|refusal| refusal.detail)?;
    plan.admitted(&grapt_types::PhysicalAttemptId::new("att_fallback_sent").ok_or("attempt")?);
    answer(&fixture, 4, "the current input's answer")?;
    slot.clear();
    slot.reuse_previous_request();

    let compaction = CompactionId::new("coverage-fallback").ok_or("compaction")?;
    let prepared = fixture
        .continuation(current)?
        .with_coverage(slot.clone())
        .prepare_summary("read on", &compaction, 1_000_000);
    if let Ok(selected) = prepared {
        super::summary_tests::commit_selection(&fixture, &selected, compaction)?;
        promote_test_input(fixture.inbox()?, &fixture.session, &next, "and then")?;
        let rebuilt = format!(
            "{:?}",
            fixture
                .continuation(next)?
                .request_input("and then", false)?
        );
        assert!(
            rebuilt.contains(EXCLUDED_MARKER),
            "the cold fallback cut past an input its payload never quoted"
        );
    }
    Ok(())
}

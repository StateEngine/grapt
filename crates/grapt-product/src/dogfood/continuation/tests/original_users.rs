//! Retained messages are original history, not newly issued requests.
use super::*;

fn retained(first: &str, later: &[&str]) -> Result<Vec<CodexInputItem>, String> {
    let fixture = Fixture::new("original-users-budget")?;
    promote_test_input(fixture.inbox()?, &fixture.session, &fixture.input, first)?;
    let mut current = fixture.input.clone();
    for (index, text) in later.iter().enumerate() {
        current = format!("msg_original_{index}")
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        promote_test_input(fixture.inbox()?, &fixture.session, &current, text)?;
    }
    let result = super::super::retained_users::prefix(
        &lock(fixture.inbox()?),
        fixture.artifacts()?,
        &fixture.session,
        &current,
        grapt_types::EventSeq::new(1_000_000),
        true,
    );
    result
}

fn users(input: &[CodexInputItem]) -> Vec<&str> {
    input
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::Message(message) if message.role == MessageRole::User => {
                Some(message.content.as_str())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn originals_are_whole_verbatim_newest_within_budget_then_chronological() -> Result<(), String> {
    let newest = "é\n\"\\".repeat(1000);
    let older = "x".repeat(12_000);
    let input = retained("first objective", &[&older, &newest, "current correction"])?;
    assert_eq!(users(&input), [&newest]);
    let exact = "a".repeat(16_384);
    assert_eq!(users(&retained(&exact, &["current"])?), [&exact]);
    let over = "a".repeat(16_385);
    assert!(retained(&over, &["current"])?.is_empty());
    assert_eq!(
        users(&retained("first\n\"raw\"", &["second", "third", "now"])?),
        ["first\n\"raw\"", "second", "third"]
    );
    // Per-message rounding cannot spend the same fractional token twice.
    assert_eq!(users(&retained(&"a".repeat(16_381), &["b", "now"])?), ["b"]);
    Ok(())
}

#[test]
fn lifetime_history_beyond_one_page_continues_after_compacted_restart() -> Result<(), String> {
    let mut fixture = Fixture::new("original-users-long-restart")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "original seed",
    )?;
    for index in 0..MAX_CONTINUATION_ROWS + 12 {
        let input: MessageId = format!("msg_lifetime_{index}")
            .parse()
            .map_err(|_| "input")?;
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &input,
            &format!("input {index:04}"),
        )?;
    }
    let current: MessageId = "msg_lifetime_current".parse().map_err(|_| "current")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &current,
        "current objective",
    )?;
    let (cut, prefix) = {
        let inbox = lock(fixture.inbox()?);
        let cut = inbox
            .promoted_input(&fixture.session, &current)
            .map_err(|error| error.to_string())?
            .seq;
        let prefix = super::super::retained_users::prefix(
            &inbox,
            fixture.artifacts()?,
            &fixture.session,
            &current,
            grapt_types::EventSeq::new(cut),
            true,
        )?;
        (cut, prefix)
    };
    let originals = users(&prefix);
    assert_eq!(originals.len(), MAX_CONTINUATION_ROWS);
    assert_eq!(originals.first(), Some(&"input 0012"));
    assert_eq!(originals.last(), Some(&"input 1291"));
    // The descriptor's source is deliberately older than a lifetime page.
    // Commit it through the same typed descriptor/artifact owner as the other
    // replay witnesses, without requiring an overflowing uncompacted request.
    let id = grapt_types::CompactionId::new("original-long-restart").ok_or("id")?;
    let selected = super::super::summarization::SummaryPreparation {
        text: "bounded long-history fixture".to_owned(),
        selection: super::super::retained::Selection {
            session: fixture.session.clone(),
            input: current.clone(),
            compaction: id.clone(),
            from: cut,
            source_head: cut,
            admission: None,
            server: None,
            summary_plan: None,
        },
        prefix,
        retained: vec![CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content: "current objective".to_owned(),
        })],
        recent: grapt_api::u3a::Utf16Text::empty(),
        previous_generation: None,
        source_epoch: grapt_types::ContextEpoch::FIRST,
        removed_bytes: 30_000,
        retained_bytes: "current objective".len(),
        quote: None,
    };
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    let continuation = fixture.continuation(current.clone())?;
    let before = continuation.request_input("current objective", false)?;
    assert_eq!(users(&before).len(), MAX_CONTINUATION_ROWS + 1);
    drop(continuation);
    fixture.close();
    let reopened = fixture.reopen(current)?;
    assert_eq!(reopened.request_input("current objective", false)?, before);
    let next: MessageId = "msg_lifetime_next".parse().map_err(|_| "next")?;
    lock(&reopened.inbox)
        .store_mut()
        .elect_writer(&AggregateId::Session(fixture.session.clone()))
        .map_err(|error| error.to_string())?;
    promote_test_input(&reopened.inbox, &fixture.session, &next, "later objective")?;
    let next = DurableContinuation::new(
        reopened.inbox.clone(),
        reopened.artifacts.clone(),
        fixture.session.clone(),
        next,
    );
    let after = next.request_input("later objective", false)?;
    assert_eq!(&after[..before.len()], &before);
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn original_seed_summary_precedence_and_tool_pairs_survive_commit_and_restart() -> Result<(), String>
{
    let mut fixture = Fixture::new("original-users-reopen")?;
    fixture
        .record_exchange_output(
            true,
            false,
            false,
            READ_TOOL,
            "read",
            &b"old output ".repeat(3000),
        )?
        .ok_or("old output")?;
    super::working_set::output_step(&fixture, 2, "recent exact result")?;
    let current: MessageId = "msg_original_current".parse().map_err(|_| "current")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &current,
        "cancel the old task; inspect only",
    )?;
    let continuation = fixture.continuation(current.clone())?;
    let id = grapt_types::CompactionId::new("original-users-reopen").ok_or("id")?;
    let selected =
        continuation.prepare_summary("cancel the old task; inspect only", &id, 128_000)?;
    assert_eq!(users(&selected.prefix).first(), Some(&"use the tool"));
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    let before = continuation.request_input("cancel the old task; inspect only", false)?;
    let reference = before
        .iter()
        .find_map(|item| match item {
            CodexInputItem::Message(message)
                if message
                    .content
                    .starts_with("[Compaction recovery reference]") =>
            {
                Some(&message.content)
            }
            _ => None,
        })
        .ok_or("committed recovery reference")?;
    assert!(selected
        .rebuilt_input("summary")
        .iter()
        .any(|item| matches!(item,
        CodexInputItem::Message(message) if &message.content == reference)));
    let coordinates = reference
        .lines()
        .next()
        .ok_or("reference line")?
        .strip_prefix("[Compaction recovery reference] ")
        .ok_or("reference label")?;
    let coordinates: serde_json::Value =
        serde_json::from_str(coordinates).map_err(|e| e.to_string())?;
    assert_eq!(coordinates["session"], fixture.session.as_str());
    assert_eq!(coordinates["compaction"], "original-users-reopen");
    assert_eq!(
        coordinates["exactHistoryFromEvent"],
        selected.selection.from
    );
    assert!(coordinates["summaryThroughEvent"]
        .as_u64()
        .is_some_and(|seq| seq < selected.selection.from));
    assert!(coordinates["lastMessageOpenedBeforeCut"].as_str().is_some());

    assert!(
        matches!(before.first(), Some(CodexInputItem::Message(message))
        if message.role == MessageRole::User && message.content == "use the tool")
    );
    let summary = before
        .iter()
        .find_map(|item| match item {
            CodexInputItem::Message(message) if message.content.starts_with(SUMMARY_LABEL) => {
                Some(&message.content)
            }
            _ => None,
        })
        .ok_or("summary absent")?;
    assert!(summary.contains("Later user corrections and the current input take precedence"));
    assert!(summary.contains("do not repeat completed or cancelled work"));
    assert_eq!(
        users(&before).last(),
        Some(&"cancel the old task; inspect only")
    );
    let calls: Vec<_> = before
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::ToolCall { call, .. } => Some(call),
            _ => None,
        })
        .collect();
    let results: Vec<_> = before
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::ToolOutput { call, .. } => Some(call),
            _ => None,
        })
        .collect();
    assert!(!calls.is_empty(), "fixture retained no exact exchange");
    assert_eq!(calls, results, "a retained exchange lost its pair");
    drop(continuation);
    fixture.close();
    let reopened = fixture.reopen(current.clone())?;
    assert_eq!(
        reopened.request_input("cancel the old task; inspect only", false)?,
        before
    );
    // A later input cannot migrate the selected current input into the prefix.
    let next: MessageId = "msg_original_later".parse().map_err(|_| "next")?;
    lock(&reopened.inbox)
        .store_mut()
        .elect_writer(&AggregateId::Session(fixture.session.clone()))
        .map_err(|error| error.to_string())?;
    promote_test_input(&reopened.inbox, &fixture.session, &next, "new objective")?;
    let next = DurableContinuation::new(
        reopened.inbox.clone(),
        reopened.artifacts.clone(),
        fixture.session.clone(),
        next,
    );
    let after = next.request_input("new objective", false)?;
    assert_eq!(&after[..before.len()], &before);
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn fork_and_history_inheriting_child_keep_originals_after_compaction() -> Result<(), String> {
    let mut fixture = Fixture::new("original-users-inheritance")?;
    let ids = CountingIds(AtomicU64::new(0x7200_0000));
    let clock = FixedClock;
    let authorities = grapt_types::DeterministicAuthorities::new(&clock, &ids);
    let seed = grapt_session::SessionSeed::new(
        ProjectId::new("prj_original_users").ok_or("project")?,
        "/work/original-users",
        "original users",
    )
    .map_err(|error| error.to_string())?;
    fixture.session = {
        let mut inbox = lock(fixture.inbox()?);
        grapt_session::create_session(
            inbox.store_mut(),
            &authorities,
            &seed,
            &mut [&mut grapt_session::SessionListProjector],
        )
        .map_err(|error| error.to_string())?
        .session
    };
    fixture.record_exchange()?;
    let current: MessageId = "msg_inherited_current".parse().map_err(|_| "current")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &current,
        "current objective",
    )?;
    let continuation = fixture.continuation(current)?;
    let id = grapt_types::CompactionId::new("original-inheritance").ok_or("compaction")?;
    let selected = continuation.prepare_summary("current objective", &id, 128_000)?;
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    let fork = {
        let mut inbox = lock(fixture.inbox()?);
        grapt_session::fork_session(
            inbox.store_mut(),
            &authorities,
            &fixture.session,
            &mut [&mut grapt_inbox::InboxProjector],
        )
        .map_err(|error| error.to_string())?
        .session
    };
    // This is the source boundary used by spawn_agent with inherited history:
    // forks rebuild the full durable conversation with fresh message IDs,
    // including the originals, but never the invoking tool call.
    let call = ToolCallId::new("original-child-call").ok_or("call")?;
    let mut owner = super::sustained_defects::step_owner(&fixture, 2)?;
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: call.clone(),
                tool: "spawn_agent".to_owned(),
                input: "{}".to_owned(),
                execution: ToolExecution::Local,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.to_string())?;
    drop(owner);
    let child = {
        let mut inbox = lock(fixture.inbox()?);
        grapt_session::fork_session_before_call(
            inbox.store_mut(),
            &authorities,
            &fixture.session,
            &call,
            &mut [&mut grapt_inbox::InboxProjector],
        )
        .map_err(|error| error.to_string())?
        .session
    };
    for (index, session) in [fork, child].into_iter().enumerate() {
        let input: MessageId = format!("msg_original_child_{index}")
            .parse()
            .map_err(|_| "input")?;
        promote_test_input(fixture.inbox()?, &session, &input, "child objective")?;
        let inherited = DurableContinuation::new(
            fixture.inbox()?.clone(),
            fixture.artifacts()?.clone(),
            session,
            input,
        );
        let input = inherited.request_input("child objective", false)?;
        assert_eq!(users(&input).first(), Some(&"use the tool"));
        assert_eq!(users(&input).last(), Some(&"child objective"));
        assert!(users(&input).contains(&"current objective"));
        let calls: Vec<_> = input
            .iter()
            .filter_map(|item| match item {
                CodexInputItem::ToolCall { call, .. } => Some(call),
                _ => None,
            })
            .collect();
        let results: Vec<_> = input
            .iter()
            .filter_map(|item| match item {
                CodexInputItem::ToolOutput { call, .. } => Some(call),
                _ => None,
            })
            .collect();
        assert!(!calls.is_empty());
        assert_eq!(calls, results, "inherited full history broke an exchange");
        assert!(!input.iter().any(|item| matches!(item, CodexInputItem::ToolCall { call: inherited, .. } if inherited == &call)));
    }
    Ok(())
}

#[test]
fn legacy_replay_stays_quoted_and_unknown_policy_refuses_instead_of_reinterpreting(
) -> Result<(), String> {
    for policy in ["scc-working-tail-v3", "scc-unknown-policy"] {
        let mut fixture = Fixture::new(policy)?;
        fixture.record_exchange()?;
        let current: MessageId = "msg_policy_current".parse().map_err(|_| "current")?;
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &current,
            "current objective",
        )?;
        let continuation = fixture.continuation(current.clone())?;
        let id = grapt_types::CompactionId::new("original-policy").ok_or("id")?;
        let selected = continuation.prepare_summary("current objective", &id, 128_000)?;
        super::summary_tests::commit_selection_with_policy(&fixture, &selected, id, policy)?;
        let before = continuation.request_input("current objective", false);
        if policy == "scc-unknown-policy" {
            assert!(before.is_err_and(|error| error.contains("unsupported retention policy")));
            continue;
        }
        let before = before?;
        let first = users(&before)
            .first()
            .copied()
            .ok_or("legacy facts absent")?;
        assert!(first.starts_with(super::super::retained_facts::FACTS_LABEL));
        assert!(first.contains("- \"use the tool\""));
        assert!(!before
            .iter()
            .any(|item| matches!(item, CodexInputItem::Message(message)
            if message.content.contains("Earlier user messages are historical"))));
        drop(continuation);
        fixture.close();
        let reopened = fixture.reopen(current)?;
        assert_eq!(reopened.request_input("current objective", false)?, before);
    }
    Ok(())
}

#[test]
fn every_original_counts_toward_combined_item_and_byte_bounds() -> Result<(), String> {
    let prefix = retained("one", &["two", "three", "current"])?;
    assert_eq!(prefix.len(), 3);
    assert!(super::super::context_error::retained_fit(
        0,
        MAX_CONTEXT_ITEMS - 3,
        &prefix,
        Some("summary"),
        &[],
        true
    )
    .is_err());
    assert!(super::super::context_error::retained_fit(
        MAX_CONTEXT_BYTES - 1,
        0,
        &prefix,
        Some("summary"),
        &[],
        true
    )
    .is_err());
    assert!(super::super::context_error::retained_fit(
        0,
        MAX_CONTEXT_ITEMS - 4,
        &prefix,
        Some("summary"),
        &[],
        true
    )
    .is_ok());
    Ok(())
}

#[test]
fn an_over_budget_seed_falls_back_to_summary_without_failing_continuation() -> Result<(), String> {
    let fixture = Fixture::new("original-users-fallback")?;
    fixture.record_exchange()?;
    let seed: MessageId = "msg_oversized_original".parse().map_err(|_| "seed")?;
    let text = "not reusable ".repeat(1400);
    promote_test_input(fixture.inbox()?, &fixture.session, &seed, &text)?;
    super::working_set::output_step(&fixture, 2, "seed acknowledged")?;
    let current: MessageId = "msg_fallback_current".parse().map_err(|_| "current")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &current,
        "continue from summary",
    )?;
    let continuation = fixture.continuation(current)?;
    let id = grapt_types::CompactionId::new("original-fallback").ok_or("id")?;
    let selected = continuation.prepare_summary("continue from summary", &id, 128_000)?;
    assert!(!users(&selected.prefix).contains(&text.as_str()));
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    let after = continuation.request_input("continue from summary", false)?;
    assert!(!users(&after).contains(&text.as_str()));
    assert_eq!(users(&after).last(), Some(&"continue from summary"));
    assert!(after
        .iter()
        .any(|item| matches!(item, CodexInputItem::Message(message)
        if message.role == MessageRole::Assistant && message.content.contains("Use tool."))));
    Ok(())
}

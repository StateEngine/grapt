//! A provider compaction in the continuation: its opaque items stand in for
//! the history they covered on the target that produced them, and any other
//! target, or an unreadable artifact, rebuilds from that durable history.
use super::*;
use grapt_provider::{CompactionTarget, ProtocolFamily};

const ITEM: &str = "PC_PROVIDER_COMPACTED";

fn target() -> Result<grapt_types::ResolvedModelTarget, String> {
    crate::dogfood::production::tests::projection_target()
}

/// Commit `selected` as a provider-bound descriptor whose artifact is `text`.
fn commit_provider(
    fixture: &Fixture,
    selected: &mut super::super::summarization::SummaryPreparation,
    compaction: grapt_types::CompactionId,
    text: &str,
) -> Result<(), String> {
    use grapt_types::{
        CompactionDescriptor, CompactionDescriptorParts, CompactionSummary, CompactionTrigger,
        ContextWindowId, NextRequestRef, RetainedHistory, RetentionPolicyVersion,
        SummaryCompatibility,
    };
    selected.selection.server = Some(super::super::retained::ServerOutcome::Committed {
        items: 1,
        bytes: ITEM.len(),
        input_tokens: Some(1200),
        output_tokens: Some(40),
        ignored_text_bytes: 0,
    });
    let capture = |text: String, sensitivity| -> Result<grapt_types::ArtifactRef, String> {
        let captured = fixture
            .artifacts()?
            .capture(
                Cursor::new(text.into_bytes()),
                CaptureBound::new(320 * 1024, 4096).ok_or("bound")?,
                ArtifactKind::CapturedOutput,
                sensitivity,
            )
            .map_err(|e| e.to_string())?;
        Ok(captured.reference().clone())
    };
    let summary = capture(text.to_owned(), Sensitivity::SecretBearing)?;
    let selection = capture(
        selected.selection.encode().to_string(),
        Sensitivity::Ordinary,
    )?;
    let compaction_name = compaction.as_str().to_owned();
    let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
        id: compaction,
        source_session: fixture.session.clone(),
        source_epoch: selected.source_epoch,
        target_window: ContextWindowId::new("provider-window").ok_or("window")?,
        target_epoch: selected.source_epoch.next().ok_or("epoch overflow")?,
        trigger: CompactionTrigger::PolicyThreshold,
        retention_policy: RetentionPolicyVersion::new(super::super::retained::RETENTION_POLICY)
            .ok_or("policy")?,
        summary: CompactionSummary::ProviderBound {
            artifact: summary,
            compatibility: SummaryCompatibility::bind(target()?.model_ref()),
        },
        retained: RetainedHistory::DeterministicSelection {
            descriptor: selection.clone(),
        },
        baseline: selection.clone(),
        next_request: NextRequestRef::new(StepNumber::new(2).ok_or("step")?, selection),
    })
    .map_err(|e| format!("{e:?}"))?;
    let mut inbox = lock(fixture.inbox()?);
    let aggregate = AggregateId::Session(fixture.session.clone());
    let held = inbox.store().held_writers();
    let event = format!("evt_{}", compaction_name.replace('-', "_"))
        .parse::<EventId>()
        .map_err(|e| e.to_string())?;
    let tx = inbox
        .store_mut()
        .governed_transaction()
        .map_err(|e| e.to_string())?;
    grapt_store::compaction::record(
        &tx,
        &event,
        held.token(&aggregate).map_err(|e| e.to_string())?,
        1_788_220_800_010,
        grapt_store::compaction::CompactionAttempt::Settled(&descriptor),
        &mut [],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?.discard();
    Ok(())
}

/// Commit one more Step of `fixture`'s turn: an assistant text of `text`.
/// `ids` seeds the Step's deterministic identities, distinct per Step.
fn record_text_step(fixture: &Fixture, step: u32, ids: u64, text: &str) -> Result<(), String> {
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        fixture.inbox()?.clone(),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(ids))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(step).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("pc-step-{step}")).ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
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
            .map_err(|error| error.to_string())?;
    }
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::Stop,
        })
        .map_err(|error| error.to_string())
}

fn items(family: ProtocolFamily) -> Result<String, String> {
    let state = grapt_provider::compaction_fixture_state(
        &serde_json::json!({"type":"compaction","id":"cmp_pc","encrypted_content":ITEM}),
        &CompactionTarget::of(&target()?, family),
    );
    Ok(serde_json::json!({"items":[state]}).to_string())
}

fn compactions(input: &[CodexInputItem]) -> usize {
    input
        .iter()
        .filter(|item| {
            matches!(item, CodexInputItem::ProviderReplay(replay) if replay.is_responses_compaction())
        })
        .count()
}

fn compacted(fixture: &mut Fixture, text: &str) -> Result<Vec<CodexInputItem>, String> {
    fixture.record_exchange()?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let before = continuation.request_input("use the tool", true)?;
    let id = grapt_types::CompactionId::new("pc-provider").ok_or("compaction")?;
    let mut selected = provider_preparation(fixture, &id, 128_000)?;
    commit_provider(fixture, &mut selected, id, text)?;
    Ok(before)
}

#[test]
fn sc_provider_compaction_stands_in_for_the_history_it_covered() -> Result<(), String> {
    let mut fixture = Fixture::new("sc-provider-stands")?;
    let before = compacted(&mut fixture, &items(ProtocolFamily::ChatGptCodex)?)?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    continuation.select_target(&target()?, ProtocolFamily::ChatGptCodex);
    let after = continuation.request_input("use the tool", true)?;
    assert_eq!(compactions(&after), 1, "{after:?}");
    assert!(after
        .iter()
        .any(|item| matches!(item, CodexInputItem::Message(message)
        if message.content.starts_with("[Compaction recovery reference]"))));

    assert_ne!(after, before);
    assert!(
        !after.iter().any(|item| matches!(item,
            CodexInputItem::Message(message) if message.content.starts_with(SUMMARY_LABEL))),
        "no text summary stands beside the provider's"
    );
    assert!(
        !format!("{after:?}").contains(ITEM),
        "opaque content never prints"
    );
    drop(continuation);
    fixture.close();
    let reopened = fixture.reopen(fixture.input.clone())?;
    reopened.select_target(&target()?, ProtocolFamily::ChatGptCodex);
    assert_eq!(reopened.request_input("use the tool", true)?, after);
    Ok(())
}

/// Another binding (here the other Responses family on the same target) and
/// an unreadable artifact both rebuild from the durable history the
/// compaction covered: the request is the one before compaction.
#[test]
fn sc_another_binding_or_a_corrupt_item_recovers_from_durable_history() -> Result<(), String> {
    for (label, text, family) in [
        (
            "binding",
            items(ProtocolFamily::ChatGptCodex)?,
            ProtocolFamily::OpenAiResponses,
        ),
        (
            "corrupt",
            "{not json".to_owned(),
            ProtocolFamily::ChatGptCodex,
        ),
        (
            "foreign",
            r#"{"items":["{}"]}"#.to_owned(),
            ProtocolFamily::ChatGptCodex,
        ),
    ] {
        let mut fixture = Fixture::new(&format!("sc-provider-{label}"))?;
        let before = compacted(&mut fixture, &text)?;
        let continuation = fixture.continuation(fixture.input.clone())?;
        continuation.select_target(&target()?, family);
        let after = continuation.request_input("use the tool", true)?;
        assert_eq!(compactions(&after), 0, "{label}: {after:?}");
        assert_eq!(after, before, "{label}: not rebuilt from durable history");
    }
    Ok(())
}

/// The public history states a provider compaction and its counts.
#[test]
fn sc_provider_compaction_notice_states_the_fact_and_counts() -> Result<(), String> {
    let mut fixture = Fixture::new("sc-provider-notice")?;
    compacted(&mut fixture, &items(ProtocolFamily::ChatGptCodex)?)?;
    let inbox = lock(fixture.inbox()?);
    let (_, _, descriptor) =
        grapt_store::compaction::latest_descriptor(inbox.store().connection(), &fixture.session)
            .map_err(|e| e.to_string())?
            .ok_or("descriptor")?;
    let notice = provider_compaction_notice(fixture.artifacts()?, &descriptor)?;
    assert!(
        notice.starts_with("[Compacted by the provider: 1 opaque item(s)"),
        "{notice}"
    );
    assert!(
        notice.contains("1200 input tokens") && notice.contains("40 output tokens"),
        "{notice}"
    );
    assert!(!notice.contains(ITEM));
    Ok(())
}

/// The selection records what provider compaction did, for counting.
#[test]
fn sc_selection_records_the_server_outcome() -> Result<(), String> {
    use super::super::retained::{Selection, ServerOutcome};
    for server in [
        ServerOutcome::Committed {
            items: 2,
            bytes: 900,
            input_tokens: Some(5),
            output_tokens: None,
            ignored_text_bytes: 0,
        },
        ServerOutcome::Committed {
            items: 3,
            bytes: 900,
            input_tokens: None,
            output_tokens: Some(7),
            ignored_text_bytes: 42,
        },
        ServerOutcome::Fallback {
            reason: "interrupted".to_owned(),
        },
    ] {
        let selection = Selection {
            session: "ses_sc".parse().map_err(|_| "session")?,
            input: "msg_sc".parse().map_err(|_| "input")?,
            compaction: grapt_types::CompactionId::new("sc").ok_or("id")?,
            from: 1,
            source_head: 2,
            admission: None,
            server: Some(server.clone()),
            summary_plan: None,
        };
        let encoded = selection.encode();
        // Ignored text is written only when there was some, so a commit
        // without it encodes as before.
        assert_eq!(
            encoded["serverSide"].get("ignoredTextBytes").is_some(),
            matches!(server, ServerOutcome::Committed { ignored_text_bytes, .. } if ignored_text_bytes > 0)
        );
        let decoded = Selection::decode(&encoded.to_string())?;
        assert_eq!(decoded.server, Some(server));
    }
    let committed = serde_json::json!({"session":"ses_sc","input":"msg_sc","compaction":"sc",
        "from":1,"source_head":2,"serverSide":{"outcome":"committed","items":1,"bytes":9,
        "inputTokens":null,"outputTokens":null}});
    for bad in [
        serde_json::json!(0),
        serde_json::json!("1"),
        serde_json::Value::Null,
    ] {
        let mut with = committed.clone();
        with["serverSide"]["ignoredTextBytes"] = bad.clone();
        assert!(
            Selection::decode(&with.to_string()).is_err(),
            "accepted ignored text {bad}"
        );
    }
    Ok(())
}

/// A crash after the artifact is captured but before the descriptor commits
/// leaves nothing in force: the descriptor is the only switch.
#[test]
fn sc_an_artifact_without_its_descriptor_changes_nothing() -> Result<(), String> {
    let fixture = Fixture::new("sc-provider-uncommitted")?;
    fixture.record_exchange()?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    continuation.select_target(&target()?, ProtocolFamily::ChatGptCodex);
    let before = continuation.request_input("use the tool", true)?;
    fixture
        .artifacts()?
        .capture(
            Cursor::new(items(ProtocolFamily::ChatGptCodex)?.into_bytes()),
            CaptureBound::new(320 * 1024, 4096).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(continuation.request_input("use the tool", true)?, before);
    Ok(())
}

/// SE's own summary after a provider compaction sets the opaque summary aside
/// and summarizes the durable history: the opaque content is never read as
/// client text (CONTEXT-QUALITY-01 J04, as revised for server compaction).
#[test]
fn sc_own_summary_never_reads_the_provider_summary_as_text() -> Result<(), String> {
    const OPAQUE: &str = "Opaque provider-held summary.";
    for (label, text) in [
        ("items", items(ProtocolFamily::ChatGptCodex)?),
        ("unreadable", OPAQUE.to_owned()),
    ] {
        let mut fixture = Fixture::new(&format!("sc-own-summary-{label}"))?;
        compacted(&mut fixture, &text)?;
        let continuation = fixture.continuation(fixture.input.clone())?;
        record_text_step(&fixture, 2, 100, &"Observed work. ".repeat(1000))?;
        let head = {
            let inbox = lock(fixture.inbox()?);
            grapt_store::compaction::latest_descriptor(inbox.store().connection(), &fixture.session)
                .map_err(|e| e.to_string())?
                .map(|(_, seq, _)| seq.value())
        };
        let id = grapt_types::CompactionId::new("sc-own-summary").ok_or("compaction")?;
        let selected = continuation.prepare_summary("use the tool", &id, 128_000)?;
        assert!(
            selected.text.contains("fixture output"),
            "{label}: the history the compaction covered was not summarized: {}",
            selected.text
        );
        for opaque in [OPAQUE, ITEM] {
            assert!(
                !selected.text.contains(opaque),
                "{label}: the provider summary was read as client text"
            );
        }
        let inbox = lock(fixture.inbox()?);
        let after = grapt_store::compaction::latest_descriptor(
            inbox.store().connection(),
            &fixture.session,
        )
        .map_err(|e| e.to_string())?
        .map(|(_, seq, _)| seq.value());
        assert_eq!(after, head, "{label}: preparing a summary committed one");
    }
    Ok(())
}

/// Every committed provider item counts against the request's item bound,
/// not one slot for the whole summary.
#[test]
fn sc_each_provider_item_counts_against_the_item_bound() {
    use super::super::exceeds_item_bound;
    let max = super::super::MAX_CONTEXT_ITEMS;
    let item = || {
        CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content: "item".to_owned(),
        })
    };
    let four: Vec<CodexInputItem> = (0..4).map(|_| item()).collect();
    // A text summary is one item: the bound is unchanged for it.
    assert!(!exceeds_item_bound(max - 1, 0, Some("summary"), &[]));
    assert!(exceeds_item_bound(max - 1, 1, Some("summary"), &[]));
    // A provider compaction with four items takes four: the limit, and the
    // first case over it.
    assert!(!exceeds_item_bound(max - 4, 0, None, &four));
    assert!(exceeds_item_bound(max - 3, 0, None, &four));
    assert!(exceeds_item_bound(max - 4, 1, None, &four));
}

/// The preparation the server-side path commits for a provider compaction.
fn provider_preparation(
    fixture: &Fixture,
    id: &grapt_types::CompactionId,
    context_tokens: u64,
) -> Result<super::super::summarization::SummaryPreparation, String> {
    fixture
        .continuation(fixture.input.clone())?
        .prepare_provider_summary("use the tool", id, context_tokens)
        .map_err(String::from)
}

/// Close `fixture`'s store and reopen it as a restarted service would: the
/// same database and artifacts, the Session's writer elected anew.
fn restart(fixture: &mut Fixture) -> Result<(), String> {
    fixture.close();
    let mut store =
        EventStore::open(&fixture.database, "d2a-continuation").map_err(|e| e.to_string())?;
    store
        .elect_writer(&AggregateId::Session(fixture.session.clone()))
        .map_err(|e| e.to_string())?;
    fixture.inbox = Some(Arc::new(Mutex::new(
        Inbox::new(store).map_err(|e| e.to_string())?,
    )));
    fixture.artifacts = Some(Arc::new(open_artifacts(&fixture.artifact_root)?));
    Ok(())
}

/// A 20 000-byte summary budget: three of the rounds' Steps never fit one.
const RETENTION_CONTEXT_TOKENS: u64 = 16_384 + 20_000;

/// The fixture after its rounds, each round's cut, and the request each
/// compaction leaves.
type Rounds = (Fixture, Vec<u64>, Vec<Vec<CodexInputItem>>);

/// Four provider compactions over one long turn, each after two more large
/// Steps, restarting the service after round `restart_after` when given.
fn retention_rounds(label: &str, restart_after: Option<u32>) -> Result<Rounds, String> {
    let step = "Observed work. ".repeat(1000);
    let mut fixture = Fixture::new(label)?;
    fixture.record_exchange()?;
    let (mut froms, mut requests) = (Vec::new(), Vec::new());
    for round in 0..4u32 {
        // Two Steps between compactions: SE's own summary would keep the
        // newer as an exact tail, which the provider's item already covers.
        if round > 0 {
            for step_number in [2 * round, 2 * round + 1] {
                record_text_step(&fixture, step_number, 1000 * u64::from(step_number), &step)?;
            }
        }
        let id = grapt_types::CompactionId::new(format!("pc-round-{round}")).ok_or("id")?;
        let mut selected = provider_preparation(&fixture, &id, RETENTION_CONTEXT_TOKENS)?;
        froms.push(selected.selection.from);
        commit_provider(
            &fixture,
            &mut selected,
            id,
            &items(ProtocolFamily::ChatGptCodex)?,
        )?;
        let request = |fixture: &Fixture| -> Result<Vec<CodexInputItem>, String> {
            let continuation = fixture.continuation(fixture.input.clone())?;
            continuation.select_target(&target()?, ProtocolFamily::ChatGptCodex);
            continuation.request_input("use the tool", true)
        };
        let next = request(&fixture)?;
        if restart_after == Some(round) {
            restart(&mut fixture)?;
            assert_eq!(
                request(&fixture)?,
                next,
                "round {round}: the restarted replay differs"
            );
        }
        requests.push(next);
    }
    Ok((fixture, froms, requests))
}

/// The Luna A/B: later provider
/// compactions kept retaining from one old cut while input climbed. The
/// provider's item stands in for the whole request it compacted, so EVERY
/// successful provider compaction retains from past the prior one's cut,
/// however long that history is to summarize as text.
#[test]
fn sc_every_provider_compaction_retains_from_past_the_prior_cut() -> Result<(), String> {
    let (fixture, froms, requests) = retention_rounds("sc-provider-retention", None)?;
    assert!(
        froms.windows(2).all(|pair| pair[1] > pair[0]),
        "a provider compaction did not retain past the prior cut: {froms:?}"
    );
    // The relief: each next request is the provider's item, not the Steps
    // it already covers.
    for (round, next) in requests.iter().enumerate() {
        assert_eq!(compactions(next), 1, "round {round}: {next:?}");
        assert!(
            !format!("{next:?}").contains("Observed work."),
            "round {round}: a covered Step was replayed beside the provider's item"
        );
    }
    // Nothing new past the last cut, though the descriptor itself is a later
    // event: refused by name, which the run records as its fallback reason.
    let id = grapt_types::CompactionId::new("pc-round-idle").ok_or("id")?;
    let refused = provider_preparation(&fixture, &id, RETENTION_CONTEXT_TOKENS)
        .err()
        .ok_or("an idle provider compaction was prepared")?;
    assert_eq!(
        refused,
        "compaction recent projection failed: compaction conversation is empty"
    );
    Ok(())
}

/// The review counterexample (HOLD R-H1): provider rounds of
/// 200 kB text Steps carry well past the 4 MiB raw-history bound while the
/// coding request itself stays a few items. Every compaction still retains
/// past the prior cut: the history a provider item covers is not read back.
#[test]
fn sc_provider_retention_stays_bounded_past_the_raw_history_wall() -> Result<(), String> {
    let fixture = Fixture::new("sc-provider-retention-long")?;
    fixture.record_exchange()?;
    let text = "Observed work. ".repeat(13_334);
    let mut prior = None;
    for round in 0..25u32 {
        if round > 0 {
            record_text_step(&fixture, round + 1, 1000 * u64::from(round), &text)?;
        }
        let continuation = fixture.continuation(fixture.input.clone())?;
        continuation.select_target(&target()?, ProtocolFamily::ChatGptCodex);
        let before = continuation.request_input("use the tool", true)?;
        assert!(
            before.len() < 16,
            "round {round}: the coding request is bounded: {} items",
            before.len()
        );
        let id = grapt_types::CompactionId::new(format!("pc-long-{round}")).ok_or("id")?;
        let mut selected = provider_preparation(&fixture, &id, 128_000).map_err(|error| {
            format!(
                "round {round}, prior cut {prior:?}, {} request items: {error}",
                before.len()
            )
        })?;
        if let Some(prior) = prior {
            assert!(
                selected.selection.from > prior,
                "round {round}: retained from {}, not past {prior}",
                selected.selection.from
            );
        }
        prior = Some(selected.selection.from);
        commit_provider(
            &fixture,
            &mut selected,
            id,
            &items(ProtocolFamily::ChatGptCodex)?,
        )?;
    }
    Ok(())
}

/// A restart mid-turn, right after the first
/// provider compaction, replays the same request from the durable
/// descriptor; the second compaction's cut is past the first's, and every
/// later compaction selects the cut it would have without the restart.
#[test]
fn sc_a_restart_between_provider_compactions_keeps_every_cut() -> Result<(), String> {
    let (_, froms, requests) = retention_rounds("sc-provider-retention-steady", None)?;
    let (_, restarted, restarted_requests) =
        retention_rounds("sc-provider-retention-restart", Some(0))?;
    assert!(
        restarted[1] > restarted[0],
        "after the restart, compaction 2 retained from {} at or before compaction 1's cut {}",
        restarted[1],
        restarted[0]
    );
    assert_eq!(restarted, froms);
    assert_eq!(restarted_requests, requests);
    Ok(())
}

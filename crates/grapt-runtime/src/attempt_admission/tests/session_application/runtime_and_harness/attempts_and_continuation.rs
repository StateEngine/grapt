use std::sync::atomic::Ordering;
use std::sync::Arc;

use grapt_provider::{ProviderFailureClass, ScriptBuilder};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    DeliveryMode, ProviderFailureClass as TerminalProviderFailureClass, StepTerminalReason,
};

use super::super::{
    admit, event_count, locked, seed, session, transcript, within, Gate, CREATED_MS,
};
use super::{
    allowance, build, fast_policy, open_file_inbox, open_inbox, stop_script, tool_script,
    LocationSpec,
};
use crate::{CoordinatorError, DurableFact};

// ---------------------------------------------------------------------------
// F) One provider stream per physical attempt
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_opens_a_new_attempt_and_never_a_second_stream_on_the_old_one() {
    // Contract rule 1. The first stream fails retryably before any visible
    // output, so the driver retries — and the retry must mint a NEW attempt.
    // `DeterministicAdapter` refuses a reused identity, so a second stream on
    // one attempt would be recorded rather than silently served.
    let inbox = open_inbox();
    let ses = session("retry");
    seed(&inbox, &ses, "/work/retry");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let failing = ScriptBuilder::new().fail(
        ProviderFailureClass::Transport,
        None,
        "connection reset before first byte",
    );
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/retry",
            LocationSpec {
                scripts: vec![failing, stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(2),
        allowance(2),
    );

    let woke = within("wake", harness.coordinator.wake(ses.clone())).await;
    assert!(woke.is_ok(), "the retry must succeed: {woke:?}");

    let log = locked(&runtimes[0].log);
    assert_eq!(log.opened.len(), 2, "two physical attempts");
    assert_eq!(log.streams.len(), 2, "two streams, one per attempt");
    assert!(
        log.reuse_errors.is_empty(),
        "an attempt identity was reused: {:?}",
        log.reuse_errors
    );
    let mut distinct = log.streams.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 2, "attempt identities must be distinct");
    let ordinals: Vec<u32> = log.opened.iter().map(|(_, ordinal, _)| *ordinal).collect();
    assert_eq!(
        ordinals,
        vec![1, 2],
        "a retry is a new attempt at the SAME step, not a new step"
    );
    let steps: Vec<u32> = log.opened.iter().map(|(step, _, _)| *step).collect();
    assert_eq!(steps, vec![1, 1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn a_failed_step_uses_one_pre_attempt_context_across_physical_retries() {
    let (dir, db, inbox) = open_file_inbox("stable-context");
    let ses = session("durablefailure");
    seed(&inbox, &ses, "/work/durablefailure");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);
    let gate = Gate::new();

    let retryable = ScriptBuilder::new().fail(
        ProviderFailureClass::Transport,
        None,
        "connection reset before first byte",
    );
    let terminal = ScriptBuilder::new().fail(
        ProviderFailureClass::Authentication,
        None,
        "credential refused",
    );
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/durablefailure",
            LocationSpec {
                scripts: vec![retryable, terminal],
                gate: Some(gate.clone()),
                record_only: true,
                ..LocationSpec::default()
            },
        )],
        fast_policy(2),
        allowance(2),
    );

    let coordinator = Arc::clone(&harness.coordinator);
    let run_session = ses.clone();
    let wake = tokio::spawn(async move { coordinator.wake(run_session).await });

    gate.wait().await;
    assert_eq!(
        harness.ids.minted(),
        2,
        "promotion and Step settlement each mint once before provider I/O"
    );
    assert_eq!(
        harness.clock.calls(),
        4,
        "three eligibility passes and Step settlement each read time once before provider I/O"
    );
    gate.open();

    gate.wait().await;
    assert_eq!(
        harness.ids.minted(),
        2,
        "a physical retry must not mint another Step event identity"
    );
    assert_eq!(
        harness.clock.calls(),
        4,
        "a physical retry must not recapture the Step timestamp"
    );
    gate.open();

    let outcome = match within("durable failed wake", wake).await {
        Ok(outcome) => outcome,
        Err(error) => panic!("durable failed wake task must join: {error}"),
    };
    assert!(
        matches!(
            outcome,
            Err(CoordinatorError::Run { .. } | CoordinatorError::ProviderFailed { .. })
        ),
        "the provider failure stays an application failure: {outcome:?}"
    );

    let log = locked(&runtimes[0].log);
    assert_eq!(log.opened.len(), 2, "two physical attempts ran");
    assert_eq!(
        log.opened
            .iter()
            .map(|(step, _, _)| *step)
            .collect::<Vec<_>>(),
        vec![1, 1],
        "both attempts belong to one chain-local Step"
    );
    drop(log);

    let observer = match rusqlite::Connection::open(&db) {
        Ok(observer) => observer,
        Err(error) => panic!("a fresh observer must reopen durable truth: {error}"),
    };
    let row: (String, i64, String) = match observer.query_row(
        "SELECT id, created, data FROM event WHERE aggregate_id = ?1 AND type = 'session.step.failed.3'",
        [ses.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ) {
        Ok(row) => row,
        Err(error) => panic!("the durable StepFailedV2 row must reopen: {error}"),
    };
    assert_eq!(row.0, "evt_mint1", "the one pre-attempt EventId is stored");
    assert_eq!(row.1, CREATED_MS, "the one pre-attempt timestamp is stored");
    let value: serde_json::Value = match serde_json::from_str(&row.2) {
        Ok(value) => value,
        Err(error) => panic!("stored StepFailedV2 data must be JSON: {error}"),
    };
    let observed = match grapt_event::session_step_observed::decode_failed(&value) {
        Ok(decoded) => decoded,
        Err(error) => panic!("stored StepFailedV2 data must decode: {error}"),
    };
    let decoded = observed.fact();
    assert_eq!(decoded.step().value(), 1);
    assert!(matches!(
        decoded.reason(),
        StepTerminalReason::ProviderFailure {
            class: TerminalProviderFailureClass::Authentication
        }
    ));
    assert!(
        value.get("detail").is_none(),
        "diagnostic detail stays operational"
    );
    assert!(
        value.get("sessionID").is_none(),
        "Session stays in the envelope"
    );
    let durable_count: i64 = match observer.query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = 'session.step.failed.3'",
        [ses.as_str()],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => panic!("durable Step failure count must read: {error}"),
    };
    assert_eq!(durable_count, 1, "one logical Step has one durable failure");
    let notifications = locked(&runtimes[0].notifications);
    assert_eq!(
        notifications
            .iter()
            .filter(|fact| matches!(fact, DurableFact::StepFailed { .. }))
            .count(),
        1,
        "Committed produces one existing post-commit advisory"
    );
    drop(notifications);
    drop(observer);
    drop(runtimes);
    drop(harness);
    drop(inbox);
    if let Err(error) = std::fs::remove_dir_all(&dir) {
        panic!("file-backed stable-context witness must clean up: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transferred_epoch_refuses_step_settlement_without_self_election() {
    let (dir, db, inbox) = open_file_inbox("stale-writer");
    let ses = session("stalewriter");
    seed(&inbox, &ses, "/work/stalewriter");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);
    let gate = Gate::new();

    let terminal = ScriptBuilder::new().fail(
        ProviderFailureClass::Authentication,
        None,
        "credential refused",
    );
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/stalewriter",
            LocationSpec {
                scripts: vec![terminal],
                gate: Some(gate.clone()),
                record_only: true,
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    let coordinator = Arc::clone(&harness.coordinator);
    let run_session = ses.clone();
    let wake = tokio::spawn(async move { coordinator.wake(run_session).await });
    gate.wait().await;

    let aggregate = AggregateId::Session(ses.clone());
    let mut successor = match EventStore::open(&db, "r2b3-successor-owner") {
        Ok(store) => store,
        Err(error) => panic!("successor store must open: {error}"),
    };
    if let Err(error) = successor.elect_writer(&aggregate) {
        panic!("successor must transfer the Session epoch: {error}");
    }
    gate.open();

    let outcome = match within("stale-writer wake", wake).await {
        Ok(outcome) => outcome,
        Err(error) => panic!("stale-writer wake task must join: {error}"),
    };
    match outcome {
        Err(CoordinatorError::Run { reason, .. }) => {
            assert!(
                reason.contains("owner refused"),
                "OwnerRejected must surface: {reason}"
            );
            assert!(
                reason.contains("StepFailedV2 append refused"),
                "the store refusal remains distinct from provider failure: {reason}"
            );
            assert!(
                !reason.contains("DurabilityFailure"),
                "a refused write cannot fabricate durable failure: {reason}"
            );
        }
        other => panic!("stale settlement must become OwnerRejected, got {other:?}"),
    }
    assert_eq!(
        event_count(&inbox, &ses, "session.step.failed.3"),
        0,
        "the stale owner writes no Step-failure row"
    );
    assert_eq!(
        locked(&runtimes[0].log).opened.len(),
        1,
        "OwnerRejected opens no later provider Step"
    );
    let notifications = locked(&runtimes[0].notifications);
    assert!(
        notifications
            .iter()
            .all(|fact| !matches!(fact, DurableFact::StepFailed { .. })),
        "a refused commit emits no Step-failure advisory"
    );
    drop(notifications);
    drop(successor);
    drop(runtimes);
    drop(harness);
    drop(inbox);
    if let Err(error) = std::fs::remove_dir_all(&dir) {
        panic!("file-backed stale-writer witness must clean up: {error}");
    }
}

// ---------------------------------------------------------------------------
// G) Continuation reloads projected history before it continues
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_calling_step_continues_only_after_history_is_reloaded() {
    let inbox = open_inbox();
    let ses = session("chain");
    seed(&inbox, &ses, "/work/chain");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/chain",
            LocationSpec {
                scripts: vec![tool_script("call_1", "read"), stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(4),
    );

    let woke = within("wake", harness.coordinator.wake(ses.clone())).await;
    assert!(woke.is_ok(), "the chain must complete: {woke:?}");

    assert_eq!(
        runtimes[0].reloads.load(Ordering::SeqCst),
        1,
        "exactly one reload, before the one continuation"
    );
    assert_eq!(
        locked(&runtimes[0].tools.calls).clone(),
        vec!["read".to_owned()],
        "the tool ran on this Location"
    );

    let log = locked(&runtimes[0].log);
    let steps: Vec<u32> = log.opened.iter().map(|(step, _, _)| *step).collect();
    assert_eq!(steps, vec![1, 2], "two Steps, each with one attempt");
    let generations: Vec<u64> = log
        .opened
        .iter()
        .map(|(_, _, generation)| *generation)
        .collect();
    assert!(
        generations[1] > generations[0],
        "the continuation must observe newer history: {generations:?}"
    );
    assert_eq!(
        transcript(&harness.inbox, &ses),
        vec!["msg_a".to_owned()],
        "a continuation is a new Step, never a re-promoted input"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_that_keeps_calling_tools_stops_at_its_allowance_rather_than_looping() {
    let inbox = open_inbox();
    let ses = session("bound");
    seed(&inbox, &ses, "/work/bound");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, _) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/bound",
            LocationSpec {
                scripts: vec![tool_script("call_1", "read"), tool_script("call_2", "read")],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    let woke = within("wake", harness.coordinator.wake(ses.clone())).await;
    match woke {
        Err(CoordinatorError::StepBudgetExhausted {
            session,
            policy,
            spent,
            granted,
        }) => {
            assert_eq!(session, ses, "the exhausted budget names its own Session");
            assert_eq!(policy.as_str(), "step-allowance-v1");
            assert_eq!(spent, 2, "the exhausted budget names spent Steps");
            assert_eq!(granted, 2, "the exhausted budget names granted Steps");
        }
        other => panic!("expected the typed Step budget refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn continued_steps_keep_context_until_the_chain_terminal_closes_once() {
    let inbox = open_inbox();
    let ses = session("contextclose");
    seed(&inbox, &ses, "/work/contextclose");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/contextclose",
            LocationSpec {
                scripts: vec![tool_script("context-close-call", "read"), stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(2),
        allowance(2),
    );
    let woke = within("wake", harness.coordinator.wake(ses)).await;
    assert!(woke.is_ok(), "chain failed: {woke:?}");
    let log = locked(&runtimes[0].log);
    assert_eq!(log.opened.len(), 2);
    assert_eq!(
        log.shutdowns, 1,
        "context was abandoned or closed between continued Steps"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invocation_cap_counts_logical_steps_and_survives_a_steer(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for (input_cap, agent_cap) in [(Some(2), None), (None, Some(2)), (Some(5), Some(2))] {
        let inbox = open_inbox();
        let ses = session("invocationcap");
        seed(&inbox, &ses, "/work/invocationcap");
        let event = "evt_cap_input".parse()?;
        let input = "msg_cap_input".parse()?;
        locked(&inbox).admit_with_options(
            grapt_inbox::AdmitRequest {
                event_id: &event,
                session: &ses,
                input: &input,
                text: "cap",
                delivery: DeliveryMode::Queue,
                resume: true,
                created_ms: CREATED_MS,
            },
            grapt_inbox::UserInputOptions {
                max_steps: input_cap.and_then(std::num::NonZeroU32::new),
                ..Default::default()
            },
        )?;
        let gate = Gate::new();
        let failing =
            ScriptBuilder::new().fail(ProviderFailureClass::Transport, None, "retry before output");
        let (harness, runtimes) = build(
            Arc::clone(&inbox),
            vec![(
                "/work/invocationcap",
                LocationSpec {
                    step_limit: agent_cap.and_then(std::num::NonZeroU32::new),
                    scripts: vec![
                        failing,
                        tool_script("cap_1", "read"),
                        tool_script("cap_2", "read"),
                        stop_script(),
                    ],
                    gate: Some(gate.clone()),
                    ..LocationSpec::default()
                },
            )],
            fast_policy(2),
            allowance(8),
        );
        let coordinator = Arc::clone(&harness.coordinator);
        let target = ses.clone();
        let drain = tokio::spawn(async move { coordinator.wake(target).await });
        gate.wait().await; // first physical attempt
        admit(&inbox, &ses, "capsteer", DeliveryMode::Steer);
        for _ in 0..8 {
            gate.open();
        }
        let result = within("capped drain", drain).await?;
        assert!(
            matches!(
                result,
                Err(CoordinatorError::StepBudgetExhausted {
                    spent: 2,
                    granted: 2,
                    ..
                })
            ),
            "{result:?}"
        );
        let log = locked(&runtimes[0].log);
        let steps: Vec<_> = log.opened.iter().map(|(step, _, _)| *step).collect();
        assert_eq!(
            steps,
            [1, 1, 2],
            "a retry shares the Step; no third Step opens"
        );
        assert!(log.reuse_errors.is_empty());
    }
    Ok(())
}

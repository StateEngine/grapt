use std::sync::Arc;
use std::time::Duration;

use grapt_types::DeliveryMode;

use super::super::{
    admit, close_gate, locked, pending_count, seed, session, settles, transcript, within, Gate,
};
use super::{allowance, build, fast_policy, open_inbox, ran, stop_script, LocationSpec};
use crate::{CoordinatorError, InterruptOutcome, WakeOutcome};

// ---------------------------------------------------------------------------
// C) Steer/queue boundary behaviour, over the real inbox
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn steers_promote_before_queued_work_and_one_queued_input_per_idle_boundary() {
    // Seed-fixture laws 2 and 3: steers promote before queued work, and an
    // idle boundary promotes ONE queued input only after steers are drained.
    let inbox = open_inbox();
    let ses = session("order");
    seed(&inbox, &ses, "/work/order");
    // Admitted queue-first on purpose: order must come from delivery mode, not
    // from admission order.
    admit(&inbox, &ses, "q1", DeliveryMode::Queue);
    admit(&inbox, &ses, "q2", DeliveryMode::Queue);
    admit(&inbox, &ses, "s1", DeliveryMode::Steer);
    admit(&inbox, &ses, "s2", DeliveryMode::Steer);

    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/order",
            LocationSpec {
                // Three chains: the steer BATCH is answered by one, then one
                // per queued input.
                scripts: (0..3).map(|_| stop_script()).collect(),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    let outcome = within("wake", harness.coordinator.wake(ses.clone())).await;
    match outcome {
        Ok(WakeOutcome::Drained {
            promotions,
            steps,
            placement_lookups,
            ..
        }) => {
            assert_eq!(promotions, 4);
            assert_eq!(
                steps, 3,
                "one Step answers the steer batch; one per queued input"
            );
            assert_eq!(placement_lookups, 1, "once per drain, not per promotion");
        }
        other => panic!("expected four promotions, got {other:?}"),
    }

    let order = vec![
        "msg_s1".to_owned(),
        "msg_s2".to_owned(),
        "msg_q1".to_owned(),
        "msg_q2".to_owned(),
    ];
    assert_eq!(
        transcript(&harness.inbox, &ses),
        order,
        "steers promote before queued work, in durable order"
    );
    assert_eq!(
        ran(&runtimes[0]),
        vec![
            "msg_s1".to_owned(),
            "msg_q1".to_owned(),
            "msg_q2".to_owned()
        ],
        "the second steer JOINED the first steer's chain rather than opening one"
    );
    // The batch really was one input event to the model: the chain answering
    // the steers saw both of them.
    let contexts = locked(&runtimes[0].log).contexts.clone();
    assert_eq!(
        contexts.first().map(Vec::as_slice),
        Some(["msg_s1".to_owned(), "msg_s2".to_owned()].as_slice()),
        "the first request carried the whole steer batch: {contexts:?}"
    );
    assert_eq!(pending_count(&harness.inbox, &ses), 0);
}

// ---------------------------------------------------------------------------
// D) Interrupt
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupt_ends_the_drain_with_no_further_promotion_and_no_loss() {
    let inbox = open_inbox();
    let ses = session("stop");
    seed(&inbox, &ses, "/work/stop");
    admit(&inbox, &ses, "s1", DeliveryMode::Steer);
    admit(&inbox, &ses, "s2", DeliveryMode::Steer);
    admit(&inbox, &ses, "q1", DeliveryMode::Queue);

    let gate = Gate::new();
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/stop",
            LocationSpec {
                scripts: (0..3).map(|_| stop_script()).collect(),
                gate: Some(gate.clone()),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    let drain = {
        let coordinator = Arc::clone(&harness.coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };

    // Exact cut: the first attempt's reader is open and BLOCKED mid-read. The
    // gate is deliberately never opened — cancellation reaches the reader
    // directly (contract §"Cancellation ownership"), so a hung read is
    // abandoned rather than waited out. Releasing the gate here would race the
    // cancel branch against a stream that can finish cleanly, and the witness
    // would sometimes observe the interrupt one Step later than it happened.
    gate.wait().await;
    assert_eq!(
        harness.coordinator.interrupt(&ses),
        Ok(InterruptOutcome::Interrupted)
    );

    let outcome = match within("drain", drain).await {
        Ok(Ok(outcome)) => outcome,
        other => panic!("drain failed: {other:?}"),
    };
    match outcome {
        WakeOutcome::Drained {
            promotions, steps, ..
        } => {
            assert_eq!(promotions, 2, "the steer batch, and nothing after it");
            assert_eq!(steps, 1, "the interrupted Step, and no Step after it");
        }
        other => panic!("expected the steer batch, got {other:?}"),
    }

    assert_eq!(
        ran(&runtimes[0]),
        vec!["msg_s1".to_owned()],
        "one chain opened, and no chain after the interrupt"
    );
    assert_eq!(
        transcript(&harness.inbox, &ses),
        vec!["msg_s1".to_owned(), "msg_s2".to_owned()],
        "the batch the interrupted Step was answering is transcript history"
    );
    assert_eq!(
        pending_count(&harness.inbox, &ses),
        1,
        "the QUEUED input never reached an idle boundary, so it stays pending"
    );
    assert!(!harness.coordinator.is_locally_draining(&ses));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborting_the_owning_wake_leaves_no_live_reader_and_no_overlapping_replacement() {
    // An interrupt is the POLITE end of a drain: the owner is still there to
    // hear it. This witness covers the impolite one — the owning wake's task is
    // aborted outright, which is what a shutdown, a dropped request, or a
    // cancelled caller does.
    //
    // Two things must hold afterwards, and neither is visible from the drain's
    // own result, because there is no result:
    //
    //   1. No provider reader is still live. A reader is spawned, so it does
    //      NOT die with the frame that spawned it unless that frame is made to
    //      own it: `tokio::spawn` detaches, and a dropped `JoinHandle` leaves
    //      the task running.
    //   2. The replacement drain the coordinator now admits does not overlap
    //      it. Releasing the Session while a reader still streams is exactly
    //      how one Session ends up with two.
    let inbox = open_inbox();
    let ses = session("abort");
    seed(&inbox, &ses, "/work/abort");
    admit(&inbox, &ses, "a1", DeliveryMode::Steer);

    let gate = Gate::new();
    let (close_gate, cleanup) = close_gate();
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/abort",
            LocationSpec {
                scripts: (0..3).map(|_| stop_script()).collect(),
                gate: Some(gate.clone()),
                close_gate: Some(Arc::clone(&close_gate)),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );
    let readers = Arc::clone(&runtimes[0].readers);

    let first = {
        let coordinator = Arc::clone(&harness.coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };

    // The cut: the first attempt's reader is open and blocked mid-read. The
    // gate is never opened, so the reader is genuinely still there to leak.
    gate.wait().await;
    assert_eq!(readers.live(), 1, "the attempt's reader is open");

    // The replacement's work is admitted BEFORE the abort, so at the instant
    // the coordinator would permit a claim there is something to claim it for.
    admit(&inbox, &ses, "a2", DeliveryMode::Steer);

    // Abort, and wait only for the wake's own future to be dropped — that drop
    // is what runs the ownership guard.
    first.abort();
    let _ = first.await;

    // NO wait for the reader here, deliberately. Racing the replacement in at
    // exactly this instant is the point: the guard has run, so a coordinator
    // that freed the Session on an abort REQUEST would let this drain open a
    // second provider reader beside the first. An earlier version of this
    // witness waited for `live() == 0` before starting the replacement, which
    // supplied by hand the very ordering the coordinator is supposed to
    // guarantee — and so passed whether or not it did.
    // Cleanup is now UNDERWAY AND HELD: the reader's destructor has begun and
    // is parked, so the abandoned reader still counts as live.
    cleanup.wait_entered();
    assert_eq!(
        readers.live(),
        1,
        "the abandoned reader is still being destroyed"
    );

    let second = {
        let coordinator = Arc::clone(&harness.coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };

    // The replacement must get NOWHERE while that is true. This is the whole
    // finding: the ownership guard has already run, so a coordinator that
    // released the Session on an abort request would let this drain open its
    // reader right here, beside one that still exists.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        readers.created(),
        1,
        "a replacement drain opened a reader while the abandoned one was still being destroyed"
    );

    // Let cleanup finish. Only now may the replacement proceed.
    cleanup.release();

    // The replacement reaches the same cut with its OWN reader.
    gate.wait().await;
    assert_eq!(
        readers.created(),
        2,
        "the replacement opened a second reader"
    );
    assert_eq!(
        readers.order(),
        vec![
            "open(1)".to_owned(),
            "close(1)".to_owned(),
            "open(2)".to_owned()
        ],
        "the abandoned reader must be GONE before the replacement's is opened"
    );
    assert_eq!(
        readers.peak(),
        1,
        "one Session, one live reader — never two at once"
    );

    second.abort();
    let _ = second.await;
    settles("the replacement's reader", || readers.live() == 0).await;
    assert_eq!(readers.peak(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abandoned_drain_answers_its_joiners_before_its_cleanup_finishes() {
    // Deferring the CLAIM must not defer the ANSWER. A joiner is waiting on a
    // result, not on a resource; making it wait for the abandoned drain's
    // reader to unwind would be a regression dressed up as safety.
    let inbox = open_inbox();
    let ses = session("joinabort");
    seed(&inbox, &ses, "/work/joinabort");
    admit(&inbox, &ses, "j1", DeliveryMode::Steer);

    let gate = Gate::new();
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/joinabort",
            LocationSpec {
                scripts: (0..2).map(|_| stop_script()).collect(),
                gate: Some(gate.clone()),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );
    let readers = Arc::clone(&runtimes[0].readers);

    let owner = {
        let coordinator = Arc::clone(&harness.coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    gate.wait().await;

    // A second caller joins the drain in flight.
    let joiner = {
        let coordinator = Arc::clone(&harness.coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    settles("the joiner to register", || {
        harness.coordinator.has_pending_wake(&ses)
    })
    .await;

    owner.abort();
    let _ = owner.await;

    // Answered now, while the reader is still unwinding.
    match within("joiner", joiner).await {
        Ok(Err(CoordinatorError::OwnerAbandoned { .. })) => {}
        other => panic!("a joiner must learn the owner was abandoned: {other:?}"),
    }
    settles("the abandoned reader", || readers.live() == 0).await;
}

// ---------------------------------------------------------------------------
// E) Store failures
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_store_read_failure_is_never_reported_as_an_unknown_session() {
    let inbox = open_inbox();
    let ses = session("readfail");
    seed(&inbox, &ses, "/work/readfail");
    let (harness, _) = build(Arc::clone(&inbox), Vec::new(), fast_policy(1), allowance(2));

    // Interrupt the existence check through the supported read hook.
    {
        let mut guard = locked(&inbox);
        if let Err(error) = guard.store_mut().progress_handler(1, Some(|| true)) {
            panic!("installing the read fault failed: {error}");
        }
    }

    let woke = within("wake", harness.coordinator.wake(ses.clone())).await;
    assert!(
        matches!(woke, Err(CoordinatorError::Inbox { .. })),
        "a store failure must surface as a store error: {woke:?}"
    );
    assert!(
        !matches!(woke, Err(CoordinatorError::UnknownSession { .. })),
        "never UnknownSession"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_only_store_fails_the_promotion_rather_than_reporting_no_work() {
    let inbox = open_inbox();
    let ses = session("readonly");
    seed(&inbox, &ses, "/work/readonly");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, _) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/readonly",
            LocationSpec {
                scripts: vec![stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    // A genuinely refusing store: every write now fails at SQLite.
    {
        let guard = locked(&inbox);
        if let Err(error) = guard
            .store()
            .connection()
            .execute_batch("PRAGMA query_only = ON")
        {
            panic!("query_only must apply: {error}");
        }
    }

    let woke = within("wake", harness.coordinator.wake(ses.clone())).await;
    assert!(
        matches!(woke, Err(CoordinatorError::Inbox { .. })),
        "a failing promotion must surface, never read as an empty drain: {woke:?}"
    );
    assert!(
        transcript(&harness.inbox, &ses).is_empty(),
        "nothing entered the transcript"
    );
    assert_eq!(
        pending_count(&harness.inbox, &ses),
        1,
        "the input is still durably pending"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_durable_fact_fails_the_run_rather_than_settling_it() {
    let inbox = open_inbox();
    let ses = session("refuse");
    seed(&inbox, &ses, "/work/refuse");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, _) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/refuse",
            LocationSpec {
                scripts: vec![stop_script()],
                // The very first fact — the attempt-start fact — is refused.
                refuse_commits_after: Some(0),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    let woke = within("wake", harness.coordinator.wake(ses.clone())).await;
    match woke {
        Err(CoordinatorError::Run { reason, .. }) => {
            assert!(
                reason.contains("owner refused"),
                "a store refusal must not be relabelled: {reason}"
            );
        }
        other => panic!("expected a run failure, got {other:?}"),
    }
    assert!(!harness.coordinator.is_locally_draining(&ses));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_location_that_cannot_open_a_chain_fails_the_run() {
    let inbox = open_inbox();
    let ses = session("nochain");
    seed(&inbox, &ses, "/work/nochain");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, _) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/nochain",
            LocationSpec {
                scripts: vec![stop_script()],
                refuse_chain: true,
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    assert!(matches!(
        within("wake", harness.coordinator.wake(ses.clone())).await,
        Err(CoordinatorError::Run { .. })
    ));
    // The promotion is durable and stays durable: it is transcript history now,
    // and re-promoting it would duplicate the user's message.
    assert_eq!(transcript(&harness.inbox, &ses), vec!["msg_a".to_owned()]);
}

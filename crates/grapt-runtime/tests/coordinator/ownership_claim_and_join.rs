// ---------------------------------------------------------------------------
// A) An interrupt that lands before the run subscribes must not be lost
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupt_between_claiming_and_running_is_not_lost() {
    // The cut: ownership is claimed and the first eligibility pass is in flight,
    // so no run has subscribed to the cancellation channel yet. A `watch` send
    // with zero receivers discards the value, which silently lost the interrupt
    // while `interrupt()` still reported success.
    let ses = session("lostint");
    let gate = Gate::new();
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .with_inbox(
                &ses,
                Inbox {
                    steers: vec![steer("a")],
                    queued: Vec::new(),
                },
            )
            .with_gate(Hook::FirstEligible, gate.clone()),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    let drain = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };

    // Exact cut: claimed, nothing subscribed yet.
    gate.wait().await;
    assert!(coordinator.is_locally_draining(&ses));
    assert_eq!(
        coordinator.interrupt(&ses),
        Ok(InterruptOutcome::Interrupted)
    );
    gate.open();

    let outcome = match within("drain", drain).await {
        Ok(Ok(outcome)) => outcome,
        other => panic!("drain failed: {other:?}"),
    };

    // The promotion ran but observed the interrupt, and the drain stopped.
    match outcome {
        WakeOutcome::Drained { promotions, .. } => assert_eq!(promotions, 1),
        other => panic!("expected a drain, got {other:?}"),
    }
    assert_eq!(
        app.ran(),
        vec!["msg_a".to_owned()],
        "the interrupt must stop the drain after the in-flight promotion"
    );
    assert!(!coordinator.is_locally_draining(&ses));
}

// ---------------------------------------------------------------------------
// B) Joining must never hang, even when the owner finishes first
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn joining_never_hangs_even_when_the_owner_finishes_first() {
    // A `Notify` cloned under the lock and awaited after releasing it loses the
    // wakeup when the owner completes in the gap. Subscribing under the lock to
    // a state channel cannot: the joiner sees the terminal value even if it
    // registers late. Repeated so an unlucky interleaving is not simply missed.
    let ses = session("joinhang");
    for round in 0..50u32 {
        let app = Arc::new(TestApp::new(vec![ses.clone()]));
        let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

        let owner = {
            let coordinator = Arc::clone(&coordinator);
            let ses = ses.clone();
            tokio::spawn(async move { coordinator.wake(ses).await })
        };
        let joiner = {
            let coordinator = Arc::clone(&coordinator);
            let ses = ses.clone();
            tokio::spawn(async move { coordinator.wake(ses).await })
        };

        let owner = within(&format!("owner round {round}"), owner).await;
        let joiner = within(&format!("joiner round {round}"), joiner).await;
        assert!(matches!(owner, Ok(Ok(_))), "round {round}: {owner:?}");
        assert!(matches!(joiner, Ok(Ok(_))), "round {round}: {joiner:?}");
    }
}

// ---------------------------------------------------------------------------
// C) A wake at the final cut must not be lost
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wake_at_the_final_empty_cut_is_not_lost() {
    // The cut: the drain has just decided nothing is eligible. Previously it
    // released the lock here, and the cleanup that followed cleared
    // `wake_pending` — so a wake arriving in that gap was acknowledged as
    // Joined while its newly admitted work never ran.
    let ses = session("finalcut");
    let gate = Gate::new();
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .with_inbox(&ses, Inbox::default())
            .with_gate(Hook::FinalCut, gate.clone()),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    let owner = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };

    // Exact cut: inside the "nothing eligible" pass.
    gate.wait().await;
    // Newly admitted work plus the wake that announces it.
    app.add_steer(&ses, "late");
    let joiner = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    // Wait for the joiner to actually register its wake before releasing the
    // owner. Spinning on the real state is deterministic; releasing after a
    // sleep would only test whichever interleaving happened to occur.
    for _ in 0..100_000 {
        if coordinator.has_pending_wake(&ses) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        coordinator.has_pending_wake(&ses),
        "the joiner must have registered its wake before the owner is released"
    );

    // Let the owner leave the cut; it must notice the pending wake.
    gate.open();
    // The next final cut (after the late work runs) needs releasing too.
    gate.open();

    let owner = match within("owner", owner).await {
        Ok(Ok(outcome)) => outcome,
        other => panic!("owner failed: {other:?}"),
    };
    let joiner = within("joiner", joiner).await;
    assert!(matches!(joiner, Ok(Ok(_))), "joiner failed: {joiner:?}");

    assert_eq!(
        app.ran(),
        vec!["msg_late".to_owned()],
        "work admitted at the final cut must still run"
    );
    match owner {
        WakeOutcome::Drained { promotions, .. } => assert_eq!(promotions, 1),
        other => panic!("expected a drain, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// D) Abort recovery and shared terminal truth
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborting_the_owning_wake_releases_ownership() {
    // An aborted owner used to leave active=true forever, wedging the Session
    // for the life of the process.
    let ses = session("abort");
    let gate = Gate::new();
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .with_inbox(
                &ses,
                Inbox {
                    steers: vec![steer("a")],
                    queued: Vec::new(),
                },
            )
            .with_gate(Hook::Run, gate.clone()),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    let owner = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    gate.wait().await;
    assert!(coordinator.is_locally_draining(&ses));
    owner.abort();
    let _ = owner.await;

    // Ownership must be released even though the owner never finished.
    for _ in 0..1000 {
        if !coordinator.is_locally_draining(&ses) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        !coordinator.is_locally_draining(&ses),
        "an aborted owner must not wedge the Session"
    );

    // And a later wake works normally.
    gate.open();
    let again = within("later wake", coordinator.wake(ses.clone())).await;
    assert!(again.is_ok(), "later wake failed: {again:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_joiner_receives_the_owners_failure_rather_than_a_false_success() {
    // Placement fails for the owner. A joiner must NOT be told Joined.
    let ses = session("joinfail");
    let gate = Gate::new();
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .failing_placement(&ses)
            .with_gate(Hook::Run, gate.clone()),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    // Claim the slot with a drain that will fail at placement, but hold the
    // joiner's registration first by making the owner start and fail.
    let owner = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    let owner = within("owner", owner).await;
    assert!(
        matches!(owner, Ok(Err(CoordinatorError::Placement { .. }))),
        "owner should fail placement: {owner:?}"
    );

    // A later wake also fails, rather than reporting a hollow success.
    let after = within("after", coordinator.wake(ses.clone())).await;
    assert!(
        matches!(after, Err(CoordinatorError::Placement { .. })),
        "a failed placement must surface, got {after:?}"
    );
    let _ = gate;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inbox_failure_is_never_read_as_no_work() {
    let ses = session("inboxfail");
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .with_inbox(&ses, Inbox::default())
            .failing_inbox_after(0),
    );
    let coordinator = SessionCoordinator::new(Arc::clone(&app));

    let outcome = within("wake", coordinator.wake(ses.clone())).await;
    assert!(
        matches!(outcome, Err(CoordinatorError::Inbox { .. })),
        "an inbox failure must surface, not become an empty drain: {outcome:?}"
    );
    assert!(!coordinator.is_locally_draining(&ses));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_failure_is_never_read_as_success() {
    let ses = session("runfail");
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .with_inbox(
                &ses,
                Inbox {
                    steers: vec![steer("a")],
                    queued: Vec::new(),
                },
            )
            .failing_run(),
    );
    let coordinator = SessionCoordinator::new(Arc::clone(&app));

    let outcome = within("wake", coordinator.wake(ses.clone())).await;
    assert!(
        matches!(outcome, Err(CoordinatorError::Run { .. })),
        "a run failure must surface: {outcome:?}"
    );
    assert!(!coordinator.is_locally_draining(&ses));
}

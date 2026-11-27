// ---------------------------------------------------------------------------
// The original coordination laws, now with cut barriers
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_wake_joins_and_never_drains_concurrently() {
    let ses = session("join");
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
    // Exact cut: the owner is inside run().
    gate.wait().await;

    let joiner = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    // Spawning the joiner says nothing about when it runs. Released first, the
    // owner can finish before the joiner registers, and the joiner then claims
    // a drain of its own (final 20260929094054). Its registration is observed
    // on the coordinator's own state, taken under the same lock as its
    // subscription, while the owner is still gated; only then is it released.
    until_wake_registered(&coordinator, &ses).await;
    gate.open();
    gate.open();

    let owner = match within("owner", owner).await {
        Ok(Ok(outcome)) => outcome,
        other => panic!("owner failed: {other:?}"),
    };
    let joiner = match within("joiner", joiner).await {
        Ok(result) => result,
        Err(error) => panic!("joiner panicked: {error}"),
    };

    assert_eq!(
        joiner,
        Ok(WakeOutcome::Joined),
        "a wake registered against the gated owner did not join its drain"
    );
    match owner {
        WakeOutcome::Drained {
            placement_lookups, ..
        } => assert_eq!(placement_lookups, 1),
        other => panic!("expected a drain, got {other:?}"),
    }
    assert_eq!(
        app.peak_concurrent.load(Ordering::SeqCst),
        1,
        "never two concurrent runs for one Session"
    );
    assert_eq!(app.placement_lookups(&ses), 1);
}

/// Waits on the coordinator's own state until a second wake has registered
/// against the active drain. `has_pending_wake` is set only by a joiner, under
/// the same lock acquisition as its subscription, so observing it proves the
/// joiner is waiting on that drain rather than about to claim its own.
async fn until_wake_registered<A: SessionApplication>(
    coordinator: &SessionCoordinator<A>,
    session: &SessionId,
) {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while !coordinator.has_pending_wake(session) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the second wake never registered against the active drain within the deadline"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn different_sessions_drain_concurrently() {
    // Both Sessions must be inside run() at the same time. The gate makes that
    // an exact observation: each arrival adds a permit, so waiting for two
    // permits proves two concurrent runs without any sleeping.
    let one = session("one");
    let two = session("two");
    let gate = Gate::new();
    let app = Arc::new(
        TestApp::new(vec![one.clone(), two.clone()])
            .with_inbox(
                &one,
                Inbox {
                    steers: vec![steer("a")],
                    queued: Vec::new(),
                },
            )
            .with_inbox(
                &two,
                Inbox {
                    steers: vec![steer("b")],
                    queued: Vec::new(),
                },
            )
            .with_gate(Hook::Run, gate.clone()),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    let a = {
        let coordinator = Arc::clone(&coordinator);
        let one = one.clone();
        tokio::spawn(async move { coordinator.wake(one).await })
    };
    let b = {
        let coordinator = Arc::clone(&coordinator);
        let two = two.clone();
        tokio::spawn(async move { coordinator.wake(two).await })
    };

    // Two arrivals means two runs in flight simultaneously. If the slot lock
    // were held across the await, the second would never arrive and this waits
    // out the deadline instead of passing by luck.
    gate.wait().await;
    gate.wait().await;
    assert_eq!(
        app.peak_concurrent.load(Ordering::SeqCst),
        2,
        "different Sessions must run concurrently"
    );
    gate.open();
    gate.open();

    assert!(matches!(within("a", a).await, Ok(Ok(_))));
    assert!(matches!(within("b", b).await, Ok(Ok(_))));
    assert_eq!(app.placement_lookups(&one), 1);
    assert_eq!(app.placement_lookups(&two), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steers_promote_before_queued_work_at_an_idle_boundary() {
    let ses = session("boundaries");
    let app = Arc::new(TestApp::new(vec![ses.clone()]).with_inbox(
        &ses,
        Inbox {
            steers: vec![steer("s1"), steer("s2")],
            queued: vec![queued("q1"), queued("q2")],
        },
    ));
    let coordinator = SessionCoordinator::new(Arc::clone(&app));

    let outcome = match within("wake", coordinator.wake(ses.clone())).await {
        Ok(outcome) => outcome,
        Err(error) => panic!("wake failed: {error}"),
    };

    assert_eq!(
        app.ran(),
        vec!["msg_s1", "msg_s2", "msg_q1", "msg_q2"],
        "steers must promote before queued work"
    );
    let boundaries = app.boundaries_for(&ses);
    match boundaries
        .iter()
        .position(|boundary| *boundary == Boundary::Idle)
    {
        Some(index) => assert!(
            boundaries[..index]
                .iter()
                .all(|boundary| *boundary == Boundary::Steer),
            "Idle must not be asked before the steers are drained: {boundaries:?}"
        ),
        None => panic!("the queued inputs required an Idle boundary"),
    }
    match outcome {
        WakeOutcome::Drained {
            promotions,
            placement_lookups,
            ..
        } => {
            assert_eq!(promotions, 4);
            assert_eq!(placement_lookups, 1, "once per drain, not per promotion");
        }
        other => panic!("expected a drain, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_is_not_resolved_outside_a_drain() {
    let ses = session("placement");
    let app = Arc::new(TestApp::new(vec![ses.clone()]));
    let coordinator = SessionCoordinator::new(Arc::clone(&app));

    assert_eq!(coordinator.interrupt(&ses), Ok(InterruptOutcome::NoOpIdle));
    assert!(!coordinator.is_locally_draining(&ses));
    assert_eq!(app.placement_lookups(&ses), 0);

    assert!(within("wake", coordinator.wake(ses.clone())).await.is_ok());
    assert_eq!(app.placement_lookups(&ses), 1);
    // A later drain resolves it again: placement is re-read per drain, so a
    // durable move would take effect at the next one rather than being cached
    // for the life of the process.
    assert!(within("wake", coordinator.wake(ses.clone())).await.is_ok());
    assert_eq!(app.placement_lookups(&ses), 2);

    // And the resolution is CARRIED: the drain context owns the placement, so
    // nothing downstream has a reason to ask again.
    let drain = match app.begin_drain(&ses) {
        Ok(drain) => drain,
        Err(error) => panic!("begin_drain must resolve: {error}"),
    };
    assert_eq!(drain.placement.location, location("loc_main"));
    assert_eq!(app.placement_lookups(&ses), 3);
    assert!(
        !app.continuation_owed(&drain),
        "a drain that has promoted nothing owes no Step"
    );
}

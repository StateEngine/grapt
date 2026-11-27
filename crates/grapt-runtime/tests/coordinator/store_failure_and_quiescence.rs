// ---------------------------------------------------------------------------
// Round-2: the remaining races
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_store_failure_is_never_reported_as_an_unknown_session() {
    // A bool existence check collapses a store read failure into "no such
    // Session", telling a caller their work vanished because a disk hiccuped.
    let ses = session("storefail");
    let app = Arc::new(TestApp::new(vec![ses.clone()]).failing_exists());
    let coordinator = SessionCoordinator::new(app);

    let woke = within("wake", coordinator.wake(ses.clone())).await;
    assert!(
        matches!(woke, Err(CoordinatorError::Inbox { .. })),
        "a store failure must surface as a store error: {woke:?}"
    );
    assert!(
        !matches!(woke, Err(CoordinatorError::UnknownSession { .. })),
        "never UnknownSession"
    );
    assert!(matches!(
        coordinator.interrupt(&ses),
        Err(CoordinatorError::Inbox { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_joiner_gets_its_own_drains_result_even_if_a_later_drain_starts() {
    // The completion channel must be per drain. Reusing one lets a later claim
    // publish Active over drain-1's terminal value before drain-1's joiner has
    // polled, so the joiner would miss its own drain's truth and observe
    // drain-2 instead.
    let ses = session("gen");
    let gate = Gate::new();
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .with_inbox(
                &ses,
                Inbox {
                    steers: vec![steer("one")],
                    queued: Vec::new(),
                },
            )
            .with_gate(Hook::Run, gate.clone()),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    // Drain 1 claims and parks inside run().
    let owner1 = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    gate.wait().await;

    // A joiner registers against drain 1.
    let joiner = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    for _ in 0..100_000 {
        if coordinator.has_pending_wake(&ses) {
            break;
        }
        tokio::task::yield_now().await;
    }

    // Let drain 1 finish. Its coalesced pass finds nothing, so it releases.
    gate.open();
    let owner1 = within("owner1", owner1).await;
    assert!(matches!(owner1, Ok(Ok(_))), "owner1: {owner1:?}");

    // The joiner must complete with drain 1's result, regardless of what any
    // later drain does. It cannot be left waiting on a newer generation.
    let joiner = within("joiner", joiner).await;
    assert!(
        matches!(joiner, Ok(Ok(WakeOutcome::Joined))),
        "the joiner must receive its own drain's result: {joiner:?}"
    );

    // And a later drain still works normally.
    app.add_steer(&ses, "two");
    gate.open();
    let later = within("later", coordinator.wake(ses.clone())).await;
    assert!(later.is_ok(), "later drain failed: {later:?}");
    assert_eq!(app.ran(), vec!["msg_one".to_owned(), "msg_two".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_admitted_work_is_lost_when_wakes_race_the_quiescent_decision() {
    // The quiescent decision, clearing ownership and publishing the terminal
    // result are one lock transition. There is no gap to gate a hook into, so
    // this hammers the boundary instead: every round admits work and wakes
    // without any synchronisation, and every admitted input must eventually
    // run. A split transition drops some.
    let ses = session("noloss");
    let app = Arc::new(TestApp::new(vec![ses.clone()]).with_inbox(&ses, Inbox::default()));
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    const ROUNDS: usize = 60;
    let mut handles = Vec::new();
    for round in 0..ROUNDS {
        app.add_steer(&ses, &format!("r{round}"));
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        handles.push(tokio::spawn(async move { coordinator.wake(ses).await }));
    }
    for (index, handle) in handles.into_iter().enumerate() {
        let result = within(&format!("wake {index}"), handle).await;
        assert!(matches!(result, Ok(Ok(_))), "wake {index}: {result:?}");
    }

    // Drain anything still admitted, then account for every input.
    let _ = within("final", coordinator.wake(ses.clone())).await;
    let ran = app.ran();
    assert_eq!(
        ran.len(),
        ROUNDS,
        "every admitted input must run exactly once; ran {} of {ROUNDS}: {ran:?}",
        ran.len()
    );
    assert!(!coordinator.is_locally_draining(&ses));
}

// A caller that prepared before owning a drain hands its cancellation over
// at the claim, and a claimed drain still waiting for capacity can be
// interrupted: an interrupt the caller acknowledged is never lost between
// the two owners.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupt_handed_over_at_the_claim_stops_the_drain_at_its_first_step() {
    // The interrupt reached the caller after its preparation ended and
    // before it owned the drain: the claim must carry it.
    let ses = session("handoff");
    let app = Arc::new(TestApp::new(vec![ses.clone()]).with_inbox(
        &ses,
        Inbox {
            steers: vec![steer("a")],
            queued: vec![queued("q")],
        },
    ));
    let coordinator = SessionCoordinator::new(Arc::clone(&app));
    let claims = AtomicUsize::new(0);

    let outcome = within(
        "handed-over wake",
        coordinator.wake_with(ses.clone(), || {
            claims.fetch_add(1, Ordering::SeqCst);
            Some(grapt_types::InterruptionSource::User)
        }),
    )
    .await;

    assert!(
        matches!(outcome, Ok(WakeOutcome::Drained { steps: 1, .. })),
        "{outcome:?}"
    );
    assert_eq!(claims.load(Ordering::SeqCst), 1, "claimed exactly once");
    assert_eq!(
        app.ran(),
        vec!["msg_a".to_owned()],
        "the interrupted input settles; the queued one never runs"
    );
    assert_eq!(
        app.stepped(),
        1,
        "one Step, ended at its cancellation check"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claimed_drain_waiting_for_capacity_is_interrupted_not_ignored() {
    let (holder, waiter) = (session("holder"), session("waiter"));
    let gate = Gate::new();
    let app = Arc::new(
        TestApp::new(vec![holder.clone(), waiter.clone()])
            .with_inbox(
                &holder,
                Inbox {
                    steers: vec![steer("h")],
                    queued: Vec::new(),
                },
            )
            .with_inbox(
                &waiter,
                Inbox {
                    steers: vec![steer("w")],
                    queued: vec![queued("q")],
                },
            )
            .with_gate(Hook::Run, gate.clone()),
    );
    let one = std::num::NonZeroUsize::new(1).unwrap_or_else(|| panic!("one is non-zero"));
    let coordinator = Arc::new(SessionCoordinator::with_capacity(Arc::clone(&app), one));

    let held = {
        let coordinator = Arc::clone(&coordinator);
        let holder = holder.clone();
        tokio::spawn(async move { coordinator.wake(holder).await })
    };
    gate.wait().await;
    // The waiter claims its slot, then waits for the one permit.
    let claimed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waiting = {
        let coordinator = Arc::clone(&coordinator);
        let waiter = waiter.clone();
        let claimed = Arc::clone(&claimed);
        tokio::spawn(async move {
            coordinator
                .wake_with(waiter, move || {
                    claimed.store(true, Ordering::SeqCst);
                    None
                })
                .await
        })
    };
    within("the waiter's claim", async {
        while !claimed.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(!coordinator.is_locally_draining(&waiter), "still waiting");

    assert_eq!(
        coordinator.interrupt(&waiter),
        Ok(InterruptOutcome::Interrupted),
        "an acknowledged turn waiting for capacity must be stoppable"
    );

    gate.open();
    assert!(matches!(within("holder", held).await, Ok(Ok(_))));
    gate.wait().await;
    gate.open();
    let outcome = within("waiter", waiting).await;
    assert!(
        matches!(outcome, Ok(Ok(WakeOutcome::Drained { steps: 1, .. }))),
        "{outcome:?}"
    );
    assert!(
        !app.ran().contains(&"msg_q".to_owned()),
        "the interrupted drain went on to the queued input: {:?}",
        app.ran()
    );
}

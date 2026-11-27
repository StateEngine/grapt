#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_is_a_no_op_when_idle_and_a_rejection_when_unknown() {
    let known = session("known");
    let unknown = session("ghost");
    let app = Arc::new(TestApp::new(vec![known.clone()]));
    let coordinator = SessionCoordinator::new(app);

    assert_eq!(
        coordinator.interrupt(&known),
        Ok(InterruptOutcome::NoOpIdle)
    );
    assert_eq!(
        coordinator.interrupt(&unknown),
        Err(CoordinatorError::UnknownSession {
            session: unknown.as_str().to_owned()
        })
    );
    assert!(matches!(
        within("wake", coordinator.wake(unknown.clone())).await,
        Err(CoordinatorError::UnknownSession { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupt_ends_the_drain_without_retrying_the_rest() {
    // Both steers belong to the SAME boundary, so both promote before the Step
    // the interrupt lands in — that is the batch, and it is what the user
    // typed. The queued input is a different question: it is offered only at an
    // otherwise-idle boundary, which the interrupt means never arrives, so it
    // stays pending.
    let ses = session("interrupt");
    let gate = Gate::new();
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .with_inbox(
                &ses,
                Inbox {
                    steers: vec![steer("a"), steer("b")],
                    queued: vec![queued("q")],
                },
            )
            .with_gate(Hook::Run, gate.clone()),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    let drain = {
        let coordinator = Arc::clone(&coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };
    gate.wait().await;
    assert_eq!(
        coordinator.interrupt(&ses),
        Ok(InterruptOutcome::Interrupted)
    );
    gate.open();

    let outcome = match within("drain", drain).await {
        Ok(Ok(outcome)) => outcome,
        other => panic!("drain failed: {other:?}"),
    };
    match outcome {
        WakeOutcome::Drained {
            promotions, steps, ..
        } => {
            assert_eq!(promotions, 2, "the steer batch, and only the steer batch");
            assert_eq!(steps, 1, "the interrupted Step, and no Step after it");
        }
        other => panic!("expected a drain, got {other:?}"),
    }
    assert_eq!(
        app.ran(),
        vec!["msg_a".to_owned(), "msg_b".to_owned()],
        "the batch promoted; the queued input did not"
    );
    assert_eq!(
        app.stepped(),
        1,
        "no retry and no further Step after an interrupt"
    );
    assert!(!coordinator.is_locally_draining(&ses));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_steer_promotes_at_the_next_step_boundary_and_queued_work_waits_for_idle() {
    // The blocker's semaphore witness, at the coordinator's altitude: a chain
    // owing continuation must come back to a boundary between Steps, promote
    // the steer waiting there, and STILL refuse the queued input until the
    // chain truly idles.
    let ses = session("boundary");
    let app = Arc::new(
        TestApp::new(vec![ses.clone()])
            .with_inbox(
                &ses,
                Inbox {
                    steers: vec![steer("a")],
                    queued: vec![queued("q")],
                },
            )
            // Three Steps per promotion: two real boundaries inside the chain.
            .with_steps_per_promotion(3),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));

    // A steer admitted while the chain is mid-flight. Adding it before the wake
    // would make it part of the first batch and prove nothing about boundaries;
    // the drain below reaches its first boundary with this already pending.
    app.add_steer(&ses, "mid");

    let outcome = within("wake", coordinator.wake(ses.clone())).await;
    match outcome {
        Ok(WakeOutcome::Drained {
            promotions, steps, ..
        }) => {
            assert_eq!(
                promotions, 3,
                "steer, mid-chain steer, then the queued input"
            );
            assert!(steps >= 4, "each promotion's chain took its Steps: {steps}");
        }
        other => panic!("expected a drain, got {other:?}"),
    }

    assert_eq!(
        app.ran(),
        vec!["msg_a".to_owned(), "msg_mid".to_owned(), "msg_q".to_owned()],
        "the mid-chain steer promoted BEFORE the queued input, not after it"
    );

    // The queued input was offered only at an idle boundary: every Idle pass
    // before the chain finished must have found nothing to promote it into.
    let boundaries = app.boundaries_for(&ses);
    let first_idle = boundaries
        .iter()
        .position(|boundary| *boundary == Boundary::Idle);
    assert!(
        first_idle.is_some(),
        "the drain must reach an idle boundary: {boundaries:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_drain_promotes_nothing() {
    let ses = session("empty");
    let app = Arc::new(TestApp::new(vec![ses.clone()]));
    let coordinator = SessionCoordinator::new(Arc::clone(&app));

    match within("wake", coordinator.wake(ses.clone())).await {
        Ok(WakeOutcome::Drained {
            promotions,
            placement_lookups,
            ..
        }) => {
            assert_eq!(promotions, 0);
            assert_eq!(placement_lookups, 1);
        }
        other => panic!("expected an empty drain, got {other:?}"),
    }
    assert!(app.ran().is_empty());
    assert!(!coordinator.is_locally_draining(&ses));
}
